//! Session metadata stays in the user-owned AEAD document, not plaintext columns.
//! V2 reference: sessions.title/pinned/status/created_at and durable read markers.
//! V3 uses a sequence cursor instead of a wall-clock read marker. Activity is
//! distinct from metadata edits; public chat publication owns timestamp updates.
use super::*;
use serde::{Deserialize, Serialize};

const MIGRATION_KEY: &str = "storage:session_metadata_v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Recycled,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub title: String,
    pub pinned: bool,
    pub status: SessionStatus,
    pub created_at: i64,
    pub last_active_at: i64,
    pub read_through_seq: u64,
    pub recycled_at: Option<i64>,
    pub metadata_revision: u64,
    pub metadata_schema_version: u32,
}

/// Fill missing fields only. Preserve identity, checkpoints and unknown fields.
/// Existing invalid values fail closed rather than being silently replaced.
pub(super) fn with_session_metadata(value: &Value, activity: Option<i64>) -> Result<Value> {
    let mut value = value.clone();
    let object = value.as_object_mut().ok_or_else(|| anyhow!("Invalid session document"))?;
    // Zero denotes unknown legacy creation time; never fabricate old activity.
    object.entry("created_at").or_insert_with(|| json!(0));
    let created = object.get("created_at").and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("Invalid session creation timestamp"))?;
    let defaults = json!({
        "title":"New chat", "pinned":false, "status":"active",
        "last_active_at":activity.unwrap_or(created).max(created),
        "read_through_seq":0, "recycled_at":null, "metadata_revision":0,
        "metadata_schema_version":1
    });
    for (key, default) in defaults.as_object().unwrap() {
        object.entry(key.clone()).or_insert_with(|| default.clone());
    }
    let metadata: SessionMetadata = serde_json::from_value(value.clone())?;
    if metadata.metadata_schema_version != 1 || metadata.created_at < 0
        || metadata.last_active_at < metadata.created_at
        || metadata.recycled_at.is_some_and(|timestamp| timestamp < 0)
    {
        return Err(anyhow!("Unsupported or invalid session metadata"));
    }
    Ok(value)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionPatch {
    pub client_request_id: String,
    pub expected_revision: u64,
    pub title: Option<String>,
    pub pinned: Option<bool>,
}

