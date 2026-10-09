//! Durable review transitions. Only acceptance creates model context and an agent run.
use super::*;
impl Store {
    pub(crate) async fn claim_manager_review(&self, id: &str) -> Result<Option<Value>> {
        let key = format!("manager_review:{id}");
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE docs SET value=value WHERE key=?")
            .bind(&key)
            .execute(&mut *tx)
            .await?;
        let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&key)
            .fetch_optional(&mut *tx)
            .await?
        else {
            return Ok(None);
        };
        let mut record = self.unseal(&raw)?;
        if record["status"] != "pending_review" {
            return Ok(None);
        }
        record["status"] = json!("reviewing");
        record["generation"] = json!(uuid::Uuid::new_v4().to_string());
        record["deadline"] = json!(crate::now() + 50);
        self.write_document(&mut tx, &key, &record).await?;
        tx.commit().await?;
        self.cache.lock().unwrap().invalidate(&[key]);
        Ok(Some(record))
    }
    pub(crate) async fn finish_manager_review(
        &self,
        observed: &Value,
        state: &str,
        verdict: Option<crate::manager::Verdict>,
    ) -> Result<bool> {
        if !matches!(state, "acceptable" | "unacceptable" | "review_error") {
            return Err(anyhow!("Invalid manager state"));
        }
        let id = observed["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Review ID missing"))?;
        let sid = observed["session_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Review session missing"))?;
        let key = format!("manager_review:{id}");
        let pointer = format!("manager_pending:{sid}");
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE docs SET value=value WHERE key=?")
            .bind(&key)
            .execute(&mut *tx)
            .await?;
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(&key)
            .fetch_one(&mut *tx)
            .await?;
        let mut record = self.unseal(&raw)?;
        if record["status"] != "reviewing" || record["generation"] != observed["generation"] {
            return Ok(false);
        }
        let pending: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(&pointer)
            .fetch_optional(&mut *tx)
            .await?;
        if pending
            .as_ref()
            .map(|v| self.unseal(v))
            .transpose()?
            .is_none_or(|v| v["id"] != id)
        {
            return Ok(false);
        }
        if (state == "unacceptable" && verdict != Some(crate::manager::Verdict::Unacceptable))
            || (state == "review_error" && verdict.is_some())
        {
            return Err(anyhow!("Review state and verdict disagree"));
        }
        let acceptable = state == "acceptable";
        if acceptable {
            if verdict != Some(crate::manager::Verdict::Acceptable) {
                return Err(anyhow!("Acceptable verdict required"));
            }
            if record["deadline"].as_u64().unwrap_or(0) <= crate::now() {
                return Err(anyhow!("Manager review lease expired"));
            }
            self.require_active_session(&mut tx, sid).await?;
            let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM run_index WHERE session_id=? AND status NOT IN ('completed','cancelled','failed')")
                .bind(sid).fetch_one(&mut *tx).await?;
            if count != 0 {
                return Err(anyhow!("Session already has unresolved work"));
            }
            let mut run = record["run"].clone();
            run["run_revision"] = json!(1);
            self.write_document(&mut tx, &format!("run:{id}"), &run)
                .await?;
            self.index_run(&mut tx, &run).await?;
            let message = &record["message"];
            // Naming begins only after the manager admits the message.
            self.begin_session_naming(&mut tx, sid, id, message).await?;
            let text = message["context_content"]
                .as_str()
                .or_else(|| message["content"].as_str())
                .ok_or_else(|| anyhow!("Review message missing"))?;
            self.insert_interaction(
                &mut tx,
                sid,
                id,
                "user",
                &json!({"role":"user","content":text}),
            )
            .await?;
            sqlx::query("INSERT INTO loop_commands(command_id,run_id,schema_version,kind,payload,created_at) VALUES(?,?,1,'message',?,strftime('%s','now'))")
                .bind(format!("manager:{id}")).bind(id).bind(self.encode_user_record(&json!({"schema_version":1,"run_id":id,"kind":"message","status":"queued"}))?)
                .execute(&mut *tx).await?;
        }
        record["status"] = json!(state);
        record["verdict"] = json!(verdict);
        record["error"] = if state == "review_error" {
            json!("Review could not be completed. This message was not sent to the agent; you may submit a new message.")
        } else {
            Value::Null
        };
        record["usage"] = observed["usage"].clone();
        record["updated_at"] = json!(crate::now());
        // Retain submission and criteria as private audit evidence; public views expose only status.
        self.write_document(&mut tx, &key, &record).await?;
        sqlx::query("DELETE FROM docs WHERE key=?")
            .bind(&pointer)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,'manager_verdict',?)")
            .bind(sid)
            .bind(self.encode_user_record(&crate::manager::public(&record))?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.cache.lock().unwrap().invalidate(&[
            key,
            pointer,
            format!("run:{id}"),
            format!("run_view:{id}"),
            format!("session:{sid}"),
        ]);
        Ok(true)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    async fn submitted(store: &Store, sid: &str, id: &str) -> Value {
        store
            .put(
                &format!("session:{sid}"),
                &json!({"id":sid,"agent_id":"shared_default","status":"active"}),
            )
            .await
            .unwrap();
        let run = json!({"id":id,"session_id":sid,"agent_id":"shared_default","status":"queued","generation":"","updated_at":0});
        let message = json!({"role":"user","content":"Rejected coding question","run_id":id});
        let record =
            json!({"id":id,"session_id":sid,"status":"pending_review","run":run,"message":message});
        store
            .atomic_docs_event(
                vec![
                    (format!("manager_review:{id}"), record),
                    (format!("manager_pending:{sid}"), json!({"id":id})),
                    (
                        format!("receipt:message:{sid}:{id}"),
                        json!({"digest":"fixture"}),
                    ),
                ],
                sid,
                "manager_submission",
                &message,
            )
            .await
            .unwrap();
        store.claim_manager_review(id).await.unwrap().unwrap()
    }
    #[tokio::test]
    async fn rejected_message_is_visible_but_never_context_or_run() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        let record = submitted(&store, "session", "review").await;
        assert!(store.context("session").await.unwrap().is_empty());
        assert!(store.active_runs().await.unwrap().is_empty());
        assert!(store
            .claim_manager_review("review")
            .await
            .unwrap()
            .is_none());
        assert!(store
            .finish_manager_review(
                &record,
                "unacceptable",
                Some(crate::manager::Verdict::Unacceptable)
            )
            .await
            .unwrap());
        assert!(!store
            .finish_manager_review(
                &record,
                "acceptable",
                Some(crate::manager::Verdict::Acceptable)
            )
            .await
            .unwrap());
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(reopened.chat_snapshot("session").await.unwrap().0.len(), 1);
        assert!(reopened.context("session").await.unwrap().is_empty());
        assert!(reopened.get("run:review").await.unwrap().is_none());
        assert!(reopened
            .get("manager_pending:session")
            .await
            .unwrap()
            .is_none());
        // A later admitted turn cannot pull rejected text into its context.
        reopened
            .atomic_docs_event(
                vec![],
                "session",
                "message",
                &json!({"role":"user","run_id":"next","content":"What sizes do you have?"}),
            )
            .await
            .unwrap();
        let input = reopened.context("session").await.unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["content"], "What sizes do you have?");
    }
    #[tokio::test]
    async fn acceptance_is_atomic_fenced_and_exactly_once() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        let record = submitted(&store, "session", "review").await;
        let mut stale = record.clone();
        stale["generation"] = json!("stale");
        assert!(!store
            .finish_manager_review(
                &stale,
                "acceptable",
                Some(crate::manager::Verdict::Acceptable)
            )
            .await
            .unwrap());
        assert!(store
            .finish_manager_review(
                &record,
                "acceptable",
                Some(crate::manager::Verdict::Acceptable)
            )
            .await
            .unwrap());
        assert!(!store
            .finish_manager_review(
                &record,
                "acceptable",
                Some(crate::manager::Verdict::Acceptable)
            )
            .await
            .unwrap());
        assert_eq!(store.chat_snapshot("session").await.unwrap().0.len(), 1);
        assert_eq!(store.context("session").await.unwrap().len(), 1);
        assert_eq!(store.active_runs().await.unwrap().len(), 1);
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM loop_commands WHERE command_id='manager:review'",
        )
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
    }
    #[tokio::test]
    async fn review_error_does_not_admit_and_pending_blocks_recycle() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        let record = submitted(&store, "session", "review").await;
        assert!(store
            .finish_manager_review(&record, "review_error", None)
            .await
            .unwrap());
        assert!(store.context("session").await.unwrap().is_empty());
        assert!(store.get("run:review").await.unwrap().is_none());
    }
}
