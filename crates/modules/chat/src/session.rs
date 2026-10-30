//! Session metadata, lifecycle validation, public projection and user-plane read cursors.
use crate::RecordCodec;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

pub const MIGRATION_KEY: &str = "storage:session_metadata_v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Archived,
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
    pub archived_at: Option<i64>,
    pub metadata_revision: u64,
    pub metadata_schema_version: u32,
}

/// Fill missing fields only. Preserve identity, checkpoints and unknown fields.
/// Existing invalid values fail closed rather than being silently replaced.
pub fn with_session_metadata(value: &Value, activity: Option<i64>) -> Result<Value> {
    let mut value = value.clone();
    let object = value
        .as_object_mut()
        .ok_or_else(|| anyhow!("Invalid session document"))?;
    // Zero denotes unknown legacy creation time; never fabricate old activity.
    object.entry("created_at").or_insert_with(|| json!(0));
    let created = object
        .get("created_at")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("Invalid session creation timestamp"))?;
    let defaults = json!({
        "title":"New chat", "pinned":false, "status":"active",
        "last_active_at":activity.unwrap_or(created).max(created),
        "read_through_seq":0, "archived_at":null, "metadata_revision":0,
        "metadata_schema_version":1
    });
    for (key, default) in defaults.as_object().unwrap() {
        object.entry(key.clone()).or_insert_with(|| default.clone());
    }
    let metadata: SessionMetadata = serde_json::from_value(value.clone())?;
    if metadata.metadata_schema_version != 1
        || metadata.created_at < 0
        || metadata.last_active_at < metadata.created_at
        || metadata.archived_at.is_some_and(|timestamp| timestamp < 0)
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
        if self.client_request_id.is_empty()
            || self.client_request_id.len() > 128
            || self.client_request_id.trim() != self.client_request_id
            || self.client_request_id.chars().any(char::is_control)
        {
            return Err("Invalid request identifier");
        }
        if self.title.is_none() && self.pinned.is_none() {
            return Err("Empty session patch");
        }
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
pub struct ArchivedSession;
impl std::fmt::Display for ArchivedSession {
    /// Hide archived-session existence behind the same public not-found message.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Session not found")
    }
}
impl std::error::Error for ArchivedSession {}

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
    summary["main_chat"] =
        json!(value["main_chat"] == true && value["main_chat_agent_id"] == value["agent_id"]);
    summary["can_delete"] =
        json!(!(value["main_chat"] == true && value["main_chat_agent_id"] == value["agent_id"]));
    summary["history_revision"] = json!(value["history_revision"].as_u64().unwrap_or(0));
    summary["agent_id"] = json!(value["agent_id"].as_str().unwrap_or("shared_default"));
    Ok(summary)
}

/// Count unread assistant bubbles against the supplied session read cursor in the caller transaction.
pub async fn read_summary_in(
    tx: &mut Transaction<'_, Sqlite>,
    sid: &str,
    session: &Value,
) -> Result<Value> {
    let mut summary = public_session(session)?;
    let cursor = session["read_through_seq"]
        .as_u64()
        .ok_or_else(|| anyhow!("Invalid read cursor"))?;
    let unread: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM chat WHERE session_id=? AND role='assistant' AND seq>?",
    )
    .bind(sid)
    .bind(i64::try_from(cursor)?)
    .fetch_one(&mut **tx)
    .await?;
    summary["unread_count"] = json!(unread);
    summary["is_unread"] = json!(unread > 0);
    Ok(summary)
}

/// Called only after a newly inserted public chat row, in its transaction.
pub async fn touch_session_activity<C: RecordCodec>(
    codec: &C,
    tx: &mut Transaction<'_, Sqlite>,
    sid: &str,
) -> Result<()> {
    let key = format!("session:{sid}");
    let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
        .bind(&key)
        .fetch_optional(&mut **tx)
        .await?
    else {
        return Ok(());
    };
    let mut session = codec.unseal(&raw)?;
    let timestamp: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(created_at),0) FROM chat WHERE session_id=?")
            .bind(sid)
            .fetch_one(&mut **tx)
            .await?;
    session["last_active_at"] =
        json!(timestamp.max(session["last_active_at"].as_i64().unwrap_or(0)));
    sqlx::query("UPDATE docs SET value=? WHERE key=?")
        .bind(codec.encode_user_record(&session)?)
        .bind(&key)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Check under the caller's write transaction, never a cached snapshot.
pub async fn require_active_session<C: RecordCodec>(
    codec: &C,
    tx: &mut Transaction<'_, Sqlite>,
    sid: &str,
) -> Result<()> {
    sqlx::query("UPDATE docs SET key=key WHERE key=?")
        .bind(format!("session:{sid}"))
        .execute(&mut **tx)
        .await?;
    if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
        .bind(format!("session:{sid}"))
        .fetch_optional(&mut **tx)
        .await?
    {
        if codec.unseal(&raw)?["status"] != "active" {
            return Err(ArchivedSession.into());
        }
    }
    Ok(())
}

