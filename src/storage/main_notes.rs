//! Main Chat and Main Notes are one bounded, always-encrypted user aggregate.
//! SQLite writer reservation commits note changes, proposals and wake events together.
use super::*;
const KEY: &str = "module_state:main_chat";
impl Store {
    pub(crate) async fn main_chat_state(&self) -> Result<Value> {
        Ok(self.get(KEY).await?.unwrap_or_else(crate::main_chat::empty))
    }

    /// Serialize API and loop writers across processes; receipts never imply execution permission.
    pub(crate) async fn main_chat_change<F>(&self, expected: u64, request: &str,
        fingerprint: &str, extra: Vec<(String,Value)>, change: F) -> Result<Value>
    where F: FnOnce(&mut Value) -> Result<Value> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE docs SET key=key WHERE key=?").bind(KEY).execute(&mut *tx).await?;
        let raw:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(KEY).fetch_optional(&mut *tx).await?;
        let mut state=raw.map(|v|self.unseal(&v)).transpose()?.unwrap_or_else(crate::main_chat::empty);
        if let Some(receipt)=state["receipts"].get(request) {
            if receipt["digest"]!=fingerprint {return Err(anyhow!("Request identifier reused"));}
            return Ok(receipt["result"].clone());
        }
        if state["revision"].as_u64()!=Some(expected) {return Err(anyhow!("Main Notes revision changed"));}
        if state["receipts"].as_object().is_none_or(|v|v.len()>=4096) {return Err(anyhow!("Main Notes receipt capacity reached"));}
        let previous_version=state["notes_version"].as_u64().unwrap_or(0);
        let result=change(&mut state)?;
        let rename_revision=state.as_object_mut().ok_or_else(||anyhow!("Invalid notes state"))?.remove("_rename_expected_revision");
        state["revision"]=json!(expected.checked_add(1).ok_or_else(||anyhow!("Revision exhausted"))?);
        state["receipts"][request]=json!({"digest":fingerprint,"result":result});
        let bytes=serde_json::to_vec(&state)?;
        if bytes.len()>3*1024*1024 {return Err(anyhow!("Main Notes capacity reached"));}
        let mut invalidated=vec![KEY.to_owned()];
        for (key,value) in extra {
            if !key.starts_with("session:") && !key.starts_with("session_mode:") && !key.starts_with("session_ability_state:") {return Err(anyhow!("Invalid Main Chat initialization document"));}
            let stored=if key.starts_with("session:") {session::with_session_metadata(&value,None)?} else {value};
            sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)").bind(&key).bind(self.encode_user_record(&stored)?).execute(&mut *tx).await?;
            invalidated.push(key);
        }
        if let Some(rename_revision)=rename_revision.filter(|_|invalidated.len()==1) {
            if let Some(sid)=state["session_id"].as_str() {
                let key=format!("session:{sid}");
                let raw:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(&key).fetch_one(&mut *tx).await?;
                let mut session=self.unseal(&raw)?;
                let expected=rename_revision.as_u64().ok_or_else(||anyhow!("Expected session metadata revision required"))?;
                if session["metadata_revision"].as_u64()!=Some(expected) {return Err(anyhow!("Session name changed; reload before saving"));}
                if session["title"]!=state["settings"]["chat_name"] {
                    session["title"]=state["settings"]["chat_name"].clone();
                    session["metadata_revision"]=json!(expected.checked_add(1).ok_or_else(||anyhow!("Session metadata revision exhausted"))?);
                    if !session["auto_naming"].is_object(){session["auto_naming"]=json!({"version":1});}
                    session["auto_naming"]["stage"]=json!("locked");session["auto_naming"]["claim"]=Value::Null;session["auto_naming"]["lease_until"]=json!(0);
                    sqlx::query("UPDATE docs SET value=? WHERE key=?").bind(self.encode_user_record(&session)?).bind(&key).execute(&mut *tx).await?;
                    let summary=json!({"session":self.read_summary_in(&mut tx,sid,&session).await?});
                    sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?, 'session_updated', ?)").bind(sid).bind(self.encode_user_record(&summary)?).execute(&mut *tx).await?;
                    invalidated.push(key);
                }
            }
        }
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(KEY).bind(self.seal(&state)?).execute(&mut *tx).await?;
        if state["notes_version"].as_u64().unwrap_or(0)>previous_version {
            let version=state["notes_version"].as_u64().ok_or_else(||anyhow!("Missing change version"))?;
            let sid=state["session_id"].as_str().ok_or_else(||anyhow!("Main Chat is not initialized"))?;
            // Every committed change remains observable. The coordinator's durable
            // consumer separately suppresses its own progress-only review wake.
            self.stage_notification(&mut tx,&format!("main_notes:{version}"),"main_notes.updated",sid,None,Some(&version.to_string())).await?;
        }
        tx.commit().await?;
        self.cache.lock().unwrap().invalidate(&invalidated);
        Ok(result)
    }
}
