//! Each agent has a random data key; only the trusted storage boundary unwraps it.
use super::*;
use aes_gcm::aead::Payload;

impl Store {
    pub(crate) fn seal_agent(&self,value:&Value)->Result<Vec<u8>> {
        self.seal_agent_for(&self.agent_id,value)
    }
    pub(crate) fn seal_agent_for(&self,agent:&str,value:&Value)->Result<Vec<u8>> {
        let keys=self.agent_keys.read().map_err(|_|anyhow!("Agent key registry unavailable"))?;
        let cipher=if agent==self.agent_id {self.agent_cipher.as_ref().or_else(||keys.get(agent))} else {keys.get(agent)}
            .ok_or_else(||anyhow!("Agent key unavailable"))?;
        let mut nonce=[0;12];rand::thread_rng().fill_bytes(&mut nonce);
        let mut result=b"WAAGENT1".to_vec();result.extend(nonce);
        result.extend(cipher.encrypt(Nonce::from_slice(&nonce),Payload{msg:&serde_json::to_vec(value)?,aad:agent.as_bytes()})
            .map_err(|_|anyhow!("Agent encryption failed"))?);
        Ok(result)
    }
    pub(crate) fn unseal_agent(&self,data:&[u8])->Result<Value> {
        self.unseal_agent_for(&self.agent_id,data)
    }
    pub(crate) fn unseal_agent_for(&self,agent:&str,data:&[u8])->Result<Value> {
        if data.len()<36 || !data.starts_with(b"WAAGENT1") {return Err(anyhow!("Invalid agent record envelope"));}
        let keys=self.agent_keys.read().map_err(|_|anyhow!("Agent key registry unavailable"))?;
        let cipher=if agent==self.agent_id {self.agent_cipher.as_ref().or_else(||keys.get(agent))} else {keys.get(agent)}
            .ok_or_else(||anyhow!("Agent key unavailable"))?;
        let plain=cipher.decrypt(Nonce::from_slice(&data[8..20]),Payload{msg:&data[20..],aad:agent.as_bytes()})
            .map_err(|_|anyhow!("Agent record authentication failed"))?;
        Ok(serde_json::from_slice(&plain)?)
    }
    pub(crate) async fn initialize_agent_encryption(&mut self)->Result<()> {
        if let Some(cipher)=self.agent_keys.read().map_err(|_|anyhow!("Agent key registry unavailable"))?.get(&self.agent_id).cloned() {
            self.agent_cipher=Some(cipher);return Ok(());
        }
        sqlx::query("CREATE TABLE IF NOT EXISTS agent_key_envelopes(id INTEGER PRIMARY KEY CHECK(id=1),envelope BLOB NOT NULL)")
            .execute(&self.agent_pool).await?;
        let mut tx=self.agent_pool.begin().await?;
        sqlx::query("UPDATE agents SET revision=revision WHERE id=?").bind(&self.agent_id).execute(&mut *tx).await?;
        let envelope:Option<Vec<u8>>=sqlx::query_scalar("SELECT envelope FROM agent_key_envelopes WHERE id=1").fetch_optional(&mut *tx).await?;
        let (key,convert)=match envelope {
            Some(bytes)=>{
                let value=self.unseal(&bytes)?;
                if value["version"]!=1 || value["agent_id"]!=self.agent_id {return Err(anyhow!("Agent key identity mismatch"));}
                let key:Vec<u8>=serde_json::from_value(value["key"].clone())?;
                (key,false)
            },
            None=>{
                // A completed conversion must never be repaired by silently creating another key.
                let exists:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
                    .bind(format!("agent:{}:encryption_v1",self.agent_id)).fetch_optional(&mut *tx).await?;
                if exists.is_some() {return Err(anyhow!("Agent key missing for existing encrypted data"));}
                let mut key=vec![0;32];rand::thread_rng().fill_bytes(&mut key);
                let wrapped=self.seal(&json!({"version":1,"agent_id":self.agent_id,"key":key}))?;
                sqlx::query("INSERT INTO agent_key_envelopes VALUES(1,?)").bind(wrapped).execute(&mut *tx).await?;
                (key,true)
            }
        };
        let cipher=Arc::new(Aes256Gcm::new_from_slice(&key).map_err(|_|anyhow!("Invalid agent key"))?);
        self.agent_cipher=Some(cipher.clone());

        if convert {
            // Fixed schema only; convert each payload and its key envelope in one transaction.
            for (table,column) in [("agents","metadata"),("agent_connections","config"),("agent_prompts","content"),("docs","value"),("prompt_slots","content"),("agent_memberships","value")] {
                let exists:Option<String>=sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='table' AND name=?").bind(table).fetch_optional(&mut *tx).await?;
                if exists.is_none() {continue;}
                let query=match table {
                    "agents"=>format!("SELECT rowid,{column} FROM {table} WHERE id=?"),
                    "docs"=>format!("SELECT rowid,{column} FROM {table} WHERE key=? OR key=? OR substr(key,1,?)=? OR key LIKE 'capability_audit:%' OR key LIKE 'ability_policy_audit:%' OR key LIKE 'ability_visibility_audit:%'"),
                    "prompt_slots"=>format!("SELECT rowid,{column} FROM {table}"),
                    _=>format!("SELECT rowid,{column} FROM {table} WHERE agent_id=?"),
                };
                let query=sqlx::query(&query);
                let query=if table=="docs" {
                    let prefix=format!("agent:{}:",self.agent_id);
                    query.bind(format!("agent:{}",self.agent_id)).bind(format!("agent_abilities:{}",self.agent_id)).bind(prefix.len() as i64).bind(prefix)
                }else if table=="prompt_slots" {query}else{query.bind(&self.agent_id)};
                let rows=query.fetch_all(&mut *tx).await?;
                for row in rows {
                    let bytes:Vec<u8>=row.get(column);
                    let value=self.unseal(&bytes)?;
                    let sealed=self.seal_agent(&value)?;
                    sqlx::query(&format!("UPDATE {table} SET {column}=? WHERE rowid=?"))
                        .bind(&sealed).bind(row.get::<i64,_>("rowid")).execute(&mut *tx).await?;
                    if self.unseal_agent(&sealed)?!=value {return Err(anyhow!("Agent conversion verification failed"));}
                }
            }
            sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)").bind(format!("agent:{}:encryption_v1",self.agent_id))
                .bind(self.seal_agent(&json!({"version":1}))?).execute(&mut *tx).await?;
        }
        tx.commit().await?;
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
