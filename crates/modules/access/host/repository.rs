//! Application-wide records for who people are, what their tier permits, and
//! which agents they may use. Conversations themselves belong to user storage.
use crate::access::{self, AccountStatus, Principal, Relationship, Role, Tier};
use crate::store::Store;
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use sqlx::Row;

fn account_view(value: &Value) -> Result<Value> {
    let principal = Principal::from_account(value)?;
    Ok(json!({"user_id":principal.user_id,"username":value["username"],
        "status":principal.status,"role":principal.role,
        "tier_id":principal.tier_id,"access_revision":principal.access_revision}))
}
fn username_ok(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128 && name.trim() == name
        && !name.chars().any(char::is_control)
}
async fn audit(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, store: &Store,
    kind: &str, payload: &Value) -> Result<()> {
    sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES('',?,?)")
        .bind(kind).bind(store.seal(payload)?).execute(&mut **tx).await?;
    Ok(())
}

impl Store {
    pub async fn access_audit(&self, limit: u32) -> Result<Vec<Value>> { self.access_repository().access_audit(limit).await }
    pub async fn all_user_ids(&self) -> Result<Vec<String>> {
        let rows = sqlx::query("SELECT user_id FROM user_accounts ORDER BY user_id")
            .fetch_all(&self.app_pool).await?;
        Ok(rows.into_iter().map(|row| row.get::<String,_>("user_id")).collect())
    }
    pub async fn access_accounts(&self) -> Result<Vec<Value>> {
        let rows = sqlx::query("SELECT value FROM user_accounts ORDER BY username LIMIT 1000")
            .fetch_all(&self.app_pool).await?;
        let mut accounts=Vec::new();
        for row in rows {
            let mut account=account_view(&self.unseal(&row.get::<Vec<u8>,_>("value"))?)?;
            let id=account["user_id"].as_str().unwrap();
            let profile=self.user_profile(id).await?.unwrap_or_else(||json!({}));
            let view=self.for_user(id).await?;
            let sessions:i64=sqlx::query_scalar("SELECT COUNT(*) FROM docs WHERE key LIKE 'session:%'").fetch_one(&view.pool).await?;
            let messages:i64=sqlx::query_scalar("SELECT COUNT(*) FROM chat").fetch_one(&view.pool).await?;
            account["profile"]=profile;account["session_count"]=json!(sessions);account["message_count"]=json!(messages);
            accounts.push(account);
        }
        Ok(accounts)
    }
    /// Guests are durable independent identities, never password accounts.
    pub async fn create_guest_account(&self) -> Result<Value> {
        let id = format!("guest_{}", uuid::Uuid::new_v4().simple());
        let account = json!({"user_id":id,"username":id,"password_hash":null,
            "generation":uuid::Uuid::new_v4().to_string(),"is_approved":true,
            "status":AccountStatus::Active,"role":Role::Guest,"tier_id":"anonymous",
            "access_revision":1});
        let profile = json!({"user_id":id,"display_name":"Guest","is_admin":false,
            "default_agent_id":null,"last_login_at":null,"source_channel":"web_public"});
        let mut tx = self.app_pool.begin().await?;
        sqlx::query("INSERT INTO user_accounts(user_id,username,value) VALUES(?,?,?)")
            .bind(&id).bind(&id).bind(self.seal(&account)?).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO user_profiles(user_id,value) VALUES(?,?)")
            .bind(&id).bind(self.seal(&profile)?).execute(&mut *tx).await?;
        audit(&mut tx,self,"account_created",&json!({"user_id":id,"role":"guest"})).await?;
        tx.commit().await?;
        Ok(account)
    }
    pub async fn create_user_account(&self, username: &str, password_hash: &str) -> Result<Value> {
        if !username_ok(username) || password_hash.len() < 20 || password_hash.len() > 1024 {
            bail!("Invalid account input");
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let account = json!({"user_id":id,"username":username,"password_hash":password_hash,
            "generation":uuid::Uuid::new_v4().to_string(),"is_approved":false,
            "status":AccountStatus::Pending,"role":Role::User,"tier_id":"basic",
            "access_revision":1});
        let profile = json!({"user_id":id,"display_name":username,"is_admin":false,
            "default_agent_id":null,"last_login_at":null});
        let mut tx = self.app_pool.begin().await?;
        sqlx::query("INSERT INTO user_accounts(user_id,username,value) VALUES(?,?,?)")
            .bind(&id).bind(username).bind(self.seal(&account)?)
            .execute(&mut *tx).await?;
        sqlx::query("INSERT INTO user_profiles(user_id,value) VALUES(?,?)")
            .bind(&id).bind(self.seal(&profile)?).execute(&mut *tx).await?;
        audit(&mut tx,self,"account_created",&json!({"user_id":id,"status":"pending"})).await?;
        tx.commit().await?;
        account_view(&account)
    }
    pub async fn update_account_access(&self, id: &str, expected_revision: u64,
        status: AccountStatus, role: Role, tier_id: &str) -> Result<Value> { self.access_repository().update_account_access(id, expected_revision, status, role, tier_id).await }
    pub async fn entitlement_tier(&self, id: &str) -> Result<Option<Tier>> { self.access_repository().entitlement_tier(id).await }
    pub async fn entitlement_tiers(&self) -> Result<Vec<Tier>> { self.access_repository().entitlement_tiers().await }
    pub async fn save_entitlement_tier(&self, tier: Tier,
        expected_revision: u64) -> Result<Tier> { self.access_repository().save_entitlement_tier(tier, expected_revision).await }
    pub async fn agent_membership(&self, user_id: &str, agent_id: &str) -> Result<Option<Value>> { let view=self.for_agent(agent_id).await?; view.access_repository().agent_membership(user_id, agent_id).await }
    /// Directory ownership is immutable. Agent administration is a separate
    /// membership capability and does not grant ability/tool permissions.
    pub async fn agent_authority(&self, user_id: &str, agent_id: &str) -> Result<Value> { let view=self.for_agent(agent_id).await?; view.access_repository().agent_authority(user_id, agent_id).await }
    /// Caller must authorize roster access before returning these bounded records.
    pub async fn agent_memberships(&self, agent_id: &str) -> Result<Vec<Value>> { let view=self.for_agent(agent_id).await?; view.access_repository().agent_memberships(agent_id).await }
    // TENANT-ASSIGNMENT: existing accounts receive revisioned grants; no invitation identity is minted.
    pub async fn assign_agent_membership(&self, actor_id: &str, user_id: &str,
        agent_id: &str, relationship: Relationship, agent_administrator: bool,
        expected_revision: u64) -> Result<Value> { crate::access::repository::validate_membership_actor(actor_id)?; crate::access::repository::validate_membership_update(user_id,agent_id,relationship)?;  let view=self.for_agent(agent_id).await?; view.access_repository().assign_agent_membership(actor_id, user_id, agent_id, relationship, agent_administrator, expected_revision).await }
    pub async fn set_agent_membership(&self, user_id: &str, agent_id: &str,
        relationship: Relationship, expected_revision: u64) -> Result<Value> { crate::access::repository::validate_membership_update(user_id,agent_id,relationship)?;  let view=self.for_agent(agent_id).await?; view.access_repository().set_agent_membership(user_id, agent_id, relationship, expected_revision).await }
    // MEMBERSHIP-TRANSACTION: authorize and rotate account access in the same app-plane write.

}

#[cfg(test)]
mod tests {
    use super::*;
    // TENANT-FIXTURE: isolated temporary agent files exercise real storage ownership.
    async fn tenant_fixture(store: &Store, id: &str, owner: &str) {
        let pool = crate::agent_storage::open_agent_pool(&store.root,id).await.unwrap();
        sqlx::query("INSERT INTO agents(id,name,description,provider,model,status,metadata,revision,created_at,updated_at) VALUES(?,?,'','openai_subscription','','active',?,1,1,1)")
            .bind(id).bind(id).bind(store.seal(&json!({"owner_id":owner})).unwrap())
            .execute(&pool).await.unwrap();
        pool.close().await;
        store.register_agent(id,owner).await.unwrap();
        store.set_agent_membership(owner,id,Relationship::Owner,0).await.unwrap();
    }
    async fn active_user(store: &Store, name: &str) -> String {
        let value = store.create_user_account(name,"long-hash-placeholder").await.unwrap();
        let id = value["user_id"].as_str().unwrap();
        store.update_account_access(id,1,AccountStatus::Active,Role::User,"basic").await.unwrap();
        id.to_owned()
    }
    // TENANT-BOUNDARY-TEST: separate owners and delegated membership authority never merge.
    #[tokio::test]
    async fn tenant_owner_and_delegated_roster_authority_are_separate() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store.create_admin_account("root","long-hash-placeholder","generation").await.unwrap();
        let owner = active_user(&store,"owner").await;
        let other_owner = active_user(&store,"other-owner").await;
        let manager = active_user(&store,"manager").await;
        let member = active_user(&store,"member").await;
        tenant_fixture(&store,"tenant-one",&owner).await;
        tenant_fixture(&store,"tenant-two",&other_owner).await;
        let first = store.for_agent("tenant-one").await.unwrap();
        let second = store.for_agent("tenant-two").await.unwrap();
        first.save_agent_model(&json!({"provider":"openai_subscription","model":"first-model"})).await.unwrap();
        assert_ne!(second.agent_model().await.unwrap()["model"],"first-model");
        assert_eq!(store.agent_authority("admin","tenant-one").await.unwrap()["owner_id"],owner);
        assert_eq!(store.agent_authority("admin","tenant-one").await.unwrap()["can_manage"],false);
        assert_eq!(store.agent_authority("admin","tenant-one").await.unwrap()["can_use"],false);
        assert!(store.assign_agent_membership("admin",&manager,"tenant-one",Relationship::Member,true,0).await.is_err());
        let appointed = store.assign_agent_membership(&owner,&manager,"tenant-one",Relationship::Member,true,0).await.unwrap();
        assert_eq!(appointed["agent_administrator"],true);
        assert_eq!(store.agent_authority(&manager,"tenant-one").await.unwrap()["can_manage"],true);
        assert_eq!(store.agent_authority(&manager,"tenant-two").await.unwrap()["can_manage"],false);
        assert!(store.assign_agent_membership(&manager,&member,"tenant-two",Relationship::Member,false,0).await.is_err());
        let visitor = store.assign_agent_membership(&manager,&member,"tenant-one",Relationship::Caller,false,0).await.unwrap();
        assert_eq!(visitor["relationship"],"caller");
        assert!(store.assign_agent_membership(&manager,&member,"tenant-one",Relationship::Member,false,0).await.is_err());
        assert!(store.assign_agent_membership(&manager,&member,"tenant-one",Relationship::Member,true,1).await.is_ok());
        store.assign_agent_membership(&manager,&member,"tenant-one",Relationship::Member,false,2).await.unwrap();
        assert!(store.assign_agent_membership(&member,&manager,"tenant-one",Relationship::Member,false,1).await.is_err());
        assert!(store.assign_agent_membership(&member,&manager,"tenant-one",Relationship::Revoked,false,1).await.is_err());
        assert!(store.assign_agent_membership(&owner,&owner,"tenant-one",Relationship::Member,false,1).await.is_err());
        assert!(store.set_agent_membership(&member,"tenant-one",Relationship::Owner,2).await.is_err());
        assert!(store.assign_agent_membership(&owner,"missing_account","tenant-one",Relationship::Member,false,0).await.is_err());
        let revoked = store.assign_agent_membership(&owner,&manager,"tenant-one",Relationship::Revoked,false,1).await.unwrap();
        assert_eq!(revoked["agent_administrator"],false);
        assert_eq!(store.agent_authority(&manager,"tenant-one").await.unwrap()["can_manage"],false);
        assert!(store.assign_agent_membership(&manager,&member,"tenant-one",Relationship::Caller,false,2).await.is_err());
        let members = store.agent_memberships("tenant-one").await.unwrap();
        assert_eq!(members.len(),3);
        assert!(members.iter().all(|value|value.get("password_hash").is_none()));
        assert!(store.agent_authority(&owner,"unknown").await.is_err());
        // Existing owner/member/caller tool policy relationships remain unchanged.
        assert_eq!(store.live_access(&member,"tenant-one").await.unwrap().relationship,Relationship::Member);
    }
    // TENANT-REVOCATION-TEST: reopen and account suspension must preserve live denials.
    #[tokio::test]
    async fn tenant_administration_revocation_persists_and_inactive_owner_cannot_write() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store.create_admin_account("root","long-hash-placeholder","generation").await.unwrap();
        let owner = active_user(&store,"owner").await;
        let manager = active_user(&store,"manager").await;
        tenant_fixture(&store,"tenant-one",&owner).await;
        store.assign_agent_membership(&owner,&manager,"tenant-one",Relationship::Member,true,0).await.unwrap();
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(reopened.agent_authority(&manager,"tenant-one").await.unwrap()["agent_administrator"],true);
        let before = store.account_by_id(&manager).await.unwrap().unwrap();
        store.assign_agent_membership(&owner,&manager,"tenant-one",Relationship::Member,false,1).await.unwrap();
        let after = store.account_by_id(&manager).await.unwrap().unwrap();
        assert_eq!(before["generation"],after["generation"]);
        assert_eq!(reopened.agent_authority(&manager,"tenant-one").await.unwrap()["can_manage"],false);
        let revision = store.account_by_id(&owner).await.unwrap().unwrap()["access_revision"].as_u64().unwrap();
        store.update_account_access(&owner,revision,AccountStatus::Suspended,Role::User,"basic").await.unwrap();
        assert!(store.assign_agent_membership(&owner,&manager,"tenant-one",Relationship::Member,true,2).await.is_err());
        assert_eq!(store.agent_authority(&owner,"tenant-one").await.unwrap()["can_manage"],false);
    }
    #[tokio::test]
    async fn new_user_is_pending_and_access_revisions_revoke_credentials() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store.create_admin_account("root","long-hash-placeholder","generation")
            .await.unwrap();
        let created = store.create_user_account("second","long-hash-placeholder").await.unwrap();
        let id = created["user_id"].as_str().unwrap();
        assert_eq!(created["status"], "pending");
        assert_eq!(created["tier_id"], "basic");
        let before = store.account_by_id(id).await.unwrap().unwrap();
        let active = store.update_account_access(id,1,AccountStatus::Active,
            Role::User,"basic").await.unwrap();
        assert_eq!(active["access_revision"],2);
        let after = store.account_by_id(id).await.unwrap().unwrap();
        assert_ne!(before["generation"],after["generation"]);
        assert!(store.update_account_access(id,1,AccountStatus::Suspended,
            Role::User,"basic").await.is_err());
        assert!(store.update_account_access("admin",1,AccountStatus::Disabled,
            Role::User,"basic").await.is_err());
        store.app_pool.close().await;
        store.pool.close().await;
        store.agent_pool.close().await;
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(reopened.account_by_id(id).await.unwrap().unwrap()["status"],"active");
    }
    #[tokio::test]
    async fn user_planes_are_isolated_and_admin_path_is_stable() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store.create_admin_account("root","long-hash-placeholder","generation")
            .await.unwrap();
        let first = store.create_user_account("first","long-hash-placeholder").await.unwrap();
        let second = store.create_user_account("second","long-hash-placeholder").await.unwrap();
        let first_id = first["user_id"].as_str().unwrap();
        let second_id = second["user_id"].as_str().unwrap();
        let one = store.for_user(first_id).await.unwrap();
        let two = store.for_user(second_id).await.unwrap();
        one.put("session:shared", &json!({"owner":first_id})).await.unwrap();
        two.put("session:shared", &json!({"owner":second_id})).await.unwrap();
        one.put("receipt:session:same-request", &json!({"owner":first_id})).await.unwrap();
        two.put("receipt:session:same-request", &json!({"owner":second_id})).await.unwrap();
        assert_eq!(one.get("receipt:session:same-request").await.unwrap().unwrap()["owner"],first_id);
        assert_eq!(two.get("receipt:session:same-request").await.unwrap().unwrap()["owner"],second_id);
        assert_eq!(one.get("session:shared").await.unwrap().unwrap()["owner"],first_id);
        assert_eq!(two.get("session:shared").await.unwrap().unwrap()["owner"],second_id);
        assert!(store.get("session:shared").await.unwrap().is_none());
        assert_eq!(store.current_user_id(),"admin");
        assert!(root.path().join("user_data/admin/admin.db").exists());
    }
    #[tokio::test]
    async fn tier_and_membership_changes_bump_access_revision() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store.create_admin_account("root","long-hash-placeholder","generation")
            .await.unwrap();
        let created = store.create_user_account("second","long-hash-placeholder").await.unwrap();
        let id = created["user_id"].as_str().unwrap();
        let tier = store.save_entitlement_tier(Tier{id:"team".into(),revision:0,model_policy:None,ability_permissions:Default::default(),
            allowed_models:vec!["gpt-6-luna".into()],tools:Default::default(),
            max_sessions:4,max_active_runs:1},0).await.unwrap();
        assert_eq!(tier.revision,1);
        store.update_account_access(id,1,AccountStatus::Active,Role::User,"team")
            .await.unwrap();
        let membership = store.set_agent_membership(id,"shared_default",
            Relationship::Caller,0).await.unwrap();
        assert_eq!(membership["revision"],1);
        let revision = store.account_by_id(id).await.unwrap().unwrap()["access_revision"].as_u64().unwrap();
        assert_eq!(revision,2);
        store.save_entitlement_tier(Tier{max_sessions:2,..tier},1).await.unwrap();
        let after = store.account_by_id(id).await.unwrap().unwrap()["access_revision"].as_u64().unwrap();
        assert_eq!(after,revision+1);
        assert_eq!(store.agent_membership(id,"shared_default").await.unwrap().unwrap()
            ["relationship"],"caller");
    }
}
