//! Bounded one-way copy: agent membership authority lives with agent configuration.
use super::*;
impl Store {
    pub(crate) async fn import_agent_memberships(&self)->Result<()> {
        sqlx::query("CREATE TABLE IF NOT EXISTS agent_memberships(user_id TEXT NOT NULL,agent_id TEXT NOT NULL,value BLOB NOT NULL,PRIMARY KEY(user_id,agent_id))")
            .execute(&self.agent_pool).await?;
        let marker=format!("agent:{}:membership_storage_v1",self.agent_id);
        let done:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(&marker).fetch_optional(&self.agent_pool).await?;
        if done.is_some() {return Ok(());}
        let mut tx=self.agent_pool.begin().await?;
        sqlx::query("UPDATE agents SET revision=revision WHERE id=?").bind(&self.agent_id).execute(&mut *tx).await?;
        let migrated:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(&marker).fetch_optional(&mut *tx).await?;
        if migrated.is_some() {tx.commit().await?;return Ok(());}
        let exists:Option<String>=sqlx::query_scalar("SELECT id FROM agent_directory WHERE id=?").bind(&self.agent_id).fetch_optional(&self.app_pool).await?;
        if exists.is_none() {tx.commit().await?;return Ok(());}
        let rows=sqlx::query("SELECT user_id,value FROM agent_memberships WHERE agent_id=? ORDER BY user_id").bind(&self.agent_id).fetch_all(&self.app_pool).await?;
        for row in rows {
            let user:String=row.get("user_id");let value=self.unseal(&row.get::<Vec<u8>,_>("value"))?;
            if value["user_id"]!=user || value["agent_id"]!=self.agent_id {return Err(anyhow!("Invalid legacy membership identity"));}
            sqlx::query("INSERT INTO agent_memberships(user_id,agent_id,value) VALUES(?,?,?)")
                .bind(user).bind(&self.agent_id).bind(self.seal_agent(&value)?).execute(&mut *tx).await?;
        }
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)").bind(marker).bind(self.seal_agent(&json!({"version":1}))?).execute(&mut *tx).await?;
        tx.commit().await?;Ok(())
    }
}

impl Store {
    pub(crate) async fn record_agent_visit(&self,user:&str,agent:&str)->Result<()> {
        let scoped=self.for_agent(agent).await?;
        let Some(value)=scoped.access_repository().resolved_membership(user,agent).await? else{return Ok(())};
        if value["origin"]!="app_login"&&value["origin"]!="visitor" {return Ok(());}
        sqlx::query("INSERT OR IGNORE INTO agent_memberships(user_id,agent_id,value) VALUES(?,?,?)")
            .bind(user).bind(agent).bind(scoped.seal_agent(&value)?).execute(&scoped.agent_pool).await?;
        Ok(())
    }
}

#[cfg(test)]
mod security_tests {
    use super::*;
    #[tokio::test]
    async fn app_admin_has_no_agent_authority_and_explicit_appointments_override_login() {
        let root=tempfile::tempdir().unwrap();let store=Store::open(root.path()).await.unwrap();
        store.create_admin_account("owner","fixture-hash","fixture-generation").await.unwrap();
        for (id,role,tier) in [("outsider","administrator","operator"),("alice","user","basic")] {
            let account=json!({"user_id":id,"username":id,"role":role,"tier_id":tier,"status":"active","access_revision":1,"generation":"fixture","is_approved":true});
            sqlx::query("INSERT INTO user_accounts(user_id,username,value) VALUES(?,?,?)")
                .bind(id).bind(id).bind(store.seal(&account).unwrap()).execute(&store.app_pool).await.unwrap();
        }
        assert_eq!(store.agent_authority("outsider","shared_default").await.unwrap()["can_manage"],false);
        let repo=store.access_repository();
        assert!(repo.manage_agent_member("outsider","alice","shared_default",Some("operator"),false,0).await.is_err());
        assert!(repo.manage_agent_member("admin","admin","shared_default",None,true,1).await.is_err());
        let raw:Vec<u8>=sqlx::query_scalar("SELECT metadata FROM agents WHERE id='shared_default'").fetch_one(&store.agent_pool).await.unwrap();
        let mut metadata=store.unseal_agent(&raw).unwrap();metadata["member_access"]=json!({"app_login_members":true,"visitor_access":true,"conversation_privacy":"private"});
        sqlx::query("UPDATE agents SET metadata=? WHERE id='shared_default'").bind(store.seal_agent(&metadata).unwrap()).execute(&store.agent_pool).await.unwrap();
        assert_eq!(repo.resolved_membership("alice","shared_default").await.unwrap().unwrap()["capability_level_id"],"member");
        repo.manage_agent_member("admin","alice","shared_default",Some("visitor"),false,0).await.unwrap();
        assert_eq!(repo.resolved_membership("alice","shared_default").await.unwrap().unwrap()["capability_level_id"],"visitor");
        repo.manage_agent_member("admin","alice","shared_default",None,true,1).await.unwrap();
        assert_eq!(repo.resolved_membership("alice","shared_default").await.unwrap().unwrap()["relationship"],"revoked");
        repo.manage_agent_member("admin","alice","shared_default",None,false,2).await.unwrap();
        assert_eq!(repo.resolved_membership("alice","shared_default").await.unwrap().unwrap()["capability_level_id"],"member");
        repo.manage_agent_member("admin","alice","shared_default",Some("operator"),false,3).await.unwrap();
        assert_eq!(store.agent_authority("alice","shared_default").await.unwrap()["can_manage"],true);
        repo.manage_agent_member("alice","outsider","shared_default",Some("operator"),false,0).await.unwrap();
        assert_eq!(store.agent_authority("outsider","shared_default").await.unwrap()["can_manage"],true);
        repo.manage_agent_member("outsider","alice","shared_default",Some("member"),false,4).await.unwrap();
        assert_eq!(store.agent_authority("alice","shared_default").await.unwrap()["can_manage"],false);
        drop(store);let reopened=Store::open(root.path()).await.unwrap();
        assert_eq!(reopened.agent_authority("outsider","shared_default").await.unwrap()["can_manage"],true);
    }
}
