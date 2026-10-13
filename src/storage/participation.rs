//! Persistent participation and follow-up admission share the conversation writer.
#[cfg(test)] use crate::State;
use super::*;
fn state(session: &Value) -> Value {
    session
        .get("participation")
        .cloned()
        .unwrap_or_else(|| json!({"active":true,"revision":0,"generation":0}))
}
impl Store {
    pub(crate) async fn shared_model_context(&self, sid: &str) -> Result<Vec<Value>> {
        let rows =
            sqlx::query("SELECT run_id,role,content FROM chat WHERE session_id=? ORDER BY seq")
                .bind(sid)
                .fetch_all(&self.pool)
                .await?;
        let legacy=self.followup_queue(sid).await?;
        let excluded:std::collections::HashSet<String>=legacy["entries"].as_array().into_iter().flatten().filter(|entry|entry["status"]!="delivered").filter_map(|entry|entry["id"].as_str().map(str::to_owned)).collect();
        let mut input = Vec::new();
        for row in rows {
            let rid: String = row.get("run_id");
            if excluded.contains(&rid){continue;}
            if self
                .get(&format!("message_meta:{rid}"))
                .await?
                .is_some_and(|meta| meta["context_eligible"] == false || meta["queued"] == true)
            {
                continue;
            }
            let role: String = row.get("role");
            if !matches!(role.as_str(), "user" | "assistant") {
                continue;
            }
            let mut content = self.unseal(&row.get::<Vec<u8>, _>("content"))?;
            if role == "user" {
                if let Some(meta)=self.get(&format!("message_meta:{rid}")).await? {if meta["context_eligible"]==true && meta["context_content"].is_string(){content=meta["context_content"].clone();}}
                if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>(
                    "SELECT item FROM interactions WHERE session_id=? AND run_id=? AND step='user'",
                )
                .bind(sid)
                .bind(&rid)
                .fetch_optional(&self.pool)
                .await?
                {
                    let admitted = self.unseal(&raw)?;
                    if admitted["role"] == "user" && admitted["content"].is_string() {
                        content = admitted["content"].clone();
                    }
                }
            }
            if let Some(text) = content.as_str() {
                input.push(json!({"role":role,"content":text}));
            }
        }
        // A participant receives public history, never the storage owner's private
        // execution interactions, compaction checkpoints or diagnostic records.
        Ok(input)
    }

    pub(crate) async fn model_message_allowed(&self, sid: &str, rid: &str) -> Result<bool> {
        if self.get(&format!("message_meta:{rid}")).await?.is_some_and(|meta|meta["context_eligible"]==false || meta["queued"]==true) {return Ok(false);}
        let queue=self.followup_queue(sid).await?;
        Ok(!queue["entries"].as_array().into_iter().flatten().any(|entry|entry["id"]==rid && entry["status"]!="delivered"))
    }

