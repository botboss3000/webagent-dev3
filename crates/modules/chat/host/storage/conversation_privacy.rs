//! Conversation privacy is pinned in its owner's database, never inferred from later agent edits.
use super::*;
impl Store {
    pub(crate) async fn pin_conversation_privacy(&self)->Result<()> {
        if self.get("migration:conversation_privacy_v1").await?.is_some() {return Ok(());}
        let sessions=self.list("session:").await?;
        let mut modes=HashMap::new();
        for session in &sessions {
            let agent=session["agent_id"].as_str().unwrap_or("shared_default");
            if !modes.contains_key(agent) {
                let registered:Option<String>=sqlx::query_scalar("SELECT id FROM agent_directory WHERE id=?").bind(agent).fetch_optional(&self.app_pool).await?;
                let mode=if registered.is_some() {
                    let policy=crate::shared_chat::policy(self,agent).await?;
                    if policy.admin_view||policy.admin_join {"operator_reviewable"}else{"private"}
                }else{"private"};
                modes.insert(agent.to_owned(),mode);
            }
        }
        let mut tx=self.pool.begin().await?;
        sqlx::query("UPDATE docs SET value=value WHERE key='migration:conversation_privacy_v1'").execute(&mut *tx).await?;
        for original in sessions {
            let Some(sid)=original["id"].as_str() else {continue};let key=format!("session:{sid}");
            let raw:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(&key).fetch_optional(&mut *tx).await?;
            let Some(raw)=raw else {continue};let mut session=self.unseal(&raw)?;
            if session.get("conversation_privacy").is_some() {continue;}
            session["conversation_privacy"]=json!(modes[session["agent_id"].as_str().unwrap_or("shared_default")]);
            self.write_document(&mut tx,&key,&session).await?;
        }
        self.write_document(&mut tx,"migration:conversation_privacy_v1",&json!({"version":1})).await?;
        tx.commit().await?;Ok(())
    }
    pub(crate) async fn operator_join(&self,sid:&str,user:&str)->Result<()> {
        let key=format!("session:{sid}");let mut tx=self.pool.begin().await?;
        sqlx::query("UPDATE docs SET value=value WHERE key=?").bind(&key).execute(&mut *tx).await?;
        let raw:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(&key).fetch_one(&mut *tx).await?;
        let mut session=self.unseal(&raw)?;
        if session["conversation_privacy"]!="operator_reviewable" || session["status"]!="active" {return Err(anyhow!("Conversation is private or unavailable"));}
        if !session["operator_joined"].is_object() {session["operator_joined"]=json!({});}
        session["operator_joined"][user]=json!(true);
        self.write_document(&mut tx,&key,&session).await?;tx.commit().await?;Ok(())
    }
}
