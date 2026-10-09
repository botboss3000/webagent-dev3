//! Session-owned encrypted naming state; claims fence manual edits and late results.
use super::*;
type Tx<'a> = sqlx::Transaction<'a, sqlx::Sqlite>;
impl Store {
    async fn naming_session(&self, tx: &mut Tx<'_>, sid: &str) -> Result<Option<Value>> {
        let key = format!("session:{sid}");
        sqlx::query("UPDATE docs SET key=key WHERE key=?")
            .bind(&key)
            .execute(&mut **tx)
            .await?;
        let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(key)
            .fetch_optional(&mut **tx)
            .await?;
        raw.map(|raw| self.unseal(&raw)).transpose()
    }
    async fn naming_save(
        &self,
        tx: &mut Tx<'_>,
        sid: &str,
        session: &mut Value,
        event: bool,
    ) -> Result<()> {
        if event {
            session["metadata_revision"] = json!(session["metadata_revision"]
                .as_u64()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| anyhow!("Session revision exhausted"))?);
        }
        self.write_document(tx, &format!("session:{sid}"), session)
            .await?;
        if event {
            let summary = self.read_summary_in(tx, sid, session).await?;
            sqlx::query(
                "INSERT INTO events(session_id,kind,payload) VALUES(?,'session_updated',?)",
            )
            .bind(sid)
            .bind(self.encode_user_record(&json!({"session":summary}))?)
            .execute(&mut **tx)
            .await?;
        }
        Ok(())
    }
    pub(super) async fn begin_session_naming(
        &self,
        tx: &mut Tx<'_>,
        sid: &str,
        rid: &str,
        message: &Value,
    ) -> Result<()> {
        let Some(mut session) = self.naming_session(tx, sid).await? else {
            return Ok(());
        };
        if session["auto_naming"]["stage"] != "awaiting_message" {
            return Ok(());
        }
        let text = message["content"].as_str().unwrap_or("");
        session["title"] = json!(crate::session_naming::fallback(text));
        let source = if text.trim().is_empty() {
            message["context_content"].as_str().unwrap_or("Attachments")
        } else {
            text
        };
        session["auto_naming"] = json!({"version":1,"stage":"waiting_turn","run_id":rid,
            "source":source.trim().chars().take(600).collect::<String>(),"attempts":0,"lease_until":0});
        self.naming_save(tx, sid, &mut session, true).await
    }
    pub(super) async fn queue_session_naming(
        &self,
        tx: &mut Tx<'_>,
        sid: &str,
        run: &Value,
    ) -> Result<()> {
        let Some(mut session) = self.naming_session(tx, sid).await? else {
            return Ok(());
        };
        if session["auto_naming"]["stage"] != "waiting_turn" {
            return Ok(());
        }
        // A later successful turn can trigger a failed first run's naming, but
        // source and System plan still belong to the first accepted message.
        let rid = session["auto_naming"]["run_id"].as_str().unwrap_or("");
        let first = if run["id"] == rid {
            run.clone()
        } else {
            let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
                .bind(format!("run:{rid}"))
                .fetch_optional(&mut **tx)
                .await?
            else {
                return Ok(());
            };
            self.unseal(&raw)?
        };
        let plan = first["system_selection_plan"].clone();
        session["auto_naming"]["stage"] = json!(if plan.is_null() { "skipped" } else { "queued" });
        session["auto_naming"]["system_plan"] = plan;
        session["auto_naming"]["queued_at"] = json!(crate::now());
        if !session["auto_naming"]["system_plan"].is_null() {
            // Sparse encrypted queue markers keep discovery scoped to pending
            // jobs, rather than decrypting completed session checkpoints.
            sqlx::query("INSERT OR IGNORE INTO docs(key,value) VALUES(?,?)")
                .bind(format!("session_naming:{sid}"))
                .bind(self.encode_user_record(&json!({"version":1}))?)
                .execute(&mut **tx)
                .await?;
        }
        self.naming_save(tx, sid, &mut session, true).await
    }
    pub(crate) async fn naming_candidates(&self, recovery: bool) -> Result<Vec<String>> {
        let rows = sqlx::query("SELECT s.value FROM docs AS job JOIN docs AS s ON s.key='session:' || substr(job.key,16) WHERE job.key LIKE 'session_naming:%' ORDER BY job.key")
            .fetch_all(&self.pool)
            .await?;
        let mut result = Vec::new();
        for row in rows {
            let session = self.unseal(&row.get::<Vec<u8>, _>("value"))?;
            if naming_due(&session, recovery) {
                if let Some(sid) = session["id"].as_str() {
                    result.push(sid.to_owned());
                }
            }
        }
        Ok(result)
    }
    pub(crate) async fn naming_claim(&self, sid: &str, recovery: bool) -> Result<Option<Value>> {
        let _guard = self.context_gate.write().await;
        let mut tx = self.pool.begin().await?;
        let Some(mut session) = self.naming_session(&mut tx, sid).await? else {
            return Ok(None);
        };
        if !naming_due(&session, recovery) {
            return Ok(None);
        }
        let attempts = session["auto_naming"]["attempts"]
            .as_u64()
            .unwrap_or(0)
            .saturating_add(1);
        session["auto_naming"]["stage"] = json!("generating");
        session["auto_naming"]["claim"] = json!(crate::uuid());
        session["auto_naming"]["lease_until"] = json!(crate::now() + 30);
        session["auto_naming"]["last_attempt_at"] = json!(crate::now());
        session["auto_naming"]["attempts"] = json!(attempts);
        self.naming_save(&mut tx, sid, &mut session, true).await?;
        tx.commit().await?;
        self.cache
            .lock()
            .unwrap()
            .invalidate(&[format!("session:{sid}")]);
        Ok(Some(session))
    }
    pub(crate) async fn naming_owned(&self, sid: &str, claim: &str) -> Result<bool> {
        Ok(self
            .get(&format!("session:{sid}"))
            .await?
            .is_some_and(|session| naming_owned(&session, claim)))
    }
    pub(crate) async fn naming_renew(&self, sid: &str, claim: &str) -> Result<bool> {
        let _guard = self.context_gate.write().await;
        let mut tx = self.pool.begin().await?;
        let Some(mut session) = self.naming_session(&mut tx, sid).await? else {
            return Ok(false);
        };
        if !naming_owned(&session, claim) {
            return Ok(false);
        }
        session["auto_naming"]["lease_until"] = json!(crate::now() + 30);
        self.naming_save(&mut tx, sid, &mut session, false).await?;
        tx.commit().await?;
        self.cache
            .lock()
            .unwrap()
            .invalidate(&[format!("session:{sid}")]);
        Ok(true)
    }
    pub(crate) async fn naming_finish(
        &self,
        sid: &str,
        claim: &str,
        title: Option<&str>,
    ) -> Result<bool> {
        if let Some(title) = title {
            if title.trim().is_empty() || title.len() > 200 || title.chars().any(char::is_control) {
                return Err(anyhow!("Invalid generated title"));
            }
        }
        let _guard = self.context_gate.write().await;
        let mut tx = self.pool.begin().await?;
        let Some(mut session) = self.naming_session(&mut tx, sid).await? else {
            return Ok(false);
        };
        if !naming_owned(&session, claim) {
            return Ok(false);
        }
        session["auto_naming"]["stage"] = json!(if title.is_some() {
            "completed"
        } else {
            "failed"
        });
        session["auto_naming"]["claim"] = Value::Null;
        session["auto_naming"]["lease_until"] = json!(0);
        if let Some(title) = title {
            session["title"] = json!(title);
            session["auto_naming"]["last_error"] = Value::Null;
            session["auto_naming"]["source"] = Value::Null;
            sqlx::query("DELETE FROM docs WHERE key=?")
                .bind(format!("session_naming:{sid}"))
                .execute(&mut *tx)
                .await?;
        } else {
            // Fixed diagnostic label only; never provider payloads or input.
            session["auto_naming"]["last_error"] = json!("Title generation unavailable");
            session["auto_naming"]["retry_at"] = json!(crate::now() + 60);
        }
        self.naming_save(&mut tx, sid, &mut session, true).await?;
        tx.commit().await?;
        self.cache
            .lock()
            .unwrap()
            .invalidate(&[format!("session:{sid}")]);
        Ok(true)
    }
}
fn naming_due(session: &Value, recovery: bool) -> bool {
    if session["status"] != "active" {
        return false;
    }
    let naming = &session["auto_naming"];
    match naming["stage"].as_str() {
        Some("queued") => true,
        Some("failed") => {
            recovery && naming["retry_at"].as_u64().unwrap_or(u64::MAX) <= crate::now()
        }
        Some("generating") => {
            recovery && naming["lease_until"].as_u64().unwrap_or(u64::MAX) <= crate::now()
        }
        _ => false,
    }
}
fn naming_owned(session: &Value, claim: &str) -> bool {
    session["status"] == "active"
        && session["auto_naming"]["stage"] == "generating"
        && session["auto_naming"]["claim"] == claim
        && session["auto_naming"]["lease_until"].as_u64().unwrap_or(0) > crate::now()
}