    pub(crate) async fn model_chat_page(&self, sid: &str, after: i64, limit: i64) -> Result<Value> {
        let mut page = self.chat_page(sid, after, limit).await?;
        if let Some(rows) = page["messages"].as_array_mut() {
            for row in rows.iter_mut() {
                let rid = row["run_id"].as_str().unwrap_or("");
                if !self.model_message_allowed(sid,rid).await?
                {
                    // Preserve cursor progression without disclosing excluded text.
                    row["content"] = Value::Null;
                    row["role"] = json!("excluded");
                }
            }
        }
        Ok(page)
    }
    pub(crate) async fn model_search_hit_allowed(&self, hit: &Value) -> Result<bool> {
        let Some(seq) = hit["seq"].as_i64() else {
            return Ok(true);
        };
        let sid = hit["session_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Search session missing"))?;
        let rid: Option<String> =
            sqlx::query_scalar("SELECT run_id FROM chat WHERE seq=? AND session_id=?")
                .bind(seq)
                .bind(sid)
                .fetch_optional(&self.pool)
                .await?;
        let Some(rid) = rid else { return Ok(false) };
        self.model_message_allowed(sid,&rid).await
    }
    pub(crate) async fn participation_state(&self, sid: &str) -> Result<Value> {
        Ok(state(
            &self
                .get(&format!("session:{sid}"))
                .await?
                .ok_or_else(|| anyhow!("Session not found"))?,
        ))
    }
    pub(crate) async fn participation_allows_run(&self, run: &Value) -> Result<bool> {
        let current = self
            .participation_state(
                run["session_id"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Run session missing"))?,
            )
            .await?;
        Ok(current["active"] == true
            && current["generation"].as_u64().unwrap_or(0)
                == run["participation_generation"].as_u64().unwrap_or(0))
    }
    pub(crate) async fn require_participation(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        run: &Value,
    ) -> Result<()> {
        let sid = run["session_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Run session missing"))?;
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(format!("session:{sid}"))
            .fetch_one(&mut **tx)
            .await?;
        let current = state(&self.unseal(&raw)?);
        if current["active"] != true
            || current["generation"].as_u64().unwrap_or(0)
                != run["participation_generation"].as_u64().unwrap_or(0)
        {
            return Err(anyhow!("Agent participation changed"));
        }
        Ok(())
    }
    pub(crate) async fn set_participation(
        &self,
        sid: &str,
        actor: &str,
        active: bool,
        expected: u64,
        request_id: &str,
    ) -> Result<Value> {
        self.background_index().await?;
        let _context_guard = self.context_gate.write().await;
        let key = format!("session:{sid}");
        let receipt = format!("receipt:session:{sid}:participation:{actor}:{request_id}");
        let input = json!({"active":active,"expected_revision":expected});
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE docs SET key=key WHERE key=?")
            .bind(&key)
            .execute(&mut *tx)
            .await?;
        if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&receipt)
            .fetch_optional(&mut *tx)
            .await?
        {
            let saved = self.unseal(&raw)?;
            if saved["input"] != input {
                return Err(anyhow!("Identifier reused"));
            }
            return Ok(saved["result"].clone());
        }
        self.require_active_session(&mut tx, sid).await?;
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(&key)
            .fetch_one(&mut *tx)
            .await?;
        let mut session = self.unseal(&raw)?;
        let mut current = state(&session);
        if current["revision"].as_u64().unwrap_or(0) != expected {
            return Err(anyhow!("Participation revision changed"));
        }
        if current["active"] != active {
            current["active"] = json!(active);
            current["revision"] = json!(expected + 1);
            current["generation"] = json!(current["generation"].as_u64().unwrap_or(0) + 1);
            current["actor_id"] = json!(actor);
            session["participation"] = current.clone();
            if !active {
            self.cancel_session_followups(&mut tx,sid).await?;
                if matches!(session["auto_naming"]["stage"].as_str(),Some("waiting_turn"|"queued"|"generating"|"failed")) {
                    session["auto_naming"]["stage"]=json!("cancelled");
                    session["auto_naming"]["claim"]=Value::Null;
                    session["auto_naming"]["lease_until"]=json!(0);
                    session["auto_naming"]["source"]=Value::Null;
                    session["auto_naming"]["system_plan"]=Value::Null;
                    sqlx::query("DELETE FROM docs WHERE key=?").bind(format!("session_naming:{sid}")).execute(&mut *tx).await?;
                }
                if let Some(queue) = session["message_queue"].as_array() {
                    for item in queue {
                        if let Some(rid) = item["message"]["run_id"].as_str() {
                            self.write_document(&mut tx,&format!("message_meta:{rid}"),&json!({"author_id":item["message"]["author_id"],"context_eligible":false,"queued":false})).await?;
                        }
                    }
                }
                session["message_queue"] = json!([]);
                session["queue_revision"] =
                    json!(session["queue_revision"].as_u64().unwrap_or(0) + 1);
                let rows=sqlx::query("SELECT d.key,d.value FROM docs d JOIN run_index r ON d.key='run:'||r.id WHERE r.session_id=? AND r.status NOT IN ('completed','cancelled','failed')").bind(sid).fetch_all(&mut *tx).await?;
                for row in rows {
                    let run_key: String = row.get("key");
                    let mut run = self
                        .decode_document(&mut tx, &row.get::<Vec<u8>, _>("value"))
                        .await?;
                    if run["context_hydration_pending"] == true {
                        let rid = run["id"]
                            .as_str()
                            .ok_or_else(|| anyhow!("Run identity missing"))?;
                        self.write_document(&mut tx,&format!("message_meta:{rid}"),&json!({"author_id":run["requester_user_id"].as_str().unwrap_or(self.current_user_id()),"context_eligible":false,"queued":false})).await?;
                        // A cached/compacted checkpoint must not retain an admitted
                        // message that never reached execution. Rebuild from eligible
                        // immutable interactions at the next active turn.
                        session["checkpoint_input"] = json!([]);
                        session["checkpoint_through"] = json!(0);
                    }
                    // Generation changes fence publication; retain uncertain effects for recovery.
                    let executing = run["pending"]["state"] == "executing"
                        || run["uncertain"] == true
                        || run["batch"].as_array().is_some_and(|batch| {
                            batch.iter().any(|item| {
                                item["state"] == "executing"
                                    || item["pending"]["state"] == "executing"
                                    || item["uncertain"] == true
                            })
                        });
                    run["status"] = json!(if executing { "stopped" } else { "cancelled" });
                    run["generation"] = json!(uuid::Uuid::new_v4().to_string());
                    run["run_revision"] = json!(run["run_revision"].as_u64().unwrap_or(0) + 1);
                    run["pending"]["decision"] = Value::Null;
                    if let Some(batch) = run["batch"].as_array_mut() {
                        for item in batch {
                            item["pending"]["decision"] = Value::Null;
                            item["pending"]["human_approved"] = json!(false);
                        }
                    }
                    run["error"] = json!("Agent deactivated");
                    self.write_document(&mut tx, &run_key, &run).await?;
                    self.index_run(&mut tx, &run).await?;
                }
                let tasks=sqlx::query("SELECT d.key,d.value FROM docs d JOIN background_task_index b ON d.key='background_task:'||b.id WHERE b.session_id=? AND b.status IN ('queued','running')").bind(sid).fetch_all(&mut *tx).await?;
                for row in tasks {
                    let task_key: String = row.get("key");
                    let mut task = self.unseal(&row.get::<Vec<u8>, _>("value"))?;
                    task["status"] = json!("cancelled");
                    task["revision"] = json!(task["revision"].as_u64().unwrap_or(0) + 1);
                    task["lease_until"] = json!(0);
                    sqlx::query(
                        "UPDATE background_task_index SET status='cancelled',revision=? WHERE id=?",
                    )
                    .bind(task["revision"].as_u64().unwrap() as i64)
                    .bind(task["id"].as_str().unwrap_or(""))
                    .execute(&mut *tx)
                    .await?;
                    sqlx::query("UPDATE docs SET value=? WHERE key=?")
                        .bind(self.seal(&task)?)
                        .bind(task_key)
                        .execute(&mut *tx)
                        .await?;
                }
                if let Some(raw) =
                    sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
                        .bind(format!("manager_pending:{sid}"))
                        .fetch_optional(&mut *tx)
                        .await?
                {
                    let pointer = self.unseal(&raw)?;
                    if let Some(id) = pointer["id"].as_str() {
                        if let Some(raw) =
                            sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
                                .bind(format!("manager_review:{id}"))
                                .fetch_optional(&mut *tx)
                                .await?
                        {
                            let mut review = self.unseal(&raw)?;
                            review["status"] = json!("cancelled");
                            self.write_document(&mut tx,&format!("message_meta:{id}"),&json!({"author_id":review["message"]["author_id"],"context_eligible":false,"queued":false})).await?;
                            review["generation"] = json!(uuid::Uuid::new_v4().to_string());
                            self.write_document(&mut tx, &format!("manager_review:{id}"), &review)
                                .await?;
                        }
                    }
                    sqlx::query("DELETE FROM docs WHERE key=?")
                        .bind(format!("manager_pending:{sid}"))
                        .execute(&mut *tx)
                        .await?;
                }
            }
            self.write_document(&mut tx, &key, &session).await?;
            sqlx::query(
                "INSERT INTO events(session_id,kind,payload) VALUES(?,'participation_changed',?)",
            )
            .bind(sid)
            .bind(self.encode_user_record(&current)?)
            .execute(&mut *tx)
            .await?;
        }
        self.write_document(&mut tx, &receipt, &json!({"input":input,"result":current}))
            .await?;
        tx.commit().await?;
        self.cache.lock().unwrap().invalidate(&[]);
        self.context_cache
            .lock()
            .unwrap()
            .invalidate(&self.context_cache_key(sid));
        Ok(current)
    }
    pub(crate) async fn admit_shared_message(
        &self,
        docs: Vec<(String, Value)>,
        sid: &str,
        kind: &str,
        message: &Value,
    ) -> Result<Value> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE docs SET key=key WHERE key=?")
            .bind(format!("session:{sid}"))
            .execute(&mut *tx)
            .await?;
        self.require_active_session(&mut tx, sid).await?;
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(format!("session:{sid}"))
            .fetch_one(&mut *tx)
            .await?;
        let mut session = self.unseal(&raw)?;
        let current = state(&session);
        let rid = message["run_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Message id missing"))?;
        let receipts: Vec<_> = docs
            .iter()
            .filter(|(k, _)| k.starts_with("receipt:message:")||k.starts_with("receipt:module:"))
            .collect();
        for (key, input) in &receipts {
            if let Some(raw) =
                sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
                    .bind(key)
                    .fetch_optional(&mut *tx)
                    .await?
            {
                let saved = self.unseal(&raw)?;
                if saved["digest"] != input["digest"] {
                    return Err(anyhow!("Identifier reused"));
                }
                return Ok(saved["result"].clone());
            }
        }
        if message["participation_generation"].as_u64().is_some_and(|generation|generation!=current["generation"].as_u64().unwrap_or(0)) {
            return Err(anyhow!("Participation revision changed"));
        }
        let eligible=kind!="human_message" && current["active"]==true;
        let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM run_index WHERE session_id=? AND status NOT IN ('completed','cancelled','failed')").bind(sid).fetch_one(&mut *tx).await?;
        let review: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM docs WHERE key=?")
            .bind(format!("manager_pending:{sid}"))
            .fetch_one(&mut *tx)
            .await?;
        let queued = eligible
            && (count > 0
                || review > 0
                || session["message_queue"]
                    .as_array()
                    .is_some_and(|q| !q.is_empty()));
        let mut result = receipts
            .first()
            .map(|(_, r)| r["result"].clone())
            .unwrap_or_else(|| json!({"run_id":rid}));
        if queued || !eligible {
            result["run_id"] = Value::Null;
            result["review_id"] = Value::Null;
            result["queued"] = json!(queued);
            result["agent_active"] = current["active"].clone();
        }
        let mut retained = Vec::new();
        for (key, mut value) in docs {
            if key.starts_with("receipt:message:")||key.starts_with("receipt:module:") {
                value["result"] = result.clone();
                self.write_document(&mut tx, &key, &value).await?;
            } else if key.starts_with("run:") || key.starts_with("manager_") {
                if eligible {
                    retained.push((key, value));
                }
            } else {
                self.write_document(&mut tx, &key, &value).await?;
            }
        }
        self.write_document(&mut tx,&format!("message_meta:{rid}"),&json!({"author_id":message["author_id"],"context_eligible":eligible,"queued":queued})).await?;
        self.insert_chat(&mut tx, sid, rid, "user", &message["content"])
            .await?;
        if queued {
            let queue = session
                .as_object_mut()
                .unwrap()
                .entry("message_queue")
                .or_insert(json!([]))
                .as_array_mut()
                .ok_or_else(|| anyhow!("Malformed message queue"))?;
            if queue.len() >= 256 {
                return Err(anyhow!("Message queue is full"));
            }
            queue.push(json!({"docs":retained,"message":message,"kind":kind,"generation":current["generation"].as_u64().unwrap_or(0)}));
            session["queue_revision"] = json!(session["queue_revision"].as_u64().unwrap_or(0) + 1);
            self.write_document(&mut tx, &format!("session:{sid}"), &session)
                .await?;
        } else if eligible {
            self.activate_message(&mut tx, sid, kind, message, retained, &current)
                .await?;
        }
        tx.commit().await?;
        self.cache.lock().unwrap().invalidate(&[]);
        self.context_cache
            .lock()
            .unwrap()
            .invalidate(&self.context_cache_key(sid));
        Ok(result)
    }
    async fn activate_message(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        sid: &str,
        kind: &str,
        message: &Value,
        docs: Vec<(String, Value)>,
        current: &Value,
    ) -> Result<()> {
        if let Some((_,run))=docs.iter().find(|(key,_)|key.starts_with("run:")) {self.reserve_requester_run(run).await?;}
        else if let Some((_,review))=docs.iter().find(|(key,_)|key.starts_with("manager_review:")) {self.reserve_requester_run(&review["run"]).await?;}
        let rid = message["run_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Message id missing"))?;
        for (key, mut value) in docs {
            if key.starts_with("run:") {
                value["participation_generation"] = current["generation"].clone();
                value["run_revision"] = json!(1);
                self.write_document(tx, &key, &value).await?;
                self.index_run(tx, &value).await?;
            } else {
                if key.starts_with("manager_review:") {
                    value["run"]["participation_generation"] = current["generation"].clone();
                }
                self.write_document(tx, &key, &value).await?;
            }
        }
        self.write_document(
            tx,
            &format!("message_meta:{rid}"),
            &json!({"author_id":message["author_id"],"context_eligible":true,"queued":false}),
        )
        .await?;
        if kind == "message" {
            self.begin_session_naming(tx, sid, rid, message).await?;
            self.insert_interaction(tx,sid,rid,"user",&json!({"role":"user","content":message["context_content"].as_str().or_else(||message["content"].as_str()).unwrap_or(""),"author_id":message["author_id"]})).await?;
            sqlx::query("INSERT INTO loop_commands(command_id,run_id,schema_version,kind,payload,created_at) VALUES(?,?,1,'message',?,strftime('%s','now'))").bind(format!("shared:{rid}")).bind(rid).bind(self.encode_user_record(&json!({"schema_version":1,"run_id":rid,"kind":"message","status":"queued"}))?).execute(&mut **tx).await?;
        }
        Ok(())
    }
    pub(crate) async fn drain_message_queues(&self) -> Result<()> {
        for session in self.list("session:").await? {
            let Some(sid) = session["id"].as_str() else {
                continue;
            };
            if !session["message_queue"]
                .as_array()
                .is_some_and(|q| !q.is_empty())
            {
                continue;
            }
            let mut tx = self.pool.begin().await?;
            sqlx::query("UPDATE docs SET key=key WHERE key=?")
                .bind(format!("session:{sid}"))
                .execute(&mut *tx)
                .await?;
            let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
                .bind(format!("session:{sid}"))
                .fetch_one(&mut *tx)
                .await?;
            let mut latest = self.unseal(&raw)?;
            let current = state(&latest);
            if current["active"] != true {
                continue;
            }
            let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM run_index WHERE session_id=? AND status NOT IN ('completed','cancelled','failed')").bind(sid).fetch_one(&mut *tx).await?;
            let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM docs WHERE key=?")
                .bind(format!("manager_pending:{sid}"))
                .fetch_one(&mut *tx)
                .await?;
            let legacy=self.queue_read_tx(&mut tx,sid).await?;
            let legacy_pending=legacy["held"]==true || legacy["entries"].as_array().into_iter().flatten().any(|entry|matches!(entry["status"].as_str(),Some("pending"|"reviewing"|"held")));
            if count > 0 || pending > 0 || legacy_pending {
                continue;
            }
            let queue = latest["message_queue"]
                .as_array_mut()
                .ok_or_else(|| anyhow!("Malformed queue"))?;
            if queue.is_empty() {
                continue;
            }
            let item = queue.remove(0);
            if item["generation"].as_u64().unwrap_or(0)
                == current["generation"].as_u64().unwrap_or(0)
            {
                let docs: Vec<(String, Value)> = serde_json::from_value(item["docs"].clone())?;
                let activation=self.activate_message(
                    &mut tx,
                    sid,
                    item["kind"].as_str().unwrap_or("message"),
                    &item["message"],
                    docs,
                    &current,
                ).await;
                if activation.as_ref().is_err_and(|error|error.to_string()=="Account run limit reached"){continue;}
                activation?;
            }
            latest["queue_revision"] = json!(latest["queue_revision"].as_u64().unwrap_or(0) + 1);
            self.write_document(&mut tx, &format!("session:{sid}"), &latest)
                .await?;
            tx.commit().await?;
            self.cache.lock().unwrap().invalidate(&[]);
            self.context_cache
                .lock()
                .unwrap()
                .invalidate(&self.context_cache_key(sid));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::{Path},
        Json,
    };
    async fn fixture() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store.create_admin_account("root","long-hash-placeholder","generation").await.unwrap();
        for name in ["alice","bob"] {
            let mut account=store.create_user_account(name,"long-hash-placeholder").await.unwrap();
            account["user_id"]=json!(name);account["status"]=json!("active");
            sqlx::query("INSERT INTO user_accounts(user_id,username,value) VALUES(?,?,?)").bind(name).bind(format!("alias_{name}")).bind(store.seal(&account).unwrap()).execute(&store.app_pool).await.unwrap();
            store.for_user(name).await.unwrap();
        }
        store
            .put(
                "session:s",
                &json!({"id":"s","agent_id":"shared_default","status":"active"}),
            )
            .await
            .unwrap();
        (root, store)
    }
    fn envelope(id: &str, author: &str) -> (Vec<(String, Value)>, Value) {
        let message = json!({"role":"user","content":format!("sentinel-{id}"),"run_id":id,"author_id":author});
        let run = json!({"id":id,"session_id":"s","agent_id":"shared_default","requester_user_id":author,"status":"queued","generation":"","input":[],"context_hydration_pending":true});
        let docs = vec![
            (format!("run:{id}"), run),
            (
                format!("receipt:message:s:{author}:{id}"),
                json!({"digest":id,"result":{"run_id":id}}),
            ),
        ];
        (docs, message)
    }
    #[tokio::test]
    async fn eligible_attachment_context_is_shared_but_withdrawn_attachment_never_reappears() {
        let (_root, store) = fixture().await;
        let (docs, mut message) = envelope("attached", "alice");
        message["context_content"] = json!("public message with ACTIVE_ATTACHMENT_SENTINEL");
        store
            .admit_shared_message(docs, "s", "message", &message)
            .await
            .unwrap();
        assert!(json!(store.shared_model_context("s").await.unwrap())
            .to_string()
            .contains("ACTIVE_ATTACHMENT_SENTINEL"));
        store
            .set_participation("s", "bob", false, 0, "off")
            .await
            .unwrap();
        store
            .set_participation("s", "alice", true, 1, "on")
            .await
            .unwrap();
        for context in [
            store.context("s").await.unwrap(),
            store.shared_model_context("s").await.unwrap(),
        ] {
            assert!(!json!(context)
                .to_string()
                .contains("ACTIVE_ATTACHMENT_SENTINEL"));
        }
    }
    #[tokio::test]
    async fn inactive_attachment_remains_publicly_addressable_without_agent_eligibility() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        app.store
            .set_participation(&sid, app.store.current_user_id(), false, 0, "off")
            .await
            .unwrap();
        let (_,Json(result))=crate::send(State(app.clone()),Path(sid.clone()),Json(json!({"text":"human attachment","client_message_id":"human-attachment","attachments":[{"name":"note.txt","media_type":"text/plain","text":"INACTIVE_ATTACHMENT_SENTINEL"}]}))).await.unwrap();
        let aid = result["attachments"][0]["id"].as_str().unwrap();
        let record = app
            .store
            .get(&format!("attachment:{aid}"))
            .await
            .unwrap()
            .unwrap();
        let rid = record["run_id"].as_str().unwrap();
        assert_eq!(
            app.store
                .get(&format!("message_attachments:{rid}"))
                .await
                .unwrap()
                .unwrap()[0]["id"],
            aid
        );
        app.store
            .set_participation(&sid, app.store.current_user_id(), true, 1, "on")
            .await
            .unwrap();
        assert!(!json!(app.store.shared_model_context(&sid).await.unwrap())
            .to_string()
            .contains("INACTIVE_ATTACHMENT_SENTINEL"));
    }
    #[tokio::test]
    async fn participant_context_uses_public_history_without_owner_private_checkpoint() {
        let (_root, store) = fixture().await;
        let (docs, message) = envelope("public", "alice");
        store
            .admit_shared_message(docs, "s", "message", &message)
            .await
            .unwrap();
        let mut session = store.get("session:s").await.unwrap().unwrap();
        session["checkpoint_input"] =
            json!([{"role":"assistant","content":"private-owner-diagnostic-sentinel"}]);
        store.put("session:s", &session).await.unwrap();
        let public = json!(store.shared_model_context("s").await.unwrap()).to_string();
        assert!(public.contains("sentinel-public"));
        assert!(!public.contains("private-owner-diagnostic-sentinel"));
    }
    #[tokio::test]
    async fn inactive_transcript_is_durable_but_never_context_or_recall() {
        let (root, store) = fixture().await;
        let state = store
            .set_participation("s", "alice", false, 0, "off")
            .await
            .unwrap();
        assert_eq!(state["revision"], 1);
        let (docs, message) = envelope("private", "alice");
        let result = store
            .admit_shared_message(docs, "s", "message", &message)
            .await
            .unwrap();
        assert!(result["run_id"].is_null());
        store
            .set_participation("s", "bob", true, 1, "on")
            .await
            .unwrap();
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(
            reopened.chat_page("s", 0, 50).await.unwrap()["messages"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(reopened.context("s").await.unwrap().is_empty());
        assert!(
            reopened.model_chat_page("s", 0, 50).await.unwrap()["messages"][0]["content"].is_null()
        );
        assert!(reopened.get("run:private").await.unwrap().is_none());
    }
    #[tokio::test]
    async fn queue_preserves_authors_and_deactivation_withdraws_unprocessed_text() {
        let (_root, store) = fixture().await;
        let (docs, message) = envelope("first", "alice");
        store
            .admit_shared_message(docs, "s", "message", &message)
            .await
            .unwrap();
        let (docs, message) = envelope("second", "bob");
        assert_eq!(
            store
                .admit_shared_message(docs, "s", "message", &message)
                .await
                .unwrap()["queued"],
            true
        );
        assert!(store.get("run:second").await.unwrap().is_none());
        assert!(!json!(store.context("s").await.unwrap())
            .to_string()
            .contains("second"));
        store
            .set_participation("s", "bob", false, 0, "off")
            .await
            .unwrap();
        store
            .set_participation("s", "alice", true, 1, "on")
            .await
            .unwrap();
        store.drain_message_queues().await.unwrap();
        assert!(store.get("run:second").await.unwrap().is_none());
        assert!(!json!(store.context("s").await.unwrap())
            .to_string()
            .contains("second"));
        assert_eq!(
            store.get("message_meta:second").await.unwrap().unwrap()["author_id"],
            "bob"
        );
        assert_eq!(
            store.get("run:first").await.unwrap().unwrap()["status"],
            "cancelled"
        );
    }
    #[tokio::test]
    async fn preparation_generation_fences_mode_flips_before_admission() {
        let (_root,store)=fixture().await;
        store.set_participation("s","alice",false,0,"off").await.unwrap();
        let (docs,mut human)=envelope("prepared-inactive","alice");human["participation_generation"]=json!(1);
        store.set_participation("s","bob",true,1,"on").await.unwrap();
        assert_eq!(store.admit_shared_message(docs,"s","human_message",&human).await.unwrap_err().to_string(),"Participation revision changed");
        let (docs,mut active)=envelope("prepared-active","bob");active["participation_generation"]=json!(2);
        store.set_participation("s","alice",false,2,"off-again").await.unwrap();store.set_participation("s","bob",true,3,"on-again").await.unwrap();
        assert_eq!(store.admit_shared_message(docs,"s","message",&active).await.unwrap_err().to_string(),"Participation revision changed");
        assert!(store.chat_page("s",0,50).await.unwrap()["messages"].as_array().unwrap().is_empty());
        assert!(store.context("s").await.unwrap().is_empty());
        assert!(store.get("receipt:message:s:alice:prepared-inactive").await.unwrap().is_none());
        assert!(store.get("receipt:message:s:bob:prepared-active").await.unwrap().is_none());
        // Legacy human-only envelopes cannot acquire eligibility merely because active.
        let (docs,human)=envelope("legacy-human","alice");
        assert!(store.admit_shared_message(docs,"s","human_message",&human).await.unwrap()["run_id"].is_null());
        assert!(store.context("s").await.unwrap().is_empty());assert!(store.shared_model_context("s").await.unwrap().is_empty());
    }
    #[tokio::test]
    async fn admitted_unprocessed_message_is_excluded_after_restart_and_reactivation() {
        let (root, store) = fixture().await;
        let (docs, message) = envelope("unprocessed-secret", "alice");
        store
            .admit_shared_message(docs, "s", "message", &message)
            .await
            .unwrap();
        // Prime both assembled cache and checkpoint to exercise invalidation.
        let input = store.context("s").await.unwrap();
        assert!(json!(input).to_string().contains("unprocessed-secret"));
        let mut session = store.get("session:s").await.unwrap().unwrap();
        session["checkpoint_input"] = json!(input);
        session["checkpoint_through"] = json!(1);
        store.put("session:s", &session).await.unwrap();
        store
            .set_participation("s", "bob", false, 0, "off")
            .await
            .unwrap();
        drop(store);
        let store = Store::open(root.path()).await.unwrap();
        store
            .set_participation("s", "alice", true, 1, "on")
            .await
            .unwrap();
        let (docs, message) = envelope("fresh-turn", "bob");
        store
            .admit_shared_message(docs, "s", "message", &message)
            .await
            .unwrap();
        for input in [
            store.context("s").await.unwrap(),
            store.shared_model_context("s").await.unwrap(),
        ] {
            let serialized = json!(input).to_string();
            assert!(serialized.contains("fresh-turn"));
            assert!(!serialized.contains("unprocessed-secret"));
        }
        assert!(json!(store.chat_page("s", 0, 50).await.unwrap())
            .to_string()
            .contains("unprocessed-secret"));
    }
    #[tokio::test]
    async fn state_receipts_and_generation_reject_stale_requests() {
        let (_root, store) = fixture().await;
        let first = store
            .set_participation("s", "alice", false, 0, "off")
            .await
            .unwrap();
        assert_eq!(
            store
                .set_participation("s", "alice", false, 0, "off")
                .await
                .unwrap(),
            first
        );
        assert!(store
            .set_participation("s", "bob", true, 0, "stale")
            .await
            .is_err());
        store
            .set_participation("s", "bob", true, 1, "on")
            .await
            .unwrap();
        assert!(!store
            .participation_allows_run(&json!({"session_id":"s","participation_generation":0}))
            .await
            .unwrap());
    }
}