/// Merge partial writes without restoring stale lifecycle, history, queue or personal-read authorities.
pub async fn prepare_session_document<C: RecordCodec>(
    codec: &C,
    tx: &mut Transaction<'_, Sqlite>,
    key: &str,
    value: &Value,
    now: u64,
) -> Result<Value> {
    let mut next = value.clone();
    let object = next
        .as_object_mut()
        .ok_or_else(|| anyhow!("Invalid session document"))?;
    let previous = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
        .bind(key)
        .fetch_optional(&mut **tx)
        .await?;
    if let Some(raw) = previous {
        let previous = codec.unseal(&raw)?;
        if previous["status"] == "deleted" {
            return Err(ArchivedSession.into());
        }
        // Checkpoints retain the current trusted role, including demotion.
        object.insert("main_chat".into(), json!(previous["main_chat"] == true));
        object.insert(
            "main_chat_agent_id".into(),
            previous["main_chat_agent_id"].clone(),
        );
        object.insert(
            "agent_id".into(),
            json!(previous["agent_id"].as_str().unwrap_or("shared_default")),
        );
        if previous["history_revision"].as_u64().unwrap_or(0)
            > object
                .get("history_revision")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        {
            // A pre-Undo snapshot must not restore the discarded compaction context.
            for field in [
                "history_revision",
                "checkpoint_input",
                "checkpoint_through",
                "tool_context_state",
                "tool_result_projection",
            ] {
                if let Some(value) = previous.get(field) {
                    object.insert(field.into(), value.clone());
                }
            }
        }
        // Participation and queue are independently revisioned authorities.
        for (field, revision) in [
            ("participation", "revision"),
            ("message_queue", "queue_revision"),
        ] {
            let older = if field == "participation" {
                previous[field][revision].as_u64().unwrap_or(0)
                    > object
                        .get(field)
                        .and_then(|v| v[revision].as_u64())
                        .unwrap_or(0)
            } else {
                previous[revision].as_u64().unwrap_or(0)
                    > object.get(revision).and_then(Value::as_u64).unwrap_or(0)
            };
            if let Some(saved) = previous.get(field) {
                if older || !object.contains_key(field) {
                    object.insert(field.into(), saved.clone());
                }
            }
            if field == "message_queue" && (older || !object.contains_key(revision)) {
                if let Some(saved) = previous.get(revision) {
                    object.insert(revision.into(), saved.clone());
                }
            }
        }
        let mut reads = previous["participant_reads"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        if let Some(incoming) = object.get("participant_reads").and_then(Value::as_object) {
            for (actor, cursor) in incoming {
                let value = cursor
                    .as_u64()
                    .ok_or_else(|| anyhow!("Invalid personal read cursor"))?;
                let old = reads.get(actor).and_then(Value::as_u64).unwrap_or(0);
                reads.insert(actor.clone(), json!(old.max(value)));
            }
        }
        if !reads.is_empty() {
            object.insert("participant_reads".into(), Value::Object(reads));
        }
        // A runner may carry a full session snapshot from before a metadata
        // edit. Preserve the newer metadata while accepting its checkpoint.
        let stale = previous["metadata_revision"].as_u64().unwrap_or(0)
            > object
                .get("metadata_revision")
                .and_then(Value::as_u64)
                .unwrap_or(0);
        for field in [
            "title",
            "created_at",
            "pinned",
            "status",
            "last_active_at",
            "read_through_seq",
            "archived_at",
            "metadata_revision",
            "metadata_schema_version",
            "auto_naming",
        ] {
            if let Some(value) = previous.get(field) {
                if stale {
                    object.insert(field.into(), value.clone());
                } else {
                    object.entry(field).or_insert_with(|| value.clone());
                }
            }
        }
    } else {
        object.entry("created_at").or_insert_with(|| json!(now));
    }
    if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
        .bind(key)
        .fetch_optional(&mut **tx)
        .await?
    {
        let previous = codec.unseal(&raw)?;
        for field in [
            "last_active_at",
            "read_through_seq",
            "execution_epoch",
            "history_revision",
        ] {
            if field == "read_through_seq"
                && next["history_revision"].as_u64().unwrap_or(0)
                    > previous["history_revision"].as_u64().unwrap_or(0)
            {
                continue;
            }
            let maximum = previous[field]
                .as_u64()
                .unwrap_or(0)
                .max(next[field].as_u64().unwrap_or(0));
            next[field] = json!(maximum);
        }
    }
    with_session_metadata(&next, None)
}

/// Project active or archived public sessions and order pins, activity and identities deterministically.
pub async fn ordered_sessions_status<C: RecordCodec>(
    pool: &SqlitePool,
    codec: &C,
    archived: bool,
) -> Result<Vec<Value>> {
    let mut tx = pool.begin().await?;
    let rows = sqlx::query("SELECT value FROM docs WHERE key LIKE 'session:%' ORDER BY key")
        .fetch_all(&mut *tx)
        .await?;
    let mut summaries = Vec::with_capacity(rows.len());
    for row in rows {
        let session = codec.unseal(&row.get::<Vec<u8>, _>("value"))?;
        if session["module_silent"] != true
            && session["status"] == if archived { "archived" } else { "active" }
        {
            let sid = session["id"]
                .as_str()
                .ok_or_else(|| anyhow!("Invalid session ID"))?;
            summaries.push(read_summary_in(&mut tx, sid, &session).await?);
        }
    }
    tx.commit().await?;
    summaries.sort_by(|a, b| {
        b["pinned"]
            .as_bool()
            .cmp(&a["pinned"].as_bool())
            .then_with(|| {
                b["last_active_at"]
                    .as_i64()
                    .cmp(&a["last_active_at"].as_i64())
            })
            .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
    });
    Ok(summaries)
}
