//! Each agent has a random data key; only the trusted storage boundary unwraps it.
//! Storage owns the envelope format and key conversion; this view selects the key.
use super::*;

impl Store {
    pub(crate) fn seal_agent(&self,value:&Value)->Result<Vec<u8>> {
        self.seal_agent_for(&self.agent_id,value)
    }
    /// Select the bound view's key first, then the shared registry.
    fn agent_key(&self,agent:&str)->Result<AgentCipher> {
        let keys=self.agent_keys.read().map_err(|_|anyhow!("Agent key registry unavailable"))?;
        if agent==self.agent_id {self.agent_cipher.as_ref().or_else(||keys.get(agent))} else {keys.get(agent)}
            .cloned().ok_or_else(||anyhow!("Agent key unavailable"))
    }
    pub(crate) fn seal_agent_for(&self,agent:&str,value:&Value)->Result<Vec<u8>> {
        self.agent_key(agent)?.seal(agent,value)
    }
    pub(crate) fn unseal_agent(&self,data:&[u8])->Result<Value> {
        self.unseal_agent_for(&self.agent_id,data)
    }
    pub(crate) fn unseal_agent_for(&self,agent:&str,data:&[u8])->Result<Value> {
        if data.len()<36 || !webagent_storage::codec::is_agent_envelope(data) {return Err(anyhow!("Invalid agent record envelope"));}
        self.agent_key(agent)?.unseal(agent,data)
    }
    pub(crate) async fn initialize_agent_encryption(&mut self)->Result<()> {
        if let Some(cipher)=self.agent_keys.read().map_err(|_|anyhow!("Agent key registry unavailable"))?.get(&self.agent_id).cloned() {
            self.agent_cipher=Some(cipher);return Ok(());
        }
        let cipher=webagent_storage::codec::initialize_agent_key(&self.agent_pool,&self.codec,&self.agent_id).await?;
        self.agent_cipher=Some(cipher.clone());
        self.agent_keys.write().map_err(|_|anyhow!("Agent key registry unavailable"))?.insert(self.agent_id.clone(),cipher);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn second_agent(store:&Store)->Store {
        let id="key_isolation_test";
        let pool=crate::agent_storage::open_agent_pool(&store.root,id).await.unwrap();
        sqlx::query("INSERT INTO agents VALUES(?,?,'','openai_subscription','','active',?,1,1,1)")
            .bind(id).bind(id).bind(store.seal(&json!({"owner_id":"admin"})).unwrap()).execute(&pool).await.unwrap();
        pool.close().await;store.register_agent(id,"admin").await.unwrap();
        store.for_agent(id).await.unwrap()
    }
    #[tokio::test]
    async fn agent_keys_are_distinct_bound_to_identity_and_survive_restart() {
        let root=tempfile::tempdir().unwrap();let store=Store::open(root.path()).await.unwrap();
        store.create_admin_account("owner","fixture-password-hash","generation").await.unwrap();
        let other=second_agent(&store).await;
        let record=json!({"setting":"agent-private-fixture"});
        let sealed=store.seal_agent(&record).unwrap();
        assert_eq!(store.unseal_agent(&sealed).unwrap(),record);
        assert!(other.unseal_agent(&sealed).is_err());
        assert!(store.unseal_agent_for("key_isolation_test",&sealed).is_err());
        let bytes=other.seal_agent(&record).unwrap();
        assert!(store.unseal_agent(&bytes).is_err());
        store.put("agent:shared_default:key_test",&record).await.unwrap();
        let reopened=Store::open(root.path()).await.unwrap();
        assert_eq!(reopened.get("agent:shared_default:key_test").await.unwrap(),Some(record));
        assert_eq!(reopened.unseal_agent(&sealed).unwrap()["setting"],"agent-private-fixture");
    }
    #[tokio::test]
    async fn initialized_model_rosters_do_not_reenter_the_app_connection_pool() {
        let root=tempfile::tempdir().unwrap();let store=Store::open(root.path()).await.unwrap();
        store.create_admin_account("owner","fixture-password-hash","generation").await.unwrap();
        let other=second_agent(&store).await;
        let mut held=Vec::new();for _ in 0..4 {held.push(store.app_pool.acquire().await.unwrap());}
        let pool=tokio::time::timeout(std::time::Duration::from_secs(1),
            webagent_models::storage::RuntimeStorage::agent_pool(&store,"key_isolation_test")).await
            .expect("agent routing must not wait for another app connection").unwrap();
        let id:String=sqlx::query_scalar("SELECT id FROM agents").fetch_one(&pool).await.unwrap();
        assert_eq!(id,other.current_agent_id());
    }
    #[tokio::test]
    async fn missing_agent_key_fails_closed_without_replacing_ciphertext() {
        let root=tempfile::tempdir().unwrap();let store=Store::open(root.path()).await.unwrap();
        let before:Vec<u8>=sqlx::query_scalar("SELECT metadata FROM agents WHERE id='shared_default'").fetch_one(&store.agent_pool).await.unwrap();
        sqlx::query("DELETE FROM agent_key_envelopes").execute(&store.agent_pool).await.unwrap();
        assert!(Store::open(root.path()).await.is_err());
        let after:Vec<u8>=sqlx::query_scalar("SELECT metadata FROM agents WHERE id='shared_default'").fetch_one(&store.agent_pool).await.unwrap();
        assert_eq!(before,after);
    }
    #[tokio::test]
    async fn tampered_key_identity_and_record_ciphertext_are_rejected() {
        let root=tempfile::tempdir().unwrap();let store=Store::open(root.path()).await.unwrap();
        let mut bytes=store.seal_agent(&json!({"setting":true})).unwrap();
        let last=bytes.len()-1;bytes[last]^=1;assert!(store.unseal_agent(&bytes).is_err());
        sqlx::query("UPDATE agent_key_envelopes SET envelope=? WHERE id=1")
            .bind(store.seal(&json!({"version":1,"agent_id":"different_agent","key":vec![0u8;32]})).unwrap()).execute(&store.agent_pool).await.unwrap();
        assert!(Store::open(root.path()).await.is_err());
    }
}
