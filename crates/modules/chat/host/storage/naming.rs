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
        if !matches!(session["auto_naming"]["stage"].as_str(),Some("awaiting_message"|"cancelled")) || session["participation"]["active"]==false {
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
            "requester_user_id":message["author_id"].as_str().unwrap_or(self.current_user_id()),
            "participation_generation":session["participation"]["generation"].as_u64().unwrap_or(0),
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
        if session["auto_naming"]["stage"] != "waiting_turn" || !naming_participation(&session) {
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
        let settings = match crate::session_naming::config::load(&self.root) {
            Ok(settings) => settings,
            // Optional title generation must not roll back the completed run.
            Err(_) => return self.skip_naming_configuration(tx, sid, &mut session).await,
        };
        let plan = if settings.model_role == crate::model_selection::ModelRole::System {
            first["system_selection_plan"].clone()
        } else { Value::Null };
        let skipped = settings.model_role == crate::model_selection::ModelRole::System && plan.is_null();
        session["auto_naming"]["config"] = json!(settings);
        session["auto_naming"]["stage"] = json!(if skipped { "skipped" } else { "queued" });
        session["auto_naming"]["system_plan"] = plan;
        session["auto_naming"]["queued_at"] = json!(crate::now());
        if !skipped {
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
    /// Retire an eligible naming job without replacing missing/invalid live settings.
    /// Database, encryption and event failures still fail the caller's transaction.
    async fn skip_naming_configuration(
        &self,
        tx: &mut Tx<'_>,
        sid: &str,
        session: &mut Value,
    ) -> Result<()> {
        session["auto_naming"]["stage"] = json!("skipped");
        session["auto_naming"]["claim"] = Value::Null;
        session["auto_naming"]["lease_until"] = json!(0);
        session["auto_naming"]["last_error"] = json!("Title configuration unavailable");
        sqlx::query("DELETE FROM docs WHERE key=?")
            .bind(format!("session_naming:{sid}"))
            .execute(&mut **tx)
            .await?;
        self.naming_save(tx, sid, session, true).await
    }
    // Range bounds use the docs primary-key index; case-insensitive LIKE scans every document.
    pub(crate) async fn naming_candidates(&self, recovery: bool) -> Result<Vec<String>> {
        let rows = sqlx::query("SELECT s.value FROM docs AS job JOIN docs AS s ON s.key='session:' || substr(job.key,16) WHERE job.key >= 'session_naming:' AND job.key < 'session_naming;' ORDER BY job.key")
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
        // Legacy pending jobs gain one durable snapshot on their first claim.
        if session["auto_naming"]["config"].is_null() {
            let settings = match crate::session_naming::config::load(&self.root) {
                Ok(settings) => settings,
                Err(_) => {
                    self.skip_naming_configuration(&mut tx, sid, &mut session).await?;
                    tx.commit().await?;
                    self.cache.lock().unwrap().invalidate(&[format!("session:{sid}")]);
                    return Ok(None);
                }
            };
            session["auto_naming"]["config"] = json!(settings);
        }
        let settings = crate::session_naming::config::for_session(&session)?;
        if session["auto_naming"]["attempts"].as_u64().unwrap_or(0) >= settings.max_job_attempts {
            session["auto_naming"]["stage"] = json!("exhausted");
            session["auto_naming"]["claim"] = Value::Null;
            session["auto_naming"]["lease_until"] = json!(0);
            session["auto_naming"]["last_error"] = json!("Title generation unavailable");
            sqlx::query("DELETE FROM docs WHERE key=?").bind(format!("session_naming:{sid}"))
                .execute(&mut *tx).await?;
            self.naming_save(&mut tx, sid, &mut session, true).await?;
            tx.commit().await?;
            self.cache.lock().unwrap().invalidate(&[format!("session:{sid}")]);
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
    pub(crate) async fn naming_pin_plan(&self, sid: &str, claim: &str, plan: &Value) -> Result<bool> {
        let _guard = self.context_gate.write().await;
        let mut tx = self.pool.begin().await?;
        let Some(mut session) = self.naming_session(&mut tx, sid).await? else { return Ok(false); };
        if !naming_owned(&session, claim) { return Ok(false); }
        let selected: crate::model_selection::SelectionPlan = serde_json::from_value(plan.clone())?;
        let settings = crate::session_naming::config::for_session(&session)?;
        if selected.role != settings.model_role || selected.agent_id != session["agent_id"].as_str().unwrap_or("") {
            return Err(anyhow!("Invalid naming plan"));
        }
        session["auto_naming"]["system_plan"] = plan.clone();
        self.naming_save(&mut tx, sid, &mut session, false).await?;
        tx.commit().await?;
        self.cache.lock().unwrap().invalidate(&[format!("session:{sid}")]);
        Ok(true)
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
        let settings = crate::session_naming::config::for_session(&session)?;
        let exhausted = title.is_none()
            && session["auto_naming"]["attempts"].as_u64().unwrap_or(0) >= settings.max_job_attempts;
        session["auto_naming"]["stage"] = json!(if title.is_some() {
            "completed"
        } else if exhausted {
            "exhausted"
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
            session["auto_naming"]["retry_at"] = json!(crate::now() + settings.recovery_retry_delay_seconds);
        }
        if exhausted {
            sqlx::query("DELETE FROM docs WHERE key=?").bind(format!("session_naming:{sid}"))
                .execute(&mut *tx).await?;
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
// Naming eligibility rules are Chat-owned; the host supplies the clock.
fn naming_due(session: &Value, recovery: bool) -> bool {
    webagent_chat::naming::naming_due(session, recovery, crate::now())
}
use webagent_chat::naming::naming_participation;
fn naming_owned(session: &Value, claim: &str) -> bool {
    webagent_chat::naming::naming_owned(session, claim, crate::now())
}

#[cfg(test)]
mod participation_tests {
 use super::*;
 #[tokio::test]
 async fn naming_binds_trusted_admitted_author_instead_of_storage_owner() {
  let (_root,app)=crate::runtime_tests::fixture().await;
  let sid=crate::runtime_tests::conversation(&app).await;
  let actor=app.store.create_user_account("naming-participant","long-hash-placeholder").await.unwrap()["user_id"].as_str().unwrap().to_owned();
  let mut session=app.store.get(&format!("session:{sid}")).await.unwrap().unwrap();
  session["auto_naming"]=json!({"stage":"awaiting_message"});
  app.store.put(&format!("session:{sid}"),&session).await.unwrap();
  let mut tx=app.store.pool.begin().await.unwrap();
  app.store.begin_session_naming(&mut tx,&sid,"participant-run",&json!({"author_id":actor,"content":"Participant title"})).await.unwrap();
  tx.commit().await.unwrap();
  app.store.cache.lock().unwrap().invalidate(&[format!("session:{sid}")]);
  let saved=app.store.get(&format!("session:{sid}")).await.unwrap().unwrap();
  assert_eq!(saved["auto_naming"]["requester_user_id"],actor);
  assert_ne!(saved["auto_naming"]["requester_user_id"],app.store.current_user_id());
  assert_eq!(saved["auto_naming"]["participation_generation"],0);
 }
}
