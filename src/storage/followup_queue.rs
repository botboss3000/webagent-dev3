//! Durable encrypted followups, serialized against publication in the user file.
use super::*;
fn empty(s: &str) -> Value {
    json!({"schema_version":1,"session_id":s,"revision":0,"held":false,"reason":null,"entries":[]})
}
impl Store {
    pub(super) async fn queue_read_tx(
        &self,
        t: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        s: &str,
    ) -> Result<Value> {
        let r: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(format!("followup_queue:{s}"))
            .fetch_optional(&mut **t)
            .await?;
        let q = r
            .map(|v| self.unseal(&v))
            .transpose()?
            .unwrap_or_else(|| empty(s));
        if q["schema_version"] != 1
            || q["session_id"] != s
            || q["revision"].as_u64().is_none()
            || q["held"].as_bool().is_none()
            || q["entries"].as_array().is_none_or(|entries| {
                entries.iter().any(|e| {
                    e["id"].as_str().is_none()
                        || e["revision"].as_u64().is_none()
                        || e["content"].as_str().is_none()
                        || e["context_content"].as_str().is_none()
                        || e["attachments"].as_array().is_none()
                        || !matches!(
                            e["choice"].as_str(),
                            Some("queue" | "steer" | "interrupt" | "cancel")
                        )
                        || !matches!(
                            e["status"].as_str(),
                            Some(
                                "pending"
                                    | "reviewing"
                                    | "held"
                                    | "delivered"
                                    | "cancelled"
                                    | "rejected"
                            )
                        )
                })
            })
        {
            return Err(anyhow!("Invalid or unsupported followup queue record"));
        }
        Ok(q)
    }
    pub(super) async fn queue_write_tx(
        &self,
        t: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        q: &mut Value,
    ) -> Result<()> {
        q["revision"] = json!(q["revision"].as_u64().unwrap_or(0) + 1);
        let s = q["session_id"].as_str().unwrap();
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").bind(format!("followup_queue:{s}")).bind(self.seal(q)?).execute(&mut **t).await?;
        sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,'queue_updated',?)")
            .bind(s)
            .bind(self.encode_user_record(&json!({"revision":q["revision"]}))?)
            .execute(&mut **t)
            .await?;
        Ok(())
    }
    /// Called inside the session lifecycle transaction, including repeated recycle.
    pub(super) async fn cancel_session_followups(
        &self,
        t: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        s: &str,
    ) -> Result<()> {
        let exists: i64 = sqlx::query_scalar("SELECT count(*) FROM docs WHERE key=?")
            .bind(format!("followup_queue:{s}"))
            .fetch_one(&mut **t)
            .await?;
        if exists == 0 {
            return Ok(());
        }
        let mut q = self.queue_read_tx(t, s).await?;
        let mut changed = q["held"] == true || !q["reason"].is_null();
        for e in q["entries"].as_array_mut().unwrap() {
            if matches!(e["status"].as_str(), Some("pending" | "reviewing" | "held")) {
                e["status"] = json!("cancelled");
                e["choice"] = json!("cancel");
                e["revision"] = json!(e["revision"]
                    .as_u64()
                    .unwrap()
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("Queue entry revision exhausted"))?);
                e["review_status"] = json!("cancelled");
                changed = true;
            }
        }
        if changed {
            q["held"] = json!(false);
            q["reason"] = Value::Null;
            self.queue_write_tx(t, &mut q).await?;
        }
        Ok(())
    }
    async fn queue_reserve(
        &self,
        t: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        s: &str,
    ) -> Result<()> {
        sqlx::query("UPDATE docs SET key=key WHERE key=?")
            .bind(format!("session:{s}"))
            .execute(&mut **t)
            .await?;
        let present: i64 = sqlx::query_scalar("SELECT count(*) FROM docs WHERE key=?")
            .bind(format!("session:{s}"))
            .fetch_one(&mut **t)
            .await?;
        if present == 0 {
            return Err(anyhow!("Session not found"));
        }
        self.require_active_session(t, s).await
    }
    pub(crate) async fn followup_last_run(&self, s: &str) -> Result<Option<Value>> {
        let id: Option<String> = sqlx::query_scalar(
            "SELECT id FROM run_index WHERE session_id=? ORDER BY enqueue_seq DESC LIMIT 1",
        )
        .bind(s)
        .fetch_optional(&self.pool)
        .await?;
        match id {
            Some(id) => self.get(&format!("run:{id}")).await,
            None => Ok(None),
        }
    }
    pub(crate) async fn followup_queue(&self, s: &str) -> Result<Value> {
        let mut t = self.pool.begin().await?;
        let q = self.queue_read_tx(&mut t, s).await?;
        t.commit().await?;
        Ok(q)
    }
    pub(crate) async fn followup_submit(
        &self,
        s: &str,
        e: Value,
        docs: Vec<(String, Value)>,
        request: &str,
        digest: &str,
        result: &Value,
    ) -> Result<Value> {
        let _g = self.context_gate.write().await;
        let mut t = self.pool.begin().await?;
        self.queue_reserve(&mut t, s).await?;
        let key = format!("receipt:message:{s}:{request}");
        if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&key)
            .fetch_optional(&mut *t)
            .await?
        {
            let v = self.unseal(&raw)?;
            if v["digest"] != digest {
                return Err(anyhow!("Message identifier reused with different input"));
            }
            return Ok(v["result"].clone());
        }
        let mut q = self.queue_read_tx(&mut t, s).await?;
        if q["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| {
                !matches!(
                    e["status"].as_str(),
                    Some("delivered" | "cancelled" | "rejected")
                )
            })
            .count()
            >= 256
        {
            return Err(anyhow!("Followup queue is full"));
        }
        for (k, v) in docs {
            self.write_document(&mut t, &k, &v).await?;
        }
        self.insert_chat(&mut t, s, e["id"].as_str().unwrap(), "user", &e["content"])
            .await?;
        q["entries"].as_array_mut().unwrap().push(e);
        self.queue_write_tx(&mut t, &mut q).await?;
        self.write_document(&mut t, &key, &json!({"digest":digest,"result":result}))
            .await?;
        t.commit().await?;
        self.context_cache
            .lock()
            .unwrap()
            .invalidate(&self.context_cache_key(s));
        self.cache
            .lock()
            .unwrap()
            .invalidate(&[format!("session:{s}"), key]);
        Ok(result.clone())
    }
    pub(crate) async fn followup_choice(
        &self,
        s: &str,
        id: Option<&str>,
        expected: u64,
        request: &str,
        body: &Value,
    ) -> Result<Value> {
        let mut t = self.pool.begin().await?;
        self.queue_reserve(&mut t, s).await?;
        let key = format!("receipt:queue:{s}:{request}");
        if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&key)
            .fetch_optional(&mut *t)
            .await?
        {
            let v = self.unseal(&raw)?;
            if v["digest"] != crate::digest(body) {
                return Err(anyhow!("Queue request identifier reused"));
            }
            return Ok(v["result"].clone());
        }
        let mut q = self.queue_read_tx(&mut t, s).await?;
        if let Some(id) = id {
            let e = q["entries"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|e| e["id"] == id)
                .ok_or_else(|| anyhow!("Queued message not found"))?;
            if e["revision"].as_u64().unwrap_or(0) != expected {
                return Err(anyhow!("Queue revision changed"));
            }
            if !matches!(e["status"].as_str(), Some("pending" | "reviewing" | "held"))
                || e["interrupt_committed"] == true
            {
                return Err(anyhow!("Message delivery already claimed"));
            }
            let choice = body["choice"].as_str().unwrap();
            e["choice"] = json!(choice);
            e["revision"] = json!(expected + 1);
            if choice == "cancel" {
                e["status"] = json!("cancelled");
            }
            if choice == "interrupt" {
                e["interrupt_committed"] = json!(true);
                let target:Option<String>=sqlx::query_scalar("SELECT id FROM run_index WHERE session_id=? AND status NOT IN ('completed','cancelled','failed') ORDER BY enqueue_seq DESC LIMIT 1").bind(s).fetch_optional(&mut *t).await?;
                e["target_run_id"] = json!(target);
            }
        } else {
            if q["revision"].as_u64().unwrap_or(0) != expected {
                return Err(anyhow!("Queue revision changed"));
            }
            let busy:i64=sqlx::query_scalar("SELECT count(*) FROM run_index WHERE session_id=? AND status NOT IN ('completed','cancelled','failed')").bind(s).fetch_one(&mut *t).await?;
            if busy != 0 {
                return Err(anyhow!("Resolve existing work before resuming queue"));
            }
            q["held"] = json!(false);
            q["reason"] = Value::Null;
            let last: Option<String> = sqlx::query_scalar(
                "SELECT id FROM run_index WHERE session_id=? ORDER BY enqueue_seq DESC LIMIT 1",
            )
            .bind(s)
            .fetch_optional(&mut *t)
            .await?;
            q["resumed_after_run_id"] = json!(last);
            for e in q["entries"].as_array_mut().unwrap() {
                if e["status"] == "held" && e["review_status"] == "acceptable" {
                    e["status"] = json!("pending");
                }
            }
        }
        self.queue_write_tx(&mut t, &mut q).await?;
        let result = crate::followup_queue::public(&q);
        self.write_document(
            &mut t,
            &key,
            &json!({"digest":crate::digest(body),"result":result}),
        )
        .await?;
        t.commit().await?;
        self.cache.lock().unwrap().invalidate(&[key]);
        Ok(result)
    }
    pub(crate) async fn followup_review_cas(
        &self,
        s: &str,
        id: &str,
        expected: u64,
        status: &str,
        review: Option<&Value>,
    ) -> Result<bool> {
        let mut t = self.pool.begin().await?;
        self.queue_reserve(&mut t, s).await?;
        let mut q = self.queue_read_tx(&mut t, s).await?;
        let Some(e) = q["entries"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|e| e["id"] == id)
        else {
            return Ok(false);
        };
        if e["revision"].as_u64().unwrap_or(0) != expected
            || matches!(
                e["status"].as_str(),
                Some("cancelled" | "delivered" | "rejected")
            )
        {
            return Ok(false);
        }
        e["revision"] = json!(expected + 1);
        e["status"] = json!(status);
        if let Some(r) = review {
            e["review"] = r.clone();
            e["review_status"] = r["status"].clone();
        }
        self.queue_write_tx(&mut t, &mut q).await?;
        t.commit().await?;
        Ok(true)
    }
    pub(crate) async fn followup_hold(&self, s: &str, reason: &str) -> Result<()> {
        let mut t = self.pool.begin().await?;
        self.queue_reserve(&mut t, s).await?;
        let mut q = self.queue_read_tx(&mut t, s).await?;
        if q["held"] == true && q["reason"] == reason {
            return Ok(());
        }
        q["held"] = json!(true);
        q["reason"] = json!(reason);
        self.queue_write_tx(&mut t, &mut q).await?;
        t.commit().await?;
        Ok(())
    }
    pub(crate) async fn followup_publication_guard(
        &self,
        t: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        s: &str,
    ) -> Result<()> {
        sqlx::query("UPDATE docs SET key=key WHERE key=?")
            .bind(format!("session:{s}"))
            .execute(&mut **t)
            .await?;
        let q = self.queue_read_tx(t, s).await?;
        if q["entries"].as_array().unwrap().iter().any(|e| {
            e["status"] == "pending"
                && e["review_status"] == "acceptable"
                && matches!(e["choice"].as_str(), Some("steer" | "interrupt"))
        }) {
            return Err(anyhow!("Pending followup prevents terminal publication"));
        }
        Ok(())
    }
    async fn followup_lease_guard(
        &self,
        t: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        run: &Value,
        previous: &Value,
    ) -> Result<()> {
        if previous["lease_epoch"] != run["lease_epoch"] {
            return Err(anyhow!("Run lease changed"));
        }
        if previous["status"] == "running" && previous["lease_epoch"].as_i64().unwrap_or(0) > 0 {
            let lease = sqlx::query(
                "SELECT lease_epoch,lease_owner,lease_expires_at FROM run_index WHERE id=?",
            )
            .bind(run["id"].as_str().unwrap())
            .fetch_one(&mut **t)
            .await?;
            let epoch: i64 = lease.get("lease_epoch");
            let owner: Option<String> = lease.get("lease_owner");
            let until: i64 = lease.get("lease_expires_at");
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs() as i64;
            if epoch != run["lease_epoch"].as_i64().unwrap_or(0)
                || owner.as_deref() != run["lease_owner"].as_str()
                || until < now
            {
                return Err(anyhow!("Run lease expired or changed"));
            }
        }
        Ok(())
    }
    pub(crate) async fn followup_consume(
        &self,
        s: &str,
        choice: &str,
        run: &mut Value,
        mut origin: Option<&mut Value>,
    ) -> Result<bool> {
        let _g = self.context_gate.write().await;
        let mut t = self.pool.begin().await?;
        self.queue_reserve(&mut t, s).await?;
        let mut q = self.queue_read_tx(&mut t, s).await?;
        if choice == "queue" && q["held"] == true {
            return Ok(false);
        }
        let mut picked = q["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| {
                e["status"] == "pending"
                    && e["review_status"] == "acceptable"
                    && (e["choice"] == choice || (choice == "queue" && e["choice"] == "steer"))
            })
            .cloned()
            .collect::<Vec<_>>();
        if choice == "interrupt" {
            if let Some(parent) = origin.as_deref() {
                picked
                    .retain(|e| e["target_run_id"].is_null() || e["target_run_id"] == parent["id"]);
            }
            picked.truncate(1);
        }
        if picked.is_empty() {
            return Ok(false);
        }
        // The preflight template must still belong to the actual first consumer;
        // a concurrent choice edit causes a fresh scheduler pass, never a wrong model.
        if choice != "steer"
            && picked[0]["template"].is_object()
            && run["selection_plan"] != picked[0]["template"]["selection_plan"]
        {
            return Ok(false);
        }
        let rid = run["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing delivery run"))?
            .to_owned();
        let busy:i64=sqlx::query_scalar("SELECT count(*) FROM run_index WHERE session_id=? AND id!=? AND status NOT IN ('completed','cancelled','failed')").bind(s).bind(&rid).fetch_one(&mut *t).await?;
        if choice == "queue" && busy != 0 {
            return Ok(false);
        }
        let text = picked
            .iter()
            .map(|e| e["context_content"].as_str().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n\n");
        let attachment_count = picked
            .iter()
            .map(|e| e["attachments"].as_array().unwrap().len())
            .sum::<usize>();
        let config = match std::fs::read(self.root.join("config/chat.json")) {
            Ok(bytes) => serde_json::from_slice::<Value>(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
            Err(e) => return Err(e.into()),
        };
        let configured = &config["chat_limits"]["attachments"]["max_per_message"];
        let maximum = if configured.is_null() {
            None
        } else {
            Some(
                configured
                    .as_u64()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| anyhow!("Invalid attachment count limit"))?,
            )
        };
        if text.len() > 65536 || maximum.is_some_and(|max| attachment_count as u64 > max) {
            q["held"] = json!(true);
            q["reason"]=json!("Combined queued input exceeds message or attachment limits; cancel messages to reduce it");
            self.queue_write_tx(&mut t, &mut q).await?;
            t.commit().await?;
            return Ok(false);
        }
        let item = json!({"role":"user","content":text});
        let mut next = run.clone();
        if choice == "steer" || choice == "interrupt" {
            next["input"]
                .as_array_mut()
                .ok_or_else(|| anyhow!("Missing run input"))?
                .push(item.clone());
        }
        let batch = uuid::Uuid::new_v4().to_string();
        next["followup_batch_id"] = json!(batch);
        let mut saved_origin = None;
        if let Some(original) = origin.as_deref() {
            let mut old = original.clone();
            let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
                .bind(format!("run:{}", old["id"].as_str().unwrap()))
                .fetch_one(&mut *t)
                .await?;
            let previous = self.unseal(&raw)?;
            if previous["run_revision"].as_u64().unwrap_or(0)
                != old["run_revision"].as_u64().unwrap_or(0)
                || previous["generation"] != old["generation"]
            {
                return Err(anyhow!("Run fence rejected interrupt handoff"));
            }
            old["status"] = json!("cancelled");
            old["generation"] = json!(uuid::Uuid::new_v4().to_string());
            old["interrupted_by"] = picked[0]["id"].clone();
            old["run_revision"] = json!(old["run_revision"].as_u64().unwrap_or(0) + 1);
            self.followup_lease_guard(&mut t, original, &previous)
                .await?;
            self.write_document(
                &mut t,
                &format!("run:{}", old["id"].as_str().unwrap()),
                &old,
            )
            .await?;
            self.index_run(&mut t, &old).await?;
            for item in old["input"]
                .as_array()
                .into_iter()
                .flatten()
                // Input includes older runs' history. Only this interrupted
                // call list can have newly synthesized results to journal here.
                .filter(|i| {
                    i["type"] == "function_call_output"
                        && old["calls"].as_array().is_some_and(|calls| {
                            calls.iter().any(|call| call["call_id"] == i["call_id"])
                        })
                })
            {
                if let Some(call) = item["call_id"].as_str() {
                    self.insert_interaction(
                        &mut t,
                        s,
                        old["id"].as_str().unwrap(),
                        &format!("tool:{call}"),
                        item,
                    )
                    .await?;
                }
            }
            for entry in q["entries"].as_array_mut().unwrap() {
                if entry["choice"] == "interrupt"
                    && entry["status"] == "pending"
                    && entry["target_run_id"] == old["id"]
                {
                    entry["target_run_id"] = json!(rid);
                    entry["revision"] = json!(entry["revision"].as_u64().unwrap_or(0) + 1);
                }
            }
            saved_origin = Some(old);
        }
        if choice == "steer" {
            let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
                .bind(format!("run:{rid}"))
                .fetch_one(&mut *t)
                .await?;
            let p = self.unseal(&raw)?;
            self.followup_lease_guard(&mut t, run, &p).await?;
            if p["generation"] != run["generation"]
                || p["run_revision"].as_u64().unwrap_or(0)
                    != run["run_revision"].as_u64().unwrap_or(0)
                || p["status"] != "running"
            {
                return Err(anyhow!("Run fence rejected steering"));
            }
        }
        next["run_revision"] = json!(next["run_revision"].as_u64().unwrap_or(0) + 1);
        next["attachments"] = json!(picked
            .iter()
            .flat_map(|e| e["attachments"].as_array().into_iter().flatten().cloned())
            .collect::<Vec<_>>());
        self.write_document(&mut t, &format!("run:{rid}"), &next)
            .await?;
        self.index_run(&mut t, &next).await?;
        self.insert_interaction(&mut t, s, &rid, &format!("followup:{batch}"), &item)
            .await?;
        for e in q["entries"].as_array_mut().unwrap() {
            if picked.iter().any(|p| p["id"] == e["id"]) {
                e["status"] = json!("delivered");
                e["consuming_run_id"] = json!(rid);
                e["batch_id"] = json!(batch);
                e["revision"] = json!(e["revision"].as_u64().unwrap_or(0) + 1);
            }
        }
        self.queue_write_tx(&mut t, &mut q).await?;
        t.commit().await?;
        *run = next;
        if let (Some(target), Some(saved)) = (origin.as_deref_mut(), saved_origin) {
            let old_id = saved["id"].as_str().unwrap().to_owned();
            *target = saved;
            self.cache
                .lock()
                .unwrap()
                .invalidate(&[format!("run:{old_id}"), format!("run_view:{old_id}")]);
        }
        self.context_cache
            .lock()
            .unwrap()
            .invalidate(&self.context_cache_key(s));
        self.cache
            .lock()
            .unwrap()
            .invalidate(&[format!("run:{rid}"), format!("run_view:{rid}")]);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn fixture() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        let s = Store::open(root.path()).await.unwrap();
        s.put(
            "session:s",
            &json!({"id":"s","agent_id":"shared_default","status":"active"}),
        )
        .await
        .unwrap();
        (root, s)
    }
    fn entry(id: &str, text: &str, choice: &str) -> Value {
        json!({"id":id,"message_id":format!("user:{id}"),"session_id":"s","agent_id":"shared_default","revision":0,"status":"pending","review_status":"acceptable","choice":choice,"content":text,"context_content":text,"attachments":[]})
    }
    async fn submit(s: &Store, id: &str, text: &str, choice: &str) {
        s.followup_submit(
            "s",
            entry(id, text, choice),
            vec![],
            id,
            id,
            &json!({"submission_id":id}),
        )
        .await
        .unwrap();
    }
    fn run(id: &str) -> Value {
        json!({"id":id,"session_id":"s","status":"queued","generation":"","input":[],"updated_at":1,"run_revision":0})
    }
    #[tokio::test]
    async fn combined_delivery_survives_reopen_without_duplicate_bubbles() {
        let (root, s) = fixture().await;
        submit(&s, "a", "first", "queue").await;
        submit(&s, "b", "second", "queue").await;
        let reopened = Store::open(root.path()).await.unwrap();
        let mut r = run("batch");
        assert!(reopened
            .followup_consume("s", "queue", &mut r, None)
            .await
            .unwrap());
        assert!(!reopened
            .followup_consume("s", "queue", &mut r, None)
            .await
            .unwrap());
        let context = reopened.context("s").await.unwrap();
        assert_eq!(context.len(), 1);
        assert_eq!(context[0]["content"], "first\n\nsecond");
        let (chat, _) = reopened.chat_snapshot("s").await.unwrap();
        assert_eq!(chat.len(), 2);
        let q = reopened.followup_queue("s").await.unwrap();
        assert!(q["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["consuming_run_id"] == "batch" && e["status"] == "delivered"));
    }
    #[tokio::test]
    async fn cancellation_cas_retry_and_request_conflicts() {
        let (_root, s) = fixture().await;
        submit(&s, "a", "first", "queue").await;
        let b = json!({"choice":"cancel"});
        let first = s
            .followup_choice("s", Some("a"), 0, "request", &b)
            .await
            .unwrap();
        assert_eq!(
            s.followup_choice("s", Some("a"), 0, "request", &b)
                .await
                .unwrap(),
            first
        );
        assert!(s
            .followup_choice("s", Some("a"), 0, "different", &json!({"choice":"steer"}))
            .await
            .is_err());
        assert!(s
            .followup_choice("s", Some("a"), 0, "request", &json!({"choice":"queue"}))
            .await
            .is_err());
        let mut r = run("batch");
        assert!(!s
            .followup_consume("s", "queue", &mut r, None)
            .await
            .unwrap());
        assert!(s.context("s").await.unwrap().is_empty());
    }
    #[tokio::test]
    async fn steering_fences_terminal_and_preserves_private_input() {
        let (_root, s) = fixture().await;
        let mut r = run("active");
        r["status"] = json!("running");
        s.put("run:active", &r).await.unwrap();
        submit(&s, "a", "steer", "steer").await;
        let mut completed = r.clone();
        completed["status"] = json!("completed");
        assert!(s
            .save_run(
                &mut completed,
                "model_output",
                &json!({"terminal_text":"late","output":[]})
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("Pending followup"));
        r["input"] = json!([{"role":"assistant","content":"private work"}]);
        assert!(s
            .followup_consume("s", "steer", &mut r, None)
            .await
            .unwrap());
        assert_eq!(r["input"][0]["content"], "private work");
        assert_eq!(r["input"][1]["content"], "steer");
        assert!(!s
            .followup_consume("s", "steer", &mut r, None)
            .await
            .unwrap());
    }
    #[tokio::test]
    async fn interrupt_only_selected_then_steer_and_resume_hold() {
        let (_root, s) = fixture().await;
        let mut old = run("old");
        s.put("run:old", &old).await.unwrap();
        submit(&s, "a", "interrupt", "interrupt").await;
        submit(&s, "b", "steer", "steer").await;
        submit(&s, "c", "later", "queue").await;
        let mut next = run("replacement");
        next["input"] = json!([{"role":"assistant","content":"retained"}]);
        assert!(s
            .followup_consume("s", "interrupt", &mut next, Some(&mut old))
            .await
            .unwrap());
        assert_eq!(old["status"], "cancelled");
        assert_eq!(next["input"][1]["content"], "interrupt");
        let q = s.followup_queue("s").await.unwrap();
        assert_eq!(q["entries"][1]["status"], "pending");
        assert_eq!(q["entries"][2]["status"], "pending");
        s.followup_hold("s", "Stopped").await.unwrap();
        assert!(s
            .followup_choice(
                "s",
                None,
                q["revision"].as_u64().unwrap(),
                "resume",
                &json!({})
            )
            .await
            .is_err());
    }
    #[tokio::test]
    async fn review_cancellation_wins_and_queue_is_encrypted() {
        let (root, s) = fixture().await;
        submit(&s, "a", "private fixture", "queue").await;
        s.followup_choice("s", Some("a"), 0, "cancel", &json!({"choice":"cancel"}))
            .await
            .unwrap();
        assert!(!s
            .followup_review_cas(
                "s",
                "a",
                1,
                "pending",
                Some(&json!({"status":"acceptable"}))
            )
            .await
            .unwrap());
        let raw: Vec<u8> =
            sqlx::query_scalar("SELECT value FROM docs WHERE key='followup_queue:s'")
                .fetch_one(&s.pool)
                .await
                .unwrap();
        assert!(serde_json::from_slice::<Value>(&raw).is_err());
        assert_eq!(
            Store::open(root.path())
                .await
                .unwrap()
                .followup_queue("s")
                .await
                .unwrap()["entries"][0]["status"],
            "cancelled"
        );
    }
    #[tokio::test]
    async fn multiple_interrupts_retarget_and_consume_separately() {
        let (_root, s) = fixture().await;
        let mut old = run("old");
        s.put("run:old", &old).await.unwrap();
        submit(&s, "a", "one", "queue").await;
        submit(&s, "b", "two", "queue").await;
        s.followup_choice("s", Some("a"), 0, "i1", &json!({"choice":"interrupt"}))
            .await
            .unwrap();
        s.followup_choice("s", Some("b"), 0, "i2", &json!({"choice":"interrupt"}))
            .await
            .unwrap();
        let mut first = run("first");
        assert!(s
            .followup_consume("s", "interrupt", &mut first, Some(&mut old))
            .await
            .unwrap());
        let q = s.followup_queue("s").await.unwrap();
        assert_eq!(q["entries"][1]["target_run_id"], "first");
        assert_eq!(q["entries"][1]["status"], "pending");
        let mut second = run("second");
        second["input"] = first["input"].clone();
        assert!(s
            .followup_consume("s", "interrupt", &mut second, Some(&mut first))
            .await
            .unwrap());
        assert_eq!(second["input"].as_array().unwrap().len(), 2);
        assert_eq!(second["input"][1]["content"], "two");
        assert_eq!(s.context("s").await.unwrap().len(), 2);
    }
    #[tokio::test]
    async fn resume_records_terminal_parent_and_lease_takeover_rejects_steering() {
        let (_root, s) = fixture().await;
        let mut stopped = run("stopped");
        stopped["status"] = json!("cancelled");
        s.put("run:stopped", &stopped).await.unwrap();
        submit(&s, "a", "later", "queue").await;
        s.followup_hold("s", "Stopped").await.unwrap();
        let q = s.followup_queue("s").await.unwrap();
        s.followup_choice(
            "s",
            None,
            q["revision"].as_u64().unwrap(),
            "resume",
            &json!({}),
        )
        .await
        .unwrap();
        let q = s.followup_queue("s").await.unwrap();
        assert_eq!(q["held"], false);
        assert_eq!(q["resumed_after_run_id"], "stopped");
        let mut live = run("live");
        live["status"] = json!("running");
        live["lease_epoch"] = json!(1);
        live["lease_owner"] = json!("loop-one");
        s.put("run:live", &live).await.unwrap();
        submit(&s, "b", "steer", "steer").await;
        sqlx::query("UPDATE run_index SET lease_epoch=2,lease_owner='loop-two',lease_expires_at=strftime('%s','now')+30 WHERE id='live'").execute(&s.pool).await.unwrap();
        assert!(s
            .followup_consume("s", "steer", &mut live, None)
            .await
            .is_err());
        assert_eq!(
            s.followup_queue("s").await.unwrap()["entries"][1]["status"],
            "pending"
        );
    }
    #[tokio::test]
    async fn abandoned_handoff_does_not_rejournal_historical_tool_results() {
        let (_root, s) = fixture().await;
        let historical_call = json!({"type":"function_call","call_id":"historical-call","name":"file_read","arguments":"{}"});
        let historical_result = json!({"type":"function_call_output","call_id":"historical-call","output":"historical result"});
        let mut historical = run("historical-run");
        historical["status"] = json!("running");
        historical["turns"] = json!(1);
        historical["input"] = json!([historical_call.clone()]);
        s.put("run:historical-run", &historical).await.unwrap();
        s.save_run(
            &mut historical,
            "model_output",
            &json!({"output":[historical_call]}),
        )
        .await
        .unwrap();
        historical["input"]
            .as_array_mut()
            .unwrap()
            .push(historical_result);
        historical["status"] = json!("failed");
        s.save_run(&mut historical, "tool_result", &json!({}))
            .await
            .unwrap();
        let current_call = json!({"type":"function_call","call_id":"current-write","name":"file_write","arguments":"{}"});
        let mut old = run("current-run");
        old["status"] = json!("running");
        old["turns"] = json!(1);
        old["input"] = json!(s.context("s").await.unwrap());
        old["input"]
            .as_array_mut()
            .unwrap()
            .push(current_call.clone());
        old["calls"] = json!([current_call.clone()]);
        old["pending"] = json!({"name":"file_write","call_id":"current-write","id":"invocation","state":"executing"});
        s.put("run:current-run", &old).await.unwrap();
        s.save_run(&mut old, "model_output", &json!({"output":[current_call]}))
            .await
            .unwrap();
        old["status"] = json!("cancelled");
        old["recovery_abandoned"] = json!(true);
        old["uncertain"] = json!(false);
        s.save_run(&mut old, "run_status", &json!({}))
            .await
            .unwrap();
        let mut e = entry("interrupt-entry", "inspect existing effects", "interrupt");
        e["target_run_id"] = json!("current-run");
        s.followup_submit("s", e, vec![], "interrupt-entry", "digest", &json!({}))
            .await
            .unwrap();
        crate::agent_loop::settle_abandoned_interrupt(&mut old).unwrap();
        let mut next = run("replacement");
        next["input"] = old["input"].clone();
        assert!(s
            .followup_consume("s", "interrupt", &mut next, Some(&mut old))
            .await
            .unwrap());
        let context = s.context("s").await.unwrap();
        assert_eq!(context.len(), 5);
        assert_eq!(
            context
                .iter()
                .filter(|item| item["type"] == "function_call_output"
                    && item["call_id"] == "historical-call")
                .count(),
            1
        );
        assert_eq!(context[0]["call_id"], "historical-call");
        assert_eq!(context[1]["call_id"], "historical-call");
        assert_eq!(context[2]["call_id"], "current-write");
        assert_eq!(context[3]["call_id"], "current-write");
        let result: Value = serde_json::from_str(context[3]["output"].as_str().unwrap()).unwrap();
        assert_eq!(result["execution_state"], "outcome_unknown");
        assert_eq!(result["no_retry"], true);
        assert_eq!(context[4]["content"], "inspect existing effects");
    }

    #[tokio::test]
    async fn followup_queue_lifecycle_cancels_choices_and_purges_receipts() {
        let (root, s) = fixture().await;
        s.session_read_summary("s").await.unwrap();
        for choice in ["queue", "steer", "interrupt"] {
            submit(&s, choice, choice, choice).await;
        }
        submit(&s, "review", "pending review", "queue").await;
        assert!(s
            .followup_review_cas("s", "review", 0, "reviewing", None)
            .await
            .unwrap());
        s.followup_hold("s", "Run stopped").await.unwrap();
        s.put("message_attachments:queue", &json!([{"id":"attachment"}]))
            .await
            .unwrap();
        s.followup_choice("s", Some("steer"), 0, "choose", &json!({"choice":"steer"}))
            .await
            .unwrap();
        let metadata = s.session_read_summary("s").await.unwrap();
        let revision = metadata["metadata_revision"].as_u64().unwrap();
        let body = crate::store::session::SessionLifecycle {
            client_request_id: "delete".into(),
            expected_revision: revision,
        };
        assert!(matches!(
            s.lifecycle_session("s", &body, false).await.unwrap(),
            crate::store::session::SessionPatchOutcome::Saved(_)
        ));
        let reopened = Store::open(root.path()).await.unwrap();
        let q = reopened.followup_queue("s").await.unwrap();
        assert_eq!(q["held"], false);
        assert!(q["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["status"] == "cancelled" && e["choice"] == "cancel"));
        let body = crate::store::session::SessionLifecycle {
            client_request_id: "restore".into(),
            expected_revision: revision + 1,
        };
        reopened.lifecycle_session("s", &body, true).await.unwrap();
        assert!(!reopened
            .followup_review_cas(
                "s",
                "review",
                1,
                "pending",
                Some(&json!({"status":"acceptable"}))
            )
            .await
            .unwrap());
        assert!(reopened
            .followup_choice(
                "s",
                Some("queue"),
                0,
                "late-choice",
                &json!({"choice":"interrupt"})
            )
            .await
            .is_err());
        let mut next = run("must-not-replay");
        assert!(!reopened
            .followup_consume("s", "queue", &mut next, None)
            .await
            .unwrap());
        assert!(reopened.context("s").await.unwrap().is_empty());
        let body = crate::store::session::SessionLifecycle {
            client_request_id: "delete-again".into(),
            expected_revision: revision + 2,
        };
        reopened.lifecycle_session("s", &body, false).await.unwrap();
        let body = crate::store::session::SessionLifecycle {
            client_request_id: "purge".into(),
            expected_revision: revision + 3,
        };
        reopened.purge_session("s", &body).await.unwrap();
        assert!(reopened
            .get("receipt:queue:s:choose")
            .await
            .unwrap()
            .is_none());
        assert!(reopened
            .get("message_attachments:queue")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn malformed_queue_fails_closed() {
        let (_root, s) = fixture().await;
        s.put(
            "followup_queue:s",
            &json!({"schema_version":2,"session_id":"s","entries":null}),
        )
        .await
        .unwrap();
        assert!(s
            .followup_queue("s")
            .await
            .unwrap_err()
            .to_string()
            .contains("unsupported"));
    }
}
