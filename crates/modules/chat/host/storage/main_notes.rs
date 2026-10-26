//! Agent-owned encrypted Notes. A durable journal bridges agent state and user
//! conversations; its owner-plane receipt makes interrupted transitions replayable.
use super::*;
const KEY:&str="main_chat";
const PENDING:&str="main_chat_pending";
impl Store {
    async fn main_chat_raw(&self,key:&str)->Result<Option<Value>> {
        let raw:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(key).fetch_optional(&self.agent_pool).await?;
        raw.map(|v|self.unseal(&v)).transpose()
    }
    pub(crate) async fn main_chat_owner(&self)->Result<Self> {
        let owner:String=sqlx::query_scalar("SELECT owner_id FROM agent_directory WHERE id=?").bind(&self.agent_id).fetch_one(&self.app_pool).await?;
        self.for_user(&owner).await
    }
    pub(crate) async fn main_chat_agent_ids(&self)->Result<Vec<String>> {
        Ok(sqlx::query_scalar("SELECT id FROM agent_directory WHERE owner_id=? ORDER BY id").bind(&self.user_id).fetch_all(&self.app_pool).await?)
    }
    // Journal recovery has independent SQL transactions. Keep its future on
    // the heap rather than growing every model/context caller's async frame.
    pub(crate) async fn main_chat_state(&self)->Result<Value> {
        Box::pin(self.reconcile_main_chat()).await?;
        let mut state=self.main_chat_raw(KEY).await?.unwrap_or_else(crate::main_chat::empty);
        state["agent_id"]=json!(self.agent_id);
        Ok(state)
    }
    async fn reconcile_main_chat(&self)->Result<()> {
        let mut agent_tx=self.agent_pool.begin().await?;
        sqlx::query("UPDATE docs SET key=key WHERE key IN (?,?)").bind(KEY).bind(PENDING).execute(&mut *agent_tx).await?;
        let raw:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(PENDING).fetch_optional(&mut *agent_tx).await?;
        if let Some(raw)=raw {
            let pending=self.unseal(&raw)?;
            let owner=self.main_chat_owner().await?;
            let request=pending["request"].as_str().ok_or_else(||anyhow!("Invalid Main Chat journal"))?;
            let receipt=format!("receipt:main_chat:{}:{request}",self.agent_id);
            let mut tx=owner.pool.begin().await?;
            sqlx::query("UPDATE docs SET key=key WHERE key=?").bind(&receipt).execute(&mut *tx).await?;
            let done:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(&receipt).fetch_optional(&mut *tx).await?;
            if done.is_none() {
                for pair in pending["extra"].as_array().into_iter().flatten() {
                    let key=pair[0].as_str().ok_or_else(||anyhow!("Invalid Main Chat journal key"))?;
                    if !key.starts_with("session:")&&!key.starts_with("session_mode:")&&!key.starts_with("session_ability_state:"){return Err(anyhow!("Invalid Main Chat initialization"));}
                    let stored=if key.starts_with("session:"){session::with_session_metadata(&pair[1],None)?}else{pair[1].clone()};
                    sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)").bind(key).bind(owner.encode_user_record(&stored)?).execute(&mut *tx).await?;
                }
                if let Some(sid)=pending["previous_session"].as_str() {
                    let key=format!("session:{sid}");
                    let raw:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(&key).fetch_one(&mut *tx).await?;
                    let mut session=owner.unseal(&raw)?;
                    if session["metadata_revision"]!=pending["expected_session_revision"]||session["main_chat"]!=true||session["main_chat_agent_id"]!=self.agent_id||session["agent_id"]!=self.agent_id {
                        tx.rollback().await?;
                        sqlx::query("DELETE FROM docs WHERE key=?").bind(PENDING).execute(&mut *agent_tx).await?;
                        agent_tx.commit().await?;
                        return Err(anyhow!("Session metadata revision changed; reload before saving"));
                    }
                    session["title"]=pending["state"]["settings"]["chat_name"].clone();
                    session["main_chat"]=json!(pending["state"]["settings"]["enabled"]==true);
                    session["metadata_revision"]=json!(session["metadata_revision"].as_u64().unwrap_or(0).checked_add(1).ok_or_else(||anyhow!("Session revision exhausted"))?);
                    session["auto_naming"]=json!({"version":1,"stage":"locked","claim":null,"lease_until":0});
                    sqlx::query("UPDATE docs SET value=? WHERE key=?").bind(owner.encode_user_record(&session)?).bind(&key).execute(&mut *tx).await?;
                    let summary=json!({"session":owner.read_summary_in(&mut tx,sid,&session).await?});
                    sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,'session_updated',?)").bind(sid).bind(owner.encode_user_record(&summary)?).execute(&mut *tx).await?;
                }
                sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)").bind(&receipt).bind(owner.seal(&json!({"applied":true}))?).execute(&mut *tx).await?;
            }
            tx.commit().await?;
            if let Some(sid)=pending["state"]["session_id"].as_str(){owner.register_shared_session(owner.current_user_id(),sid,&self.agent_id).await?;}
            sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").bind(KEY).bind(self.seal(&pending["state"])?).execute(&mut *agent_tx).await?;
            sqlx::query("DELETE FROM docs WHERE key=?").bind(PENDING).execute(&mut *agent_tx).await?;
            let keys:Vec<String>=pending["extra"].as_array().into_iter().flatten().filter_map(|p|p[0].as_str().map(str::to_owned)).chain(pending["previous_session"].as_str().map(|s|format!("session:{s}"))).collect();
            owner.cache.lock().unwrap().invalidate(&keys);
        }
        agent_tx.commit().await?;
        self.emit_main_notes().await
    }
    /// The committed change log is the durable outbox. Notification staging dedups
    /// by agent/version; a crash before delivery remains recoverable on the next read.
    async fn emit_main_notes(&self)->Result<()> {
        let Some(state)=self.main_chat_raw(KEY).await? else{return Ok(())};
        let version=state["notes_version"].as_u64().unwrap_or(0);
        let through=self.main_chat_raw("main_notes_emitted").await?.and_then(|v|v.as_u64()).unwrap_or(0);
        if version<=through{return Ok(())}
        let owner=self.main_chat_owner().await?;
        let sid=state["session_id"].as_str().unwrap_or("");
        let last=version.min(through.saturating_add(128));
        let mut tx=owner.pool.begin().await?;
        for version in through+1..=last {
            owner.stage_notification(&mut tx,&format!("main_notes:{}:{version}",self.agent_id),"main_notes.updated",sid,None,Some(&version.to_string())).await?;
        }
        tx.commit().await?;
        let mut tx=self.agent_pool.begin().await?;
        sqlx::query("UPDATE docs SET key=key WHERE key='main_notes_emitted'").execute(&mut *tx).await?;
        let raw:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key='main_notes_emitted'").fetch_optional(&mut *tx).await?;
        let before=raw.map(|v|self.unseal(&v)).transpose()?.and_then(|v|v.as_u64()).unwrap_or(0);
        sqlx::query("INSERT INTO docs(key,value) VALUES('main_notes_emitted',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").bind(self.seal(&json!(last.max(before)))?).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    pub(crate) async fn main_chat_change<F>(&self,expected:u64,request:&str,fingerprint:&str,extra:Vec<(String,Value)>,change:F)->Result<Value>
    where F:FnOnce(&mut Value)->Result<Value> {
        Box::pin(self.reconcile_main_chat()).await?;
        let mut tx=self.agent_pool.begin().await?;
        sqlx::query("UPDATE docs SET key=key WHERE key IN (?,?)").bind(KEY).bind(PENDING).execute(&mut *tx).await?;
        if sqlx::query_scalar::<_,i64>("SELECT count(*) FROM docs WHERE key=?").bind(PENDING).fetch_one(&mut *tx).await?>0{return Err(anyhow!("Main Chat transition in progress; retry the same request"));}
        let raw:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(KEY).fetch_optional(&mut *tx).await?;
        let mut state=raw.map(|v|self.unseal(&v)).transpose()?.unwrap_or_else(crate::main_chat::empty);
        state["agent_id"]=json!(self.agent_id);
        if let Some(receipt)=state["receipts"].get(request) {
            if receipt["digest"]!=fingerprint{return Err(anyhow!("Request identifier reused"));}
            return Ok(receipt["result"].clone());
        }
        if state["revision"].as_u64()!=Some(expected){return Err(anyhow!("Main Notes revision changed"));}
        if state["receipts"].as_object().is_none_or(|v|v.len()>=4096){return Err(anyhow!("Main Notes receipt capacity reached"));}
        let previous=state["session_id"].clone();
        let result=change(&mut state)?;
        let session_revision=state.as_object_mut().unwrap().remove("_rename_expected_revision");
        state["revision"]=json!(expected.checked_add(1).ok_or_else(||anyhow!("Revision exhausted"))?);
        state["receipts"][request]=json!({"digest":fingerprint,"result":result});
        if serde_json::to_vec(&state)?.len()>3*1024*1024{return Err(anyhow!("Main Notes capacity reached"));}
        if !extra.is_empty()||session_revision.is_some()&&!previous.is_null() {
            let pending=json!({"request":request,"state":state,"extra":extra,"previous_session":if session_revision.is_some(){previous}else{Value::Null},"expected_session_revision":session_revision});
            sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)").bind(PENDING).bind(self.seal(&pending)?).execute(&mut *tx).await?;
        } else {
            sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").bind(KEY).bind(self.seal(&state)?).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Box::pin(self.reconcile_main_chat()).await?;
        Ok(result)
    }
}