impl SessionPatch {
    /// Reject empty patches and malformed retry identities or titles before entering a session transaction.
    pub fn validate(&self) -> std::result::Result<(), &'static str> {
        if self.client_request_id.is_empty() || self.client_request_id.len() > 128
            || self.client_request_id.trim() != self.client_request_id
            || self.client_request_id.chars().any(char::is_control) {
            return Err("Invalid request identifier");
        }
        if self.title.is_none() && self.pinned.is_none() { return Err("Empty session patch"); }
        if let Some(title) = &self.title {
            let title = title.trim();
            if title.is_empty() || title.len() > 200 || title.chars().any(char::is_control) {
                return Err("Invalid session title");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionLifecycle {
    pub client_request_id: String,
    pub expected_revision: u64,
}

#[derive(Debug)]
pub struct RecycledSession;
impl std::fmt::Display for RecycledSession {
    /// Hide recycled-session existence behind the same public not-found message.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("Session not found") }
}
impl std::error::Error for RecycledSession {}

pub enum SessionPatchOutcome {
    Saved(Value),
    NotFound,
    Conflict(&'static str),
    Invalid(&'static str),
}

/// Public projection: never return checkpoint input or other runtime documents.
pub fn public_session(value: &Value) -> Result<Value> {
    let metadata: SessionMetadata = serde_json::from_value(value.clone())?;
    let mut summary = serde_json::to_value(metadata)?;
    summary["id"] = value["id"].clone();
    summary["history_revision"] = json!(value["history_revision"].as_u64().unwrap_or(0));
    summary["agent_id"] = json!(value["agent_id"].as_str().unwrap_or("shared_default"));
    Ok(summary)
}

impl Store {
    /// Called only after a newly inserted public chat row, in its transaction.
    pub(super) async fn touch_session_activity(&self, tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, sid: &str) -> Result<()> {
        let key = format!("session:{sid}");
        let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&key).fetch_optional(&mut **tx).await? else { return Ok(()); };
        let mut session = self.unseal(&raw)?;
        let timestamp: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(created_at),0) FROM chat WHERE session_id=?")
            .bind(sid).fetch_one(&mut **tx).await?;
        session["last_active_at"] = json!(timestamp.max(session["last_active_at"].as_i64().unwrap_or(0)));
        sqlx::query("UPDATE docs SET value=? WHERE key=?").bind(self.encode_user_record(&session)?).bind(&key).execute(&mut **tx).await?;
        Ok(())
    }

    /// Read public metadata and unread counts from one user-owned snapshot.
    pub async fn session_read_summary(&self, sid: &str) -> Result<Value> {
        let mut tx = self.pool.begin().await?;
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(format!("session:{sid}")).fetch_one(&mut *tx).await?;
        let session = self.unseal(&raw)?;
        let result = self.read_summary_in(&mut tx, sid, &session).await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Count unread assistant bubbles against the supplied session read cursor in the caller transaction.
    pub(super) async fn read_summary_in(&self, tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, sid: &str, session: &Value) -> Result<Value> {
        let mut summary = public_session(session)?;
        let cursor = session["read_through_seq"].as_u64().ok_or_else(|| anyhow!("Invalid read cursor"))?;
        let unread: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat WHERE session_id=? AND role='assistant' AND seq>?")
            .bind(sid).bind(i64::try_from(cursor)?).fetch_one(&mut **tx).await?;
        summary["unread_count"] = json!(unread);
        summary["is_unread"] = json!(unread > 0);
        Ok(summary)
    }

    /// Explicit rendered-message acknowledgment. Natural max semantics make
    /// retries and older tabs harmless without mutating metadata revisions.
    pub async fn acknowledge_session_read(&self, sid: &str, through: u64) -> Result<SessionPatchOutcome> {
        let Ok(seq) = i64::try_from(through) else { return Ok(SessionPatchOutcome::Invalid("Invalid read cursor")); };
        let key = format!("session:{sid}");
        let _guard = self.context_gate.write().await;
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE docs SET key=key WHERE key=?").bind(&key).execute(&mut *tx).await?;
        let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&key).fetch_optional(&mut *tx).await? else { return Ok(SessionPatchOutcome::NotFound); };
        let mut session = self.unseal(&raw)?;
        if session["status"] != "active" { return Ok(SessionPatchOutcome::Conflict("Session is recycled")); }
        if seq != 0 {
            let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat WHERE session_id=? AND seq=?")
                .bind(sid).bind(seq).fetch_one(&mut *tx).await?;
            if exists != 1 { return Ok(SessionPatchOutcome::Invalid("Read cursor is not a message in this session")); }
        }
        let previous = session["read_through_seq"].as_u64().ok_or_else(|| anyhow!("Invalid stored read cursor"))?;
        if through > previous {
            session["read_through_seq"] = json!(through);
            sqlx::query("UPDATE docs SET value=? WHERE key=?").bind(self.encode_user_record(&session)?).bind(&key).execute(&mut *tx).await?;
        }
        let result = json!({"session":self.read_summary_in(&mut tx,sid,&session).await?});
        if through > previous {
            sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,'session_read',?)")
                .bind(sid).bind(self.encode_user_record(&result)?).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        self.cache.lock().unwrap().invalidate(&[key]);
        Ok(SessionPatchOutcome::Saved(result))
    }

    /// List active public sessions using the canonical pin/activity ordering.
    pub async fn ordered_sessions(&self) -> Result<Vec<Value>> {
        self.ordered_sessions_status(false).await
    }

    /// Project active or recycled public sessions and order pins, activity and identities deterministically.
    pub async fn ordered_sessions_status(&self, recycled: bool) -> Result<Vec<Value>> {
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query("SELECT value FROM docs WHERE key LIKE 'session:%' ORDER BY key").fetch_all(&mut *tx).await?;
        let mut summaries = Vec::with_capacity(rows.len());
        for row in rows {
            let session = self.unseal(&row.get::<Vec<u8>, _>("value"))?;
            if session["module_silent"] != true && session["status"] == if recycled { "recycled" } else { "active" } {
                let sid = session["id"].as_str().ok_or_else(|| anyhow!("Invalid session ID"))?;
                summaries.push(self.read_summary_in(&mut tx, sid, &session).await?);
            }
        }
        tx.commit().await?;
        summaries.sort_by(|a, b| b["pinned"].as_bool().cmp(&a["pinned"].as_bool())
            .then_with(|| b["last_active_at"].as_i64().cmp(&a["last_active_at"].as_i64()))
            .then_with(|| a["id"].as_str().cmp(&b["id"].as_str())));
        Ok(summaries)
    }

    /// Table-only statistics from this user's public chat and recorded usage.
    /// Pagination bounds payloads; authority is rechecked by the browser handler.
    pub async fn session_statistics_page(&self, recycled: bool, offset: usize, limit: usize) -> Result<(Vec<Value>, Option<usize>)> {
        let summaries = self.ordered_sessions_status(recycled).await?;
        let end = offset.saturating_add(limit).min(summaries.len());
        let next = (end < summaries.len()).then_some(end);
        let mut tx = self.pool.begin().await?;
        let mut page = Vec::new();
        for mut summary in summaries.into_iter().skip(offset).take(limit) {
            let sid = summary["id"].as_str().ok_or_else(|| anyhow!("Invalid session ID"))?;
            let (count, last_seq): (i64, i64) = sqlx::query_as(
                "SELECT COUNT(*),COALESCE(MAX(seq),0) FROM chat WHERE session_id=? AND role IN ('user','assistant')")
                .bind(sid).fetch_one(&mut *tx).await?;
            let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
                .bind(format!("session_usage:{sid}")).fetch_optional(&mut *tx).await?;
            let usage = match raw { Some(raw) => crate::usage::public(&self.unseal(&raw)?), None => Value::Null };
            let status: Option<String> = sqlx::query_scalar("SELECT status FROM run_index WHERE session_id=? ORDER BY updated_at DESC,id DESC LIMIT 1")
                .bind(sid).fetch_optional(&mut *tx).await?;
            summary["message_count"] = json!(count);
            summary["last_public_seq"] = json!(last_seq);
            summary["usage"] = usage;
            summary["run_status"] = json!(status);
            page.push(summary);
        }
        tx.commit().await?;
        Ok((page, next))
    }

    /// Reserve SQLite's writer before reading. Session, event and encrypted retry
    /// receipt commit together; the request key is scoped to this user and session.
    pub async fn patch_session(&self, sid: &str, patch: &SessionPatch) -> Result<SessionPatchOutcome> {
        if let Err(message) = patch.validate() { return Ok(SessionPatchOutcome::Invalid(message)); }
        let session_key = format!("session:{sid}");
        let receipt_key = format!("receipt:session:patch:{sid}:{}", patch.client_request_id);
        let fingerprint = crate::digest(patch);
        let _guard = self.context_gate.write().await;
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE docs SET key=key WHERE key=?")
            .bind(&session_key).execute(&mut *tx).await?;
        if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&receipt_key).fetch_optional(&mut *tx).await? {
            let receipt = self.unseal(&raw)?;
            if receipt["digest"] != fingerprint {
                return Ok(SessionPatchOutcome::Conflict("Request identifier reused with different input"));
            }
            tx.commit().await?;
            return Ok(SessionPatchOutcome::Saved(receipt["result"].clone()));
        }
        let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&session_key).fetch_optional(&mut *tx).await? else {
            return Ok(SessionPatchOutcome::NotFound);
        };
        let mut session = self.unseal(&raw)?;
        let metadata: SessionMetadata = serde_json::from_value(session.clone())?;
        if metadata.status != SessionStatus::Active {
            return Ok(SessionPatchOutcome::Conflict("Session is recycled"));
        }
        if metadata.metadata_revision != patch.expected_revision {
            return Ok(SessionPatchOutcome::Conflict("Session metadata revision changed"));
        }
        let mut changed = false;
        if let Some(title) = &patch.title {
            let title = title.trim();
            changed |= metadata.title != title;
            if session["auto_naming"].is_object() && session["auto_naming"]["stage"] != "locked" {
                changed = true;
                session["auto_naming"]["stage"] = json!("locked");
                session["auto_naming"]["claim"] = Value::Null;
                session["auto_naming"]["lease_until"] = json!(0);
                sqlx::query("DELETE FROM docs WHERE key=?").bind(format!("session_naming:{sid}"))
                    .execute(&mut *tx).await?;
            }
            session["title"] = json!(title);
        }
        if let Some(pinned) = patch.pinned {
            changed |= metadata.pinned != pinned;
            session["pinned"] = json!(pinned);
        }
        if changed {
            let Some(revision) = metadata.metadata_revision.checked_add(1) else {
                return Ok(SessionPatchOutcome::Conflict("Session metadata revision exhausted"));
            };
            session["metadata_revision"] = json!(revision);
            self.write_document(&mut tx, &session_key, &session).await?;
        }
        let summary = self.read_summary_in(&mut tx, sid, &session).await?;
        let result = json!({"session":summary});
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(&receipt_key).bind(self.encode_user_record(&json!({"digest":fingerprint,"result":result}))?)
            .execute(&mut *tx).await?;
        if changed {
            sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?, 'session_updated', ?)")
                .bind(sid).bind(self.encode_user_record(&result)?).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        self.cache.lock().unwrap().invalidate(&[session_key]);
        self.context_cache.lock().unwrap().invalidate(&self.context_cache_key(sid));
        Ok(SessionPatchOutcome::Saved(result))
    }

    /// Check under the caller's write transaction, never a cached snapshot.
    pub(super) async fn require_active_session(&self, tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, sid: &str) -> Result<()> {
        sqlx::query("UPDATE docs SET key=key WHERE key=?").bind(format!("session:{sid}")).execute(&mut **tx).await?;
        if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(format!("session:{sid}")).fetch_optional(&mut **tx).await? {
            if self.unseal(&raw)?["status"] != "active" { return Err(RecycledSession.into()); }
        }
        Ok(())
    }

    /// Check the current persisted lifecycle state without trusting a cached run snapshot.
    pub(crate) async fn session_available(&self, sid: &str) -> Result<bool> {
        Ok(self.get(&format!("session:{sid}")).await?
            .is_some_and(|session| session["status"] == "active"))
    }

    /// Revoke all durable session work in the lifecycle writer transaction.
    /// Increment run/task fences so old executors cannot publish after restore.
    async fn cancel_session_work(&self, tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, sid: &str) -> Result<()> {
        self.cancel_session_followups(tx,sid).await?;
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM run_index WHERE session_id=? AND status NOT IN ('completed','cancelled','failed')")
            .bind(sid).fetch_all(&mut **tx).await?;
        for id in ids {
            let key = format!("run:{id}");
            let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
                .bind(&key).fetch_one(&mut **tx).await?;
            let mut run = self.decode_document(tx,&raw).await?;
            // Retain uncertain-effect evidence; deletion never authorizes replay.
            if run["uncertain"] == true
                || (run["pending"]["state"] == "executing"
                    && !crate::tools::is_read(run["pending"]["name"].as_str().unwrap_or("")))
                || crate::parallel_batch::executing_write(&run) {
                run["uncertain"] = json!(true);
                crate::parallel_batch::mark_uncertain_writes(&mut run);
            }
            run["status"] = json!("cancelled");
            run["error"] = json!("Session not found");
            run["run_revision"] = json!(run["run_revision"].as_u64().unwrap_or(0)
                .checked_add(1).ok_or_else(||anyhow!("Run revision exhausted"))?);
            let epoch=run["lease_epoch"].as_i64().unwrap_or(0)
                .checked_add(1).ok_or_else(||anyhow!("Run lease epoch exhausted"))?;
            run["lease_epoch"] = json!(epoch);
            run["generation"] = json!(uuid::Uuid::new_v4().to_string());
            self.write_document(tx,&key,&run).await?;
            self.index_run(tx,&run).await?;
            sqlx::query("UPDATE run_index SET lease_epoch=?,lease_owner=NULL,lease_expires_at=0 WHERE id=?")
                .bind(epoch).bind(&id).execute(&mut **tx).await?;
        }
        let rows = sqlx::query("SELECT key,value FROM docs WHERE key LIKE 'background_task:%' OR key LIKE 'manager_review:%'")
            .fetch_all(&mut **tx).await?;
        for row in rows {
            let key: String = row.get("key");
            let mut record = self.unseal(&row.get::<Vec<u8>,_>("value"))?;
            if record["session_id"] != sid { continue; }
            if key.starts_with("background_task:") {
                if record["delivery_run_id"].is_string() || matches!(record["status"].as_str(),Some("cancelled"|"failed")) { continue; }
                record["status"] = json!("cancelled");
                let revision=record["revision"].as_i64().unwrap_or(0)
                    .checked_add(1).ok_or_else(||anyhow!("Background revision exhausted"))?;
                record["revision"] = json!(revision);
                record["lease_until"] = json!(0);
                record["claim_id"] = Value::Null;
                sqlx::query("UPDATE background_task_index SET status='cancelled',revision=? WHERE id=?")
                    .bind(revision).bind(record["id"].as_str().unwrap_or(""))
                    .execute(&mut **tx).await?;
                sqlx::query("UPDATE docs SET value=? WHERE key=?").bind(self.seal(&record)?).bind(&key).execute(&mut **tx).await?;
            } else if matches!(record["status"].as_str(),Some("pending_review"|"reviewing")) {
                record["status"] = json!("cancelled");
                record["generation"] = Value::Null;
                self.write_document(tx,&key,&record).await?;
            }
        }
        for key in [format!("manager_pending:{sid}"),format!("session_naming:{sid}")] {
            sqlx::query("DELETE FROM docs WHERE key=?").bind(key).execute(&mut **tx).await?;
        }
        Ok(())
    }

    /// Lifecycle, event and encrypted retry receipt commit atomically. History,
    /// pinning, activity, read cursor and agent binding are deliberately retained.
    pub async fn lifecycle_session(&self, sid: &str, body: &SessionLifecycle, restore: bool) -> Result<SessionPatchOutcome> {
        let validation = SessionPatch { client_request_id: body.client_request_id.clone(),
            expected_revision: body.expected_revision, title: None, pinned: Some(false) };
        if let Err(message) = validation.validate() { return Ok(SessionPatchOutcome::Invalid(message)); }
        let key = format!("session:{sid}");
        let receipt_key = format!("receipt:session:lifecycle:{sid}:{}",body.client_request_id);
        let fingerprint = crate::digest(&json!({"body":body,"restore":restore}));
        let _guard = self.context_gate.write().await;
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE docs SET key=key WHERE key=?").bind(&key).execute(&mut *tx).await?;
        if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&receipt_key).fetch_optional(&mut *tx).await? {
            let receipt = self.unseal(&raw)?;
            if receipt["digest"] != fingerprint { return Ok(SessionPatchOutcome::Conflict("Request identifier reused with different input")); }
            tx.commit().await?;
            return Ok(SessionPatchOutcome::Saved(receipt["result"].clone()));
        }
        let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&key).fetch_optional(&mut *tx).await? else { return Ok(SessionPatchOutcome::NotFound); };
        let mut session = self.unseal(&raw)?;
        let metadata: SessionMetadata = serde_json::from_value(session.clone())?;
        if metadata.status == SessionStatus::Deleted { return Ok(SessionPatchOutcome::NotFound); }
        if metadata.metadata_revision != body.expected_revision { return Ok(SessionPatchOutcome::Conflict("Session metadata revision changed")); }
        let target = if restore { "active" } else { "recycled" };
        let changed = session["status"] != target;
        if !restore {
            self.cancel_session_work(&mut tx,sid).await?;
            if session["auto_naming"].is_object() {
                session["auto_naming"]["stage"] = json!("locked");
                session["auto_naming"]["claim"] = Value::Null;
                session["auto_naming"]["lease_until"] = json!(0);
            }
        }
        if changed {
            let Some(revision) = metadata.metadata_revision.checked_add(1) else { return Ok(SessionPatchOutcome::Conflict("Session metadata revision exhausted")); };
            if !restore { session["execution_epoch"] = json!(session["execution_epoch"].as_u64().unwrap_or(0)
                .checked_add(1).ok_or_else(||anyhow!("Session execution epoch exhausted"))?); }
            session["status"] = json!(target);
            session["recycled_at"] = if restore { Value::Null } else { json!(crate::now()) };
            session["metadata_revision"] = json!(revision);
            self.write_document(&mut tx,&key,&session).await?;
        }
        let result = json!({"session":self.read_summary_in(&mut tx,sid,&session).await?});
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)").bind(&receipt_key)
            .bind(self.encode_user_record(&json!({"digest":fingerprint,"result":result}))?).execute(&mut *tx).await?;
        if changed {
            sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,?,?)").bind(sid)
                .bind(if restore { "session_restored" } else { "session_recycled" })
                .bind(self.encode_user_record(&result)?).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        self.cache.lock().unwrap().invalidate(&[key]);
        self.context_cache.lock().unwrap().invalidate(&self.context_cache_key(sid));
        Ok(SessionPatchOutcome::Saved(result))
    }

    /// Permanent deletion leaves only an identity tombstone and retry receipt.
    /// Uncertain external jobs retain their independent broker audit/rollback data.
    pub(crate) async fn purge_session(&self,sid:&str,body:&SessionLifecycle)->Result<SessionPatchOutcome> {
        let validation=SessionPatch {client_request_id:body.client_request_id.clone(),
            expected_revision:body.expected_revision,title:None,pinned:Some(false)};
        if let Err(message)=validation.validate() {return Ok(SessionPatchOutcome::Invalid(message));}
        let _guard=self.context_gate.write().await;
        let mut tx=self.pool.begin().await?;
        let key=format!("session:{sid}");
        let receipt_key=format!("receipt:session:purge:{sid}:{}",body.client_request_id);
        let fingerprint=crate::digest(body);
        sqlx::query("UPDATE docs SET value=value WHERE key=?").bind(&key).execute(&mut *tx).await?;
        if let Some(raw)=sqlx::query_scalar::<_,Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&receipt_key).fetch_optional(&mut *tx).await? {
            let receipt=self.unseal(&raw)?;
            return Ok(if receipt["digest"]==fingerprint {SessionPatchOutcome::Saved(receipt["result"].clone())}
                else {SessionPatchOutcome::Conflict("Request identifier reused with different input")});
        }
        let Some(raw)=sqlx::query_scalar::<_,Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&key).fetch_optional(&mut *tx).await? else {return Ok(SessionPatchOutcome::NotFound);};
        let session=self.unseal(&raw)?;
        if session["status"]=="deleted" {return Ok(SessionPatchOutcome::NotFound);}
        if session["metadata_revision"].as_u64()!=Some(body.expected_revision) {
            return Ok(SessionPatchOutcome::Conflict("Session metadata revision changed"));
        }
        if session["status"]!="recycled" {return Ok(SessionPatchOutcome::Conflict("Recycle the session before permanently deleting it"));}
        self.cancel_session_work(&mut tx,sid).await?;
        let mut ids:Vec<String>=sqlx::query_scalar("SELECT id FROM run_index WHERE session_id=?")
            .bind(sid).fetch_all(&mut *tx).await?;
        // Undo retains audited runs outside the active index. Purge their recovery data too.
        let archived_runs=sqlx::query("SELECT value FROM docs WHERE substr(key,1,4)=?")
            .bind("run:").fetch_all(&mut *tx).await?;
        for row in archived_runs {
            let record=self.unseal(&row.get::<Vec<u8>,_>("value"))?;
            if record["session_id"]==sid {
                if let Some(id)=record["id"].as_str() { if !ids.iter().any(|known|known==id) {ids.push(id.into());} }
            }
        }
        for id in &ids {
            for table in ["recovery_input","loop_commands","loop_events","publication_outbox"] {
                sqlx::query(&format!("DELETE FROM {table} WHERE run_id=?")).bind(id).execute(&mut *tx).await?;
            }
        }
        let message_ids:Vec<String>=sqlx::query_scalar("SELECT run_id FROM chat WHERE session_id=? AND role='user'")
            .bind(sid).fetch_all(&mut *tx).await?;
        // Explicit key namespaces plus sealed ownership; never substring-match an ID.
        let rows=sqlx::query("SELECT key,value FROM docs").fetch_all(&mut *tx).await?;
        let mut uncertain_effects=Vec::new();
        let mut removed=Vec::new();
        let mut upload_bytes=0u64;
        for row in rows {
            let candidate:String=row.get("key");
            if candidate==key {continue;}
            let record=self.unseal(&row.get::<Vec<u8>,_>("value"))?;
            let owned=record["session_id"]==sid || record["session"]["id"]==sid
                || record["result"]["session"]["id"]==sid || record["result"]["session_id"]==sid
                || (candidate.starts_with("receipt:session:") && record["result"]["id"]==sid)
                || ["session_mode","session_ability_state","session_usage","manager_pending","session_naming",
                    "administrator_host","administrator_browser","administrator_notice"].iter()
                    .any(|prefix|candidate==format!("{prefix}:{sid}"))
                || ["receipt:message","receipt:queue","receipt:session:patch","receipt:session:lifecycle","receipt:session:history","receipt:base",
                    "receipt:voice","receipt:chat_controls","receipt:session_mode","administrator_receipt"].iter()
                    .any(|prefix|candidate.starts_with(&format!("{prefix}:{sid}:")))
                || message_ids.iter().any(|id|candidate==format!("message_attachments:{id}"))
                || ["run","run_view"].iter().any(|prefix|ids.iter().any(|id|candidate==format!("{prefix}:{id}")))
                || ids.iter().any(|id|candidate.starts_with(&format!("receipt:run:{id}:")))
                || ids.iter().any(|id|record["result"]["run_id"]==*id
                    || (candidate.starts_with("receipt:") && record["result"]["id"]==*id));
            if owned {
                if candidate.starts_with("run:") && record["uncertain"]==true {
                    uncertain_effects.push(json!({"run_id":record["id"],"pending":record["pending"],"batch":record["batch"]}));
                }
                if candidate.starts_with("attachment:") {upload_bytes=upload_bytes.saturating_add(record["bytes"].as_u64().unwrap_or(0));}
                sqlx::query("DELETE FROM docs WHERE key=?").bind(&candidate).execute(&mut *tx).await?;
                removed.push(candidate);
            }
        }
        if upload_bytes>0 {
            if let Some(raw)=sqlx::query_scalar::<_,Vec<u8>>("SELECT value FROM docs WHERE key='attachment_quota'").fetch_optional(&mut *tx).await? {
                let mut quota=self.unseal(&raw)?;
                quota["bytes"]=json!(quota["bytes"].as_u64().unwrap_or(0).saturating_sub(upload_bytes));
                self.write_document(&mut tx,"attachment_quota",&quota).await?;
            }
        }
        // Background tables are lazily initialized; deletion must also work without any tasks.
        let has_background:i64=sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='background_task_index'").fetch_one(&mut *tx).await?;
        if has_background!=0 {
            sqlx::query("DELETE FROM background_task_delivery WHERE id IN (SELECT id FROM background_task_index WHERE session_id=?)").bind(sid).execute(&mut *tx).await?;
            sqlx::query("DELETE FROM background_task_index WHERE session_id=?").bind(sid).execute(&mut *tx).await?;
        }
        sqlx::query("DELETE FROM chat_context_boundaries WHERE message_id IN (SELECT id FROM chat WHERE session_id=?)").bind(sid).execute(&mut *tx).await?;
        for table in ["chat","interactions","events","reasoning_cleanup","run_index"] {
            sqlx::query(&format!("DELETE FROM {table} WHERE session_id=?")).bind(sid).execute(&mut *tx).await?;
        }
        sqlx::query("DELETE FROM activity_boundaries WHERE interaction_seq NOT IN (SELECT seq FROM interactions)").execute(&mut *tx).await?;
        let Some(revision)=body.expected_revision.checked_add(1) else {return Ok(SessionPatchOutcome::Conflict("Session metadata revision exhausted"));};
        let tombstone=with_session_metadata(&json!({"id":sid,"agent_id":session["agent_id"],"status":"deleted",
            "title":"","metadata_revision":revision,"execution_epoch":session["execution_epoch"],
            "uncertain_effects":uncertain_effects}),None)?;
        // Bypass prepare_session_document only for this lifecycle-owned tombstone.
        sqlx::query("UPDATE docs SET value=? WHERE key=?").bind(self.encode_user_record(&tombstone)?).bind(&key).execute(&mut *tx).await?;
        let result=json!({"id":sid,"deleted":true});
        self.write_document(&mut tx,&receipt_key,&json!({"digest":fingerprint,"result":result})).await?;
        tx.commit().await?;
        removed.push(key);
        self.cache.lock().unwrap().invalidate(&removed);
        self.context_cache.lock().unwrap().invalidate(&self.context_cache_key(sid));
        Ok(SessionPatchOutcome::Saved(result))
    }

    /// Merge partial writes without restoring stale lifecycle, history, queue or personal-read authorities.
    pub(super) async fn prepare_session_document(
        &self, tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, key: &str, value: &Value,
    ) -> Result<Value> {
        let mut next = value.clone();
        let object = next.as_object_mut().ok_or_else(|| anyhow!("Invalid session document"))?;
        let previous = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(key).fetch_optional(&mut **tx).await?;
        if let Some(raw) = previous {
            let previous = self.unseal(&raw)?;
            if previous["status"] == "deleted" { return Err(RecycledSession.into()); }
            if previous["history_revision"].as_u64().unwrap_or(0)
                > object.get("history_revision").and_then(Value::as_u64).unwrap_or(0) {
                // A pre-Undo snapshot must not restore the discarded compaction context.
                for field in ["history_revision", "checkpoint_input", "checkpoint_through", "tool_context_state"] {
                    if let Some(value) = previous.get(field) { object.insert(field.into(), value.clone()); }
                }
            }
            // Participation and queue are independently revisioned authorities.
            for (field,revision) in [("participation","revision"),("message_queue","queue_revision")] {
                let older=if field=="participation" {
                    previous[field][revision].as_u64().unwrap_or(0)>object.get(field).and_then(|v|v[revision].as_u64()).unwrap_or(0)
                } else {
                    previous[revision].as_u64().unwrap_or(0)>object.get(revision).and_then(Value::as_u64).unwrap_or(0)
                };
                if let Some(saved)=previous.get(field) {
                    if older || !object.contains_key(field){object.insert(field.into(),saved.clone());}
                }
                if field=="message_queue" && (older||!object.contains_key(revision)) {
                    if let Some(saved)=previous.get(revision){object.insert(revision.into(),saved.clone());}
                }
            }
            let mut reads=previous["participant_reads"].as_object().cloned().unwrap_or_default();
            if let Some(incoming)=object.get("participant_reads").and_then(Value::as_object) {
                for (actor,cursor) in incoming {
                    let value=cursor.as_u64().ok_or_else(||anyhow!("Invalid personal read cursor"))?;
                    let old=reads.get(actor).and_then(Value::as_u64).unwrap_or(0);
                    reads.insert(actor.clone(),json!(old.max(value)));
                }
            }
            if !reads.is_empty(){object.insert("participant_reads".into(),Value::Object(reads));}
            // A runner may carry a full session snapshot from before a metadata
            // edit. Preserve the newer metadata while accepting its checkpoint.
            let stale = previous["metadata_revision"].as_u64().unwrap_or(0)
                > object.get("metadata_revision").and_then(Value::as_u64).unwrap_or(0);
            for field in ["title", "created_at", "pinned", "status", "last_active_at",
                "read_through_seq", "recycled_at", "metadata_revision", "metadata_schema_version", "auto_naming"] {
                if let Some(value) = previous.get(field) {
                    if stale { object.insert(field.into(), value.clone()); }
                    else { object.entry(field).or_insert_with(|| value.clone()); }
                }
            }
        } else {
            object.entry("created_at").or_insert_with(|| json!(crate::now()));
        }
        if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(key).fetch_optional(&mut **tx).await? {
            let previous = self.unseal(&raw)?;
            for field in ["last_active_at", "read_through_seq", "execution_epoch", "history_revision"] {
                if field == "read_through_seq" && next["history_revision"].as_u64().unwrap_or(0) > previous["history_revision"].as_u64().unwrap_or(0) { continue; }
                let maximum = previous[field].as_u64().unwrap_or(0).max(next[field].as_u64().unwrap_or(0));
                next[field] = json!(maximum);
            }
        }
        with_session_metadata(&next, None)
    }

    /// Transactional, retry-safe backfill on first use of each user database.
    /// The SQLite write reservation precedes all reads, fencing other processes.
    /// A failure rolls back documents and marker together. No chat/event rows,
    /// agent binding, run state, or checkpoint sequences are modified.
    pub(super) async fn migrate_session_metadata(&self) -> Result<()> {
        if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(MIGRATION_KEY).fetch_optional(&self.pool).await?
        {
            if self.unseal(&raw)?["version"] != 1 {
                return Err(anyhow!("Unsupported session metadata migration"));
            }
            return Ok(());
        }
        let _guard = self.context_gate.write().await;
        let mut tx = self.pool.begin().await?;
        // Even when no marker exists this statement reserves the SQLite writer.
        sqlx::query("UPDATE docs SET key=key WHERE key=?")
            .bind(MIGRATION_KEY).execute(&mut *tx).await?;
        if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(MIGRATION_KEY).fetch_optional(&mut *tx).await?
        {
            if self.unseal(&raw)?["version"] != 1 {
                return Err(anyhow!("Unsupported session metadata migration"));
            }
            tx.commit().await?;
            return Ok(());
        }
        let mut after = String::new();
        loop {
            let rows = sqlx::query("SELECT key,value FROM docs WHERE key LIKE 'session:%' AND key>? ORDER BY key LIMIT 128")
                .bind(&after).fetch_all(&mut *tx).await?;
            if rows.is_empty() { break; }
            for row in rows {
                let key: String = row.get("key");
                let session = self.unseal(&row.get::<Vec<u8>, _>("value"))?;
                let activity = sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(created_at) FROM chat WHERE session_id=?")
                    .bind(key.strip_prefix("session:").unwrap()).fetch_one(&mut *tx).await?;
                let upgraded = with_session_metadata(&session, activity)?;
                if upgraded != session {
                    sqlx::query("UPDATE docs SET value=? WHERE key=?")
                        .bind(self.encode_user_record(&upgraded)?).bind(&key).execute(&mut *tx).await?;
                }
                after = key;
            }
        }
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(MIGRATION_KEY).bind(self.encode_user_record(&json!({"version":1}))?)
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a typed session metadata request with an explicit retry identity and expected revision.
    fn patch(request: &str, revision: u64, title: Option<&str>, pinned: Option<bool>) -> SessionPatch {
        SessionPatch { client_request_id:request.into(), expected_revision:revision,
            title:title.map(str::to_owned), pinned }
    }

    /// Build a typed recycle/restore/purge request for lifecycle assertions.
    fn lifecycle(request: &str, revision: u64) -> SessionLifecycle {
        SessionLifecycle { client_request_id: request.into(), expected_revision: revision }
    }

    /// Verify statistics pagination omits private context while retaining unknown usage accounting.
    #[tokio::test]
    async fn session_statistics_are_bounded_private_and_preserve_unknown_usage() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store.put("session:a", &json!({"id":"a","title":"First","checkpoint_input":["private"]})).await.unwrap();
        store.put("session:b", &json!({"id":"b","title":"Second"})).await.unwrap();
        store.put("session_usage:a", &json!({"input_tokens":7,"output_tokens":3,"reported_calls":1,"unreported_calls":1,"known_cost_usd":0.0,"unknown_cost_calls":1,"private":"secret"})).await.unwrap();
        let mut tx = store.pool.begin().await.unwrap();
        store.insert_chat(&mut tx,"a","r","assistant", &json!("secret content")).await.unwrap();
        tx.commit().await.unwrap();
        let (first, next) = store.session_statistics_page(false,0,1).await.unwrap();
        assert_eq!(first.len(),1); assert_eq!(next,Some(1));
        assert_eq!(first[0]["id"],"a"); assert_eq!(first[0]["message_count"],1);
        assert!(first[0]["last_public_seq"].as_u64().unwrap()>0);
        assert_eq!(first[0]["usage"]["input_tokens"],7);
        assert_eq!(first[0]["usage"]["unknown_cost_calls"],1);
        assert!(!serde_json::to_string(&first).unwrap().contains("secret"));
        assert!(first[0].get("checkpoint_input").is_none());
        let (second, next) = store.session_statistics_page(false,1,1).await.unwrap();
        assert_eq!(next,None); assert!(second[0]["usage"].is_null());
        assert_eq!(second[0]["message_count"],0);
        assert!(store.session_statistics_page(true,0,100).await.unwrap().0.is_empty());
    }

    /// Verify recycle/restore preserves history and metadata and old receipts cannot repeat an effect after restart.
    #[tokio::test]
    async fn lifecycle_preserves_history_metadata_retries_and_restart() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store.put("session:s", &json!({"id":"s","title":"Keep me","pinned":true,"created_at":100,"checkpoint_input":["keep"],"checkpoint_through":42})).await.unwrap();
        let mut tx = store.pool.begin().await.unwrap();
        store.insert_chat(&mut tx,"s","r","assistant",&json!("history")).await.unwrap(); tx.commit().await.unwrap();
        let history = store.chat_snapshot("s").await.unwrap();
        let stale = store.get("session:s").await.unwrap().unwrap();
        let SessionPatchOutcome::Saved(result) = store.lifecycle_session("s",&lifecycle("delete",0),false).await.unwrap() else { panic!() };
        assert_eq!(result["session"]["status"],"recycled");
        assert_eq!(result["session"]["metadata_revision"],1);
        assert!(store.ordered_sessions().await.unwrap().is_empty());
        assert_eq!(store.ordered_sessions_status(true).await.unwrap().len(),1);
        assert_eq!(store.chat_snapshot("s").await.unwrap(),history);
        store.put("session:s",&stale).await.unwrap();
        assert_eq!(store.get("session:s").await.unwrap().unwrap()["status"],"recycled");
        let SessionPatchOutcome::Saved(retry) = store.lifecycle_session("s",&lifecycle("delete",0),false).await.unwrap() else { panic!() };
        assert_eq!(result,retry);
        assert!(matches!(store.lifecycle_session("s",&lifecycle("delete",0),true).await.unwrap(),SessionPatchOutcome::Conflict(_)));
        assert!(matches!(store.lifecycle_session("s",&lifecycle("restore-stale",0),true).await.unwrap(),SessionPatchOutcome::Conflict(_)));
        assert!(matches!(store.lifecycle_session("missing",&lifecycle("missing",0),false).await.unwrap(),SessionPatchOutcome::NotFound));
        assert!(matches!(store.lifecycle_session("s",&lifecycle(" ",1),true).await.unwrap(),SessionPatchOutcome::Invalid(_)));
        let resumed = Store::open(root.path()).await.unwrap();
        let SessionPatchOutcome::Saved(restored) = resumed.lifecycle_session("s",&lifecycle("restore",1),true).await.unwrap() else { panic!() };
        assert_eq!(restored["session"]["status"],"active");
        assert!(restored["session"]["recycled_at"].is_null());
        for field in ["title","pinned","last_active_at","read_through_seq","created_at"] { assert_eq!(restored["session"][field],stale[field]); }
        let saved = resumed.get("session:s").await.unwrap().unwrap();
        assert_eq!(saved["checkpoint_input"],stale["checkpoint_input"]);
        assert_eq!(saved["checkpoint_through"],42);
        assert_eq!(resumed.chat_snapshot("s").await.unwrap(),history);
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key='receipt:session:lifecycle:s:delete'").fetch_one(&resumed.pool).await.unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("Keep me"));
        // Retrying an old delete after restore returns its receipt, not a new deletion.
        resumed.lifecycle_session("s",&lifecycle("delete",0),false).await.unwrap();
        assert_eq!(resumed.get("session:s").await.unwrap().unwrap()["status"],"active");
    }


    /// Verify purge removes conversation-owned private records and selections without touching another session or reviving work.
    #[tokio::test]
    async fn purge_erases_private_data_without_reviving_work_or_other_sessions() {
        let root=tempfile::tempdir().unwrap();
        let store=Store::open(root.path()).await.unwrap();
        store.put("session:s",&json!({"id":"s","agent_id":"shared_default","title":"Private title"})).await.unwrap();
        store.put("session:other",&json!({"id":"other"})).await.unwrap();
        store.put("run:r",&json!({"id":"r","session_id":"s","status":"queued","input":[{"role":"user","content":"private"}]})).await.unwrap();
        store.put("attachment:a",&json!({"id":"a","session_id":"s","bytes":3})).await.unwrap();
        store.put("attachment_quota",&json!({"bytes":8})).await.unwrap();
        store.put("session_context_assets:s",&json!({"version":1,"session_id":"s","scopes":{}})).await.unwrap();
        let mut tx=store.pool.begin().await.unwrap();
        store.write_document(&mut tx,"receipt:base:s:r:call",&json!({"output":{"artifact":"private checkpoint"}})).await.unwrap();
        tx.commit().await.unwrap();
        assert!(matches!(store.purge_session("s",&lifecycle("purge",0)).await.unwrap(),SessionPatchOutcome::Conflict(_)));
        store.lifecycle_session("s",&lifecycle("recycle",0),false).await.unwrap();
        assert!(store.get("session_context_assets:s").await.unwrap().is_some());
        let receipt=lifecycle("purge",1);
        assert!(matches!(store.purge_session("s",&receipt).await.unwrap(),SessionPatchOutcome::Saved(_)));
        assert!(matches!(store.purge_session("s",&receipt).await.unwrap(),SessionPatchOutcome::Saved(_)));
        assert!(matches!(store.lifecycle_session("s",&lifecycle("restore",2),true).await.unwrap(),SessionPatchOutcome::NotFound));
        assert!(store.get("run:r").await.unwrap().is_none());
        assert!(store.get("attachment:a").await.unwrap().is_none());
        assert!(store.get("session_context_assets:s").await.unwrap().is_none());
        let base_count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM docs WHERE key='receipt:base:s:r:call'").fetch_one(&store.pool).await.unwrap();
        assert_eq!(base_count,0);
        assert_eq!(store.get("attachment_quota").await.unwrap().unwrap()["bytes"],5);
        assert_eq!(store.get("session:other").await.unwrap().unwrap()["status"],"active");
        assert!(store.ordered_sessions_status(true).await.unwrap().is_empty());
        let tombstone=store.get("session:s").await.unwrap().unwrap();
        assert_eq!(tombstone["status"],"deleted");
        assert!(!tombstone.to_string().contains("Private title"));
        assert!(store.put("session:s",&json!({"id":"s","status":"active"})).await.is_err());
        assert!(store.put("run:late",&json!({"id":"late","session_id":"s","status":"queued"})).await.is_err());
        let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM recovery_input WHERE run_id='r'").fetch_one(&store.pool).await.unwrap();
        assert_eq!(count,0);
        let reopened=Store::open(root.path()).await.unwrap();
        assert!(!reopened.session_available("s").await.unwrap());
    }

    /// Verify recycle cancels session runs/tasks/reviews and rejects late publication even after restore.
    #[tokio::test]
    async fn recycle_cancels_owned_work_and_fences_late_writers() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store.create_admin_account("root","long-hash-placeholder","generation").await.unwrap();
        store.put("session:s",&json!({"id":"s"})).await.unwrap();
        store.put("session:other",&json!({"id":"other"})).await.unwrap();
        store.put("run:r",&json!({"id":"r","session_id":"s","status":"queued","generation":"","lease_epoch":0,"run_revision":0})).await.unwrap();
        let mut run=store.get("run:r").await.unwrap().unwrap();
        assert!(store.claim_run(&mut run,"owner","generation").await.unwrap());
        store.put("run:other",&json!({"id":"other","session_id":"other","status":"queued"})).await.unwrap();
        let task=json!({"id":"bg","user_id":"admin","session_id":"s","status":"running","revision":0});
        assert!(store.background_cas(&task,None).await.unwrap());
        store.put("manager_review:review",&json!({"id":"review","session_id":"s","status":"reviewing","generation":"before"})).await.unwrap();
        store.put("manager_pending:s",&json!({"id":"review"})).await.unwrap();
        assert!(matches!(store.lifecycle_session("s",&lifecycle("delete",0),false).await.unwrap(),SessionPatchOutcome::Saved(_)));
        assert_eq!(store.get("run:r").await.unwrap().unwrap()["status"],"cancelled");
        assert_eq!(store.get("run:other").await.unwrap().unwrap()["status"],"queued");
        assert_eq!(store.background_get("bg").await.unwrap().unwrap()["status"],"cancelled");
        assert!(store.get("manager_pending:s").await.unwrap().is_none());
        assert_eq!(store.get("manager_review:review").await.unwrap().unwrap()["status"],"cancelled");
        assert!(!store.renew_run_lease("r","generation","owner",1).await.unwrap());
        run["status"]=json!("completed");
        assert!(store.save_run(&mut run,"model_output",&json!({"output":[],"terminal_text":"late"})).await.is_err());
        store.lifecycle_session("s",&lifecycle("restore",1),true).await.unwrap();
        assert!(store.active_runs().await.unwrap().iter().all(|r|r["session_id"]!="s"));
        assert!(!store.background_cas(&task,Some(0)).await.unwrap());
    }

    /// Verify lifecycle cancellation handles unknown future nonterminal states and receipt failure rolls back all effects.
    #[tokio::test]
    async fn lifecycle_cancels_all_nonterminal_states_and_rolls_back_receipt_failure() {
        let root = tempfile::tempdir().unwrap(); let store = Store::open(root.path()).await.unwrap();
        store.put("session:s",&json!({"id":"s"})).await.unwrap();
        for (i,status) in ["queued","running","waiting_approval","waiting_recovery","stopped","unknown_future_state"].iter().enumerate() {
            let sid=format!("state-{i}");
            let rid=format!("state-run-{i}");
            store.put(&format!("session:{sid}"),&json!({"id":sid})).await.unwrap();
            store.put(&format!("run:{rid}"),&json!({"id":rid,"session_id":sid,"status":status})).await.unwrap();
            assert!(matches!(store.lifecycle_session(&sid,&lifecycle("delete",0),false).await.unwrap(),SessionPatchOutcome::Saved(_)),"{status}");
            assert_eq!(store.get(&format!("run:{rid}")).await.unwrap().unwrap()["status"],"cancelled");
        }
        store.put("run:r",&json!({"id":"r","session_id":"s","status":"completed"})).await.unwrap();
        sqlx::query("CREATE TRIGGER reject_lifecycle BEFORE INSERT ON docs WHEN NEW.key LIKE 'receipt:session:lifecycle:%' BEGIN SELECT RAISE(ABORT,'receipt failure'); END").execute(&store.pool).await.unwrap();
        assert!(store.lifecycle_session("s",&lifecycle("delete",0),false).await.is_err());
        assert_eq!(store.get("session:s").await.unwrap().unwrap()["status"],"active");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind='session_recycled' AND session_id='s'").fetch_one(&store.pool).await.unwrap(); assert_eq!(count,0);
        sqlx::query("DROP TRIGGER reject_lifecycle").execute(&store.pool).await.unwrap();
        store.lifecycle_session("s",&lifecycle("delete",0),false).await.unwrap();
        assert!(store.put("run:new",&json!({"id":"new","session_id":"s","status":"queued"})).await.unwrap_err().downcast_ref::<RecycledSession>().is_some());
        let mut run = store.get("run:r").await.unwrap().unwrap(); run["status"] = json!("queued");
        assert!(store.save_run_override(&mut run,"resumed",&json!({})).await.unwrap_err().downcast_ref::<RecycledSession>().is_some());
        assert!(store.atomic_docs_event(vec![],"s","message",&json!({"role":"user","run_id":"new","content":"blocked"})).await.unwrap_err().downcast_ref::<RecycledSession>().is_some());
        assert!(store.chat_snapshot("s").await.unwrap().0.is_empty());
    }

    /// Verify lifecycle no-ops produce no new revision/event and claims cannot revive recycled sessions.
    #[tokio::test]
    async fn lifecycle_noops_and_scheduler_claim_do_not_revive_recycled_sessions() {
        let root = tempfile::tempdir().unwrap(); let store = Store::open(root.path()).await.unwrap();
        store.put("session:s",&json!({"id":"s"})).await.unwrap();
        store.lifecycle_session("s",&lifecycle("already-active",0),true).await.unwrap();
        assert_eq!(store.get("session:s").await.unwrap().unwrap()["metadata_revision"],0);
        store.put("run:r",&json!({"id":"r","session_id":"s","status":"queued","generation":"","lease_epoch":0,"run_revision":0})).await.unwrap();
        // A legacy inconsistent fixture must not bypass scheduler admission.
        let mut session = store.get("session:s").await.unwrap().unwrap();
        session["status"] = json!("recycled"); session["metadata_revision"] = json!(1); session["recycled_at"] = json!(1);
        store.put("session:s",&session).await.unwrap();
        let mut run = store.get("run:r").await.unwrap().unwrap();
        assert!(store.claim_run(&mut run,"test-owner","test-generation").await.unwrap_err().downcast_ref::<RecycledSession>().is_some());
        assert_eq!(store.get("run:r").await.unwrap().unwrap()["status"],"queued");
        store.lifecycle_session("s",&lifecycle("already-recycled",1),false).await.unwrap();
        assert_eq!(store.get("session:s").await.unwrap().unwrap()["metadata_revision"],1);
        let count:i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind IN ('session_recycled','session_restored')").fetch_one(&store.pool).await.unwrap();
        assert_eq!(count,0);
    }

    /// Race message admission against recycle and verify admitted work is cancelled or never committed.
    #[tokio::test]
    async fn lifecycle_and_admission_race_across_independent_stores() {
        let root = tempfile::tempdir().unwrap(); let a = Store::open(root.path()).await.unwrap(); let b = Store::open(root.path()).await.unwrap();
        for i in 0..12 {
            let sid = format!("race-{i}"); a.put(&format!("session:{sid}"),&json!({"id":sid})).await.unwrap();
            let request = lifecycle("delete",0);
            let docs = vec![(format!("run:r-{i}"),json!({"id":format!("r-{i}"),"session_id":sid,"status":"queued"}))];
            let payload = json!({"role":"user","content":"hello","run_id":format!("r-{i}")});
            let (deleted, admitted) = tokio::join!(a.lifecycle_session(&sid,&request,false),b.atomic_docs_event(docs,&sid,"message",&payload));
            let deleted = matches!(deleted.unwrap(),SessionPatchOutcome::Saved(_));
            assert!(deleted);
            assert_eq!(a.get(&format!("session:{sid}")).await.unwrap().unwrap()["status"],"recycled");
            if admitted.is_ok() {
                assert_eq!(a.get(&format!("run:r-{i}")).await.unwrap().unwrap()["status"],"cancelled");
            } else {
                assert!(a.get(&format!("run:r-{i}")).await.unwrap().is_none());
            }
        }
    }

    /// Verify public publication advances activity and read acknowledgments survive stale session snapshots.
    #[tokio::test]
    async fn publication_activity_read_cursor_and_checkpoint_preservation() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store.put("session:s", &json!({"id":"s","created_at":100,"last_active_at":100})).await.unwrap();
        store.put("session:other", &json!({"id":"other","created_at":100})).await.unwrap();
        let stale = store.get("session:s").await.unwrap().unwrap();
        let mut tx = store.pool.begin().await.unwrap();
        store.insert_chat(&mut tx,"s","r1","user",&json!("hello")).await.unwrap();
        store.insert_chat(&mut tx,"s","r1","assistant",&json!("reply")).await.unwrap();
        store.insert_chat(&mut tx,"other","r2","assistant",&json!("elsewhere")).await.unwrap();
        tx.commit().await.unwrap();
        let (messages, cursor) = store.chat_snapshot("s").await.unwrap();
        let summary = store.session_read_summary("s").await.unwrap();
        assert_eq!(summary["unread_count"],1);
        assert_eq!(summary["is_unread"],true);
        assert_eq!(summary["last_active_at"],messages[1]["created_at"]);
        assert_eq!(summary["metadata_revision"],0);
        assert!(matches!(store.acknowledge_session_read("s",cursor as u64+1).await.unwrap(),SessionPatchOutcome::Invalid(_)));
        assert!(matches!(store.acknowledge_session_read("s",u64::MAX).await.unwrap(),SessionPatchOutcome::Invalid(_)));
        assert!(matches!(store.acknowledge_session_read("missing",0).await.unwrap(),SessionPatchOutcome::NotFound));
        let SessionPatchOutcome::Saved(read) = store.acknowledge_session_read("s",cursor as u64).await.unwrap() else { panic!() };
        assert_eq!(read["session"]["unread_count"],0);
        store.acknowledge_session_read("s",messages[0]["seq"].as_u64().unwrap()).await.unwrap();
        store.acknowledge_session_read("s",cursor as u64).await.unwrap();
        store.put("session:s", &stale).await.unwrap();
        let after = store.session_read_summary("s").await.unwrap();
        assert_eq!(after["read_through_seq"],cursor);
        assert_eq!(after["last_active_at"],summary["last_active_at"]);
        assert_eq!(after["metadata_revision"],0);
        let mut tx = store.pool.begin().await.unwrap();
        store.insert_chat(&mut tx,"s","r1","assistant",&json!("reply")).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(store.chat_snapshot("s").await.unwrap().0.len(),2);
        assert_eq!(store.session_read_summary("s").await.unwrap(),after);
        let events:i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind='session_read'").fetch_one(&store.pool).await.unwrap();
        assert_eq!(events,1);
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(reopened.session_read_summary("s").await.unwrap(),after);
    }

    /// Race read acknowledgments across Stores and verify monotonic cursors leave later publications unread.
    #[tokio::test]
    async fn read_ack_cross_store_race_preserves_maximum_and_new_unread() {
        let root = tempfile::tempdir().unwrap();
        let first = Store::open(root.path()).await.unwrap();
        first.put("session:s", &json!({"id":"s","created_at":1})).await.unwrap();
        let mut tx = first.pool.begin().await.unwrap();
        first.insert_chat(&mut tx,"s","r1","assistant",&json!("one")).await.unwrap();
        first.insert_chat(&mut tx,"s","r2","assistant",&json!("two")).await.unwrap();
        tx.commit().await.unwrap();
        let (messages, cursor) = first.chat_snapshot("s").await.unwrap();
        let second = Store::open(root.path()).await.unwrap();
        let (a,b) = tokio::join!(first.acknowledge_session_read("s",cursor as u64),
            second.acknowledge_session_read("s",messages[0]["seq"].as_u64().unwrap()));
        assert!(matches!(a.unwrap(),SessionPatchOutcome::Saved(_)));
        assert!(matches!(b.unwrap(),SessionPatchOutcome::Saved(_)));
        assert_eq!(first.session_read_summary("s").await.unwrap()["read_through_seq"],cursor);
        let mut tx = first.pool.begin().await.unwrap();
        first.insert_chat(&mut tx,"s","r3","assistant",&json!("three")).await.unwrap();
        tx.commit().await.unwrap();
        first.acknowledge_session_read("s",cursor as u64).await.unwrap();
        let summary = first.session_read_summary("s").await.unwrap();
        assert_eq!(summary["unread_count"],1);
        assert_eq!(summary["is_unread"],true);
        assert_eq!(summary["read_through_seq"],cursor);
        let SessionPatchOutcome::Saved(patched) = first.patch_session("s",&patch("rename",0,Some("renamed"),Some(true))).await.unwrap() else { panic!() };
        assert_eq!(patched["session"]["unread_count"],1);
        assert_eq!(patched["session"]["last_active_at"],summary["last_active_at"]);
        assert_eq!(patched["session"]["read_through_seq"],cursor);
    }

    /// Verify publication activity and read acknowledgment roll back with their failed transactions.
    #[tokio::test]
    async fn activity_and_read_event_failures_roll_back() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store.put("session:s", &json!({"id":"s","created_at":1})).await.unwrap();
        let mut tx = store.pool.begin().await.unwrap();
        store.insert_chat(&mut tx,"s","r","assistant",&json!("reply")).await.unwrap();
        tx.rollback().await.unwrap();
        assert_eq!(store.chat_snapshot("s").await.unwrap().0.len(),0);
        assert_eq!(store.session_read_summary("s").await.unwrap()["last_active_at"],1);
        let mut tx = store.pool.begin().await.unwrap();
        store.insert_chat(&mut tx,"s","r","assistant",&json!("reply")).await.unwrap();
        tx.commit().await.unwrap();
        let cursor = store.chat_snapshot("s").await.unwrap().1;
        sqlx::query("CREATE TRIGGER fail_read BEFORE INSERT ON events WHEN NEW.kind='session_read' BEGIN SELECT RAISE(ABORT,'test failure'); END").execute(&store.pool).await.unwrap();
        assert!(store.acknowledge_session_read("s",cursor as u64).await.is_err());
        assert_eq!(store.session_read_summary("s").await.unwrap()["read_through_seq"],0);
    }

    /// Exercise title/identity bounds and reject model-selected authority fields in session patches.
    #[test]
    fn session_patch_validation() {
        for title in ["", "   ", "bad\nname", &"é".repeat(101)] {
            assert!(patch("r", 0, Some(title), None).validate().is_err());
        }
        assert!(patch("r", 0, Some(&"é".repeat(100)), None).validate().is_ok());
        assert!(patch("r", 0, None, Some(false)).validate().is_ok());
        assert!(patch("r", 0, None, None).validate().is_err());
        assert!(patch(" r", 0, None, Some(true)).validate().is_err());
        assert!(serde_json::from_value::<SessionPatch>(json!({"client_request_id":"r",
            "expected_revision":0,"pinned":true,"agent_id":"other"})).is_err());
    }

    /// Verify metadata patch receipt replay and stale-checkpoint writes preserve newer user metadata.
    #[tokio::test]
    async fn session_patch_atomic_retry_and_stale_checkpoint() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store.put("session:s", &json!({"id":"s","title":"Old","created_at":100,
            "last_active_at":150,"read_through_seq":7,"checkpoint_through":42})).await.unwrap();
        let mut stale = store.get("session:s").await.unwrap().unwrap();
        let request = patch("request", 0, Some("  Private renamed title  "), Some(true));
        let SessionPatchOutcome::Saved(result) = store.patch_session("s", &request).await.unwrap()
            else { panic!("patch failed") };
        assert_eq!(result["session"]["title"], "Private renamed title");
        assert_eq!(result["session"]["metadata_revision"], 1);
        assert_eq!(result["session"]["last_active_at"], 150);
        assert_eq!(result["session"]["read_through_seq"], 7);
        assert!(result["session"].get("checkpoint_through").is_none());
        let SessionPatchOutcome::Saved(retry) = store.patch_session("s", &request).await.unwrap()
            else { panic!("retry failed") };
        assert_eq!(retry, result);
        assert!(matches!(store.patch_session("s", &patch("request", 0, Some("Different"), None)).await.unwrap(),
            SessionPatchOutcome::Conflict(_)));
        assert!(matches!(store.patch_session("s", &patch("stale", 0, None, Some(false))).await.unwrap(),
            SessionPatchOutcome::Conflict(_)));
        stale["checkpoint_through"] = json!(99);
        store.put("session:s", &stale).await.unwrap();
        let saved = store.get("session:s").await.unwrap().unwrap();
        assert_eq!(saved["title"], "Private renamed title");
        assert_eq!(saved["pinned"], true);
        assert_eq!(saved["metadata_revision"], 1);
        assert_eq!(saved["checkpoint_through"], 99);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind='session_updated'")
            .fetch_one(&store.pool).await.unwrap();
        assert_eq!(count, 1);
        // A no-op is receipted but creates no new revision/event.
        assert!(matches!(store.patch_session("s", &patch("noop", 1, None, Some(true))).await.unwrap(),
            SessionPatchOutcome::Saved(_)));
        assert_eq!(store.get("session:s").await.unwrap().unwrap()["metadata_revision"], 1);
        let raws: Vec<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs")
            .fetch_all(&store.pool).await.unwrap();
        assert!(raws.iter().all(|raw| !String::from_utf8_lossy(raw).contains("Private renamed title")));
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(reopened.get("session:s").await.unwrap().unwrap(), saved);
        assert!(matches!(reopened.patch_session("missing", &request).await.unwrap(), SessionPatchOutcome::NotFound));
    }

    /// Verify session ordering, account isolation and single-winner revision CAS across independent Stores.
    #[tokio::test]
    async fn session_patch_ordering_isolation_and_cross_process_conflict() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        for (id, pinned, active) in [("a",false,200),("b",false,200),("p",true,100),("new",false,300)] {
            store.put(&format!("session:{id}"), &json!({"id":id,"pinned":pinned,"created_at":100,
                "last_active_at":active,"checkpoint_input":["secret"]})).await.unwrap();
        }
        store.put("session:recycled", &json!({"id":"recycled","status":"recycled","pinned":true})).await.unwrap();
        let list = store.ordered_sessions().await.unwrap();
        assert_eq!(list.iter().map(|s|s["id"].as_str().unwrap()).collect::<Vec<_>>(), vec!["p","new","a","b"]);
        assert!(list.iter().all(|s|s.get("checkpoint_input").is_none()));
        assert!(matches!(store.patch_session("recycled", &patch("r",0,None,Some(false))).await.unwrap(),SessionPatchOutcome::Conflict(_)));
        sqlx::query("INSERT INTO user_accounts(user_id,username,value) VALUES('other','other',?)")
            .bind(store.seal(&json!({"user_id":"other","username":"other"})).unwrap())
            .execute(&store.app_pool).await.unwrap();
        let other = store.for_user("other").await.unwrap();
        assert!(other.ordered_sessions().await.unwrap().is_empty());
        assert!(matches!(other.patch_session("a", &patch("r",0,None,Some(true))).await.unwrap(),SessionPatchOutcome::NotFound));
        // Independently opened Store instances do not share the in-process gate.
        let independent = Store::open(root.path()).await.unwrap();
        let first = patch("one",0,Some("Winner one"),None);
        let second = patch("two",0,Some("Winner two"),None);
        let (one,two) = tokio::join!(store.patch_session("a",&first), independent.patch_session("a",&second));
        let outcomes = [one.unwrap(),two.unwrap()];
        assert_eq!(outcomes.iter().filter(|o| matches!(o,SessionPatchOutcome::Saved(_))).count(),1);
        assert_eq!(outcomes.iter().filter(|o| matches!(o,SessionPatchOutcome::Conflict(_))).count(),1);
        assert_eq!(store.get("session:a").await.unwrap().unwrap()["metadata_revision"],1);
    }

    /// Inject retry-receipt failure and verify metadata and event writes roll back before a successful retry.
    #[tokio::test]
    async fn session_patch_receipt_failure_rolls_back_everything() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store.put("session:s", &json!({"id":"s","title":"Original"})).await.unwrap();
        let original = store.get("session:s").await.unwrap().unwrap();
        sqlx::query("CREATE TRIGGER reject_patch_receipt BEFORE INSERT ON docs WHEN NEW.key LIKE 'receipt:session:patch:%' BEGIN SELECT RAISE(ABORT,'fixture receipt failure'); END")
            .execute(&store.pool).await.unwrap();
        let request = patch("atomic",0,Some("Changed"),Some(true));
        assert!(store.patch_session("s",&request).await.is_err());
        assert_eq!(store.get("session:s").await.unwrap().unwrap(),original);
        let count:i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind='session_updated'")
            .fetch_one(&store.pool).await.unwrap();
        assert_eq!(count,0);
        sqlx::query("DROP TRIGGER reject_patch_receipt").execute(&store.pool).await.unwrap();
        assert!(matches!(store.patch_session("s",&request).await.unwrap(),SessionPatchOutcome::Saved(_)));
    }

    /// Verify missing legacy metadata receives defaults without replacing saved fields or accepting invalid values.
    #[test]
    fn session_metadata_defaults_and_preservation() {
        let original = json!({"id":"s", "agent_id":"a", "title":"Original",
            "created_at":100, "checkpoint_input":[{"text":"keep"}], "checkpoint_through":42});
        let upgraded = with_session_metadata(&original, Some(150)).unwrap();
        assert_eq!(upgraded["last_active_at"], 150);
        assert_eq!(upgraded["pinned"], false);
        assert_eq!(upgraded["status"], "active");
        assert_eq!(upgraded["read_through_seq"], 0);
        for key in ["id", "agent_id", "title", "created_at", "checkpoint_input", "checkpoint_through"] {
            assert_eq!(original[key], upgraded[key]);
        }
        assert_eq!(with_session_metadata(&upgraded, Some(999)).unwrap(), upgraded);
        let mut edited = upgraded.clone();
        edited["pinned"] = json!(true);
        edited["status"] = json!("recycled");
        edited["recycled_at"] = json!(200);
        edited["read_through_seq"] = json!(17);
        edited["metadata_revision"] = json!(3);
        assert_eq!(with_session_metadata(&edited, None).unwrap(), edited);
        assert_eq!(with_session_metadata(&original, None).unwrap()["last_active_at"], 100);
        let mut invalid = original;
        invalid["pinned"] = json!("yes");
        assert!(with_session_metadata(&invalid, None).is_err());
    }

    /// Verify partial checkpoint writes preserve newer lifecycle and user metadata.
    #[tokio::test]
    async fn session_metadata_partial_writes_preserve_existing_values() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        let original = json!({"id":"s", "title":"Keep title", "created_at":100,
            "pinned":true, "status":"recycled", "recycled_at":160,
            "last_active_at":150, "read_through_seq":17, "metadata_revision":3});
        store.put("session:s", &original).await.unwrap();
        store.put("session:s", &json!({"id":"s", "checkpoint_input":[{"text":"updated"}], "checkpoint_through":42}))
            .await.unwrap();
        let saved = store.get("session:s").await.unwrap().unwrap();
        for field in ["title", "created_at", "pinned", "status", "recycled_at", "last_active_at", "read_through_seq", "metadata_revision"] {
            assert_eq!(saved[field], original[field]);
        }
        assert_eq!(saved["checkpoint_through"], 42);
    }

    /// Verify per-user metadata migration rolls back all upgrades and its marker if any document is invalid.
    #[tokio::test]
    async fn session_metadata_migration_is_user_scoped_and_rolls_back() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        sqlx::query("INSERT INTO user_accounts(user_id,username,value) VALUES('other','other',?)")
            .bind(store.seal(&json!({"user_id":"other", "username":"other"})).unwrap())
            .execute(&store.app_pool).await.unwrap();
        let other = store.for_user("other").await.unwrap();
        let app_marker: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM docs WHERE key=?")
            .bind(MIGRATION_KEY).fetch_one(&store.app_pool).await.unwrap();
        assert_eq!(app_marker, 0);
        sqlx::query("DELETE FROM docs WHERE key=?").bind(MIGRATION_KEY).execute(&other.pool).await.unwrap();
        let valid = json!({"id":"a", "created_at":100, "title":"Untouched until commit"});
        let invalid = json!({"id":"z", "created_at":100, "pinned":"invalid"});
        for (key, value) in [("session:a", &valid), ("session:z", &invalid)] {
            sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
                .bind(key).bind(store.seal(value).unwrap()).execute(&other.pool).await.unwrap();
        }
        assert!(other.migrate_session_metadata().await.is_err());
        assert_eq!(other.get("session:a").await.unwrap().unwrap(), valid);
        let marker: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM docs WHERE key=?")
            .bind(MIGRATION_KEY).fetch_one(&other.pool).await.unwrap();
        assert_eq!(marker, 0);
        sqlx::query("DELETE FROM docs WHERE key='session:z'").execute(&other.pool).await.unwrap();
        other.migrate_session_metadata().await.unwrap();
        assert_eq!(other.get("session:a").await.unwrap().unwrap()["last_active_at"], 100);
        assert!(store.get("session:a").await.unwrap().is_none());
    }

    /// Verify legacy metadata backfill is durable/idempotent and newly admitted sessions get canonical metadata immediately.
    #[tokio::test]
    async fn session_metadata_migration_persistence_and_atomic_creation() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        // Seed an old document through SQL only in this disposable test database.
        let original = json!({"id":"old", "agent_id":"a", "title":"Secret legacy title",
            "created_at":100, "checkpoint_input":[], "checkpoint_through":7});
        sqlx::query("DELETE FROM docs WHERE key=?").bind(MIGRATION_KEY).execute(&store.pool).await.unwrap();
        sqlx::query("INSERT INTO docs(key,value) VALUES('session:old',?)")
            .bind(store.seal(&original).unwrap()).execute(&store.pool).await.unwrap();
        sqlx::query("INSERT INTO chat(id,session_id,run_id,role,content,created_at) VALUES('m','old','r','assistant',?,150)")
            .bind(store.seal(&json!("Secret message")).unwrap()).execute(&store.pool).await.unwrap();
        let (_, cursor) = store.chat_snapshot("old").await.unwrap();
        store.migrate_session_metadata().await.unwrap();
        let migrated = store.get("session:old").await.unwrap().unwrap();
        assert_eq!(migrated["last_active_at"], 150);
        assert_eq!(migrated["checkpoint_through"], 7);
        assert_eq!(store.chat_snapshot("old").await.unwrap().1, cursor);
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key='session:old'")
            .fetch_one(&store.pool).await.unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("Secret legacy title"));
        store.migrate_session_metadata().await.unwrap();
        let again: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key='session:old'")
            .fetch_one(&store.pool).await.unwrap();
        assert_eq!(raw, again);
        store.atomic_docs_event(vec![("session:new".into(), json!({"id":"new", "title":"New", "created_at":200}))],
            "new", "session_created", &json!({})).await.unwrap();
        assert_eq!(store.get("session:new").await.unwrap().unwrap()["metadata_revision"], 0);
        store.put("session:put", &json!({"id":"put", "created_at":300})).await.unwrap();
        assert_eq!(store.get("session:put").await.unwrap().unwrap()["last_active_at"], 300);
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(reopened.get("session:old").await.unwrap().unwrap(), migrated);
    }
}