#[cfg(test)]
mod transition_tests {
    use super::*;
    #[tokio::test]
    async fn interrupted_agent_transition_replays_exact_owner_receipt() {
        let (root,app)=crate::runtime_tests::fixture().await;let store=app.store.clone();
        let mut state=crate::main_chat::empty();state["revision"]=json!(1);state["settings"]["enabled"]=json!(true);state["session_id"]=json!("recovered");
        let pending=json!({"request":"recover-once","state":state,"previous_session":null,"expected_session_revision":null,
            "extra":[["session:recovered",{"id":"recovered","agent_id":"shared_default","title":"Main Chat","main_chat":true,"main_chat_agent_id":"shared_default"}]]});
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)").bind(PENDING).bind(store.seal(&pending).unwrap()).execute(&store.agent_pool).await.unwrap();
        let restored=Store::open(root.path()).await.unwrap();
        assert_eq!(restored.main_chat_state().await.unwrap()["session_id"],"recovered");
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM docs WHERE key='session:recovered'").fetch_one(&store.pool).await.unwrap();assert_eq!(count,1);
        // Simulate the earlier durable journal surviving a crash after owner commit.
        sqlx::query("DELETE FROM docs WHERE key=?").bind(KEY).execute(&store.agent_pool).await.unwrap();
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)").bind(PENDING).bind(store.seal(&pending).unwrap()).execute(&store.agent_pool).await.unwrap();
        assert_eq!(restored.main_chat_state().await.unwrap()["session_id"],"recovered");
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM docs WHERE key='session:recovered'").fetch_one(&store.pool).await.unwrap();assert_eq!(count,1);
    }
}
