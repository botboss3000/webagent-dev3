//! One-time copy of the legacy single-database `runtime.sqlite` into the split
//! application/user/agent authorities, completed by the `storage:split_migrated`
//! version 2 marker. Every row is idempotent and verified after copy.
use crate::{docs, recovery, routing::Planes, ScopedCodec};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use sqlx::{sqlite::SqliteConnectOptions, Row, SqlitePool};
use std::{path::Path, str::FromStr};

const MARKER: &str = "storage:split_migrated";

/// Copy and verify legacy documents, events and run indexes before publishing the completed split marker.
pub async fn migrate_legacy_split<C: ScopedCodec>(
    root: &Path,
    planes: Planes<'_>,
    codec: &C,
) -> Result<()> {
    let legacy_path = root.join("runtime.sqlite");
    let marker = {
        let mut connection = planes.application.acquire().await?;
        match docs::read(&mut connection, MARKER).await? {
            Some(raw) => Some(recovery::read_document(&mut connection, codec, &raw).await?),
            None => None,
        }
    };
    let marker_version = marker
        .and_then(|marker| marker["version"].as_u64())
        .unwrap_or(0);
    if !legacy_path.exists() || marker_version >= 2 {
        return Ok(());
    }
    let upgrading_v1 = marker_version == 1;
    let legacy = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::from_str(&format!("sqlite://{}", legacy_path.display()))?
                .read_only(true),
        )
        .await?;
    let result = async {
        // Each row is idempotent. The marker is written only after all three
        // authorities have received and verified their legacy records.
        let mut last_key: Option<String> = None;
        loop {
            let rows = sqlx::query("SELECT key,value FROM docs WHERE (? IS NULL OR key>?) ORDER BY key LIMIT 128")
                .bind(&last_key).bind(&last_key).fetch_all(&legacy).await?;
            if rows.is_empty() { break; }
            for row in rows {
                let key: String = row.get("key");
                last_key = Some(key.clone());
                if key == MARKER { continue; }
                let raw: Vec<u8> = row.get("value");
                let mut value = codec.unseal(&raw)?;
                if key.starts_with("session:") && value["checkpoint_input"].is_null() {
                    if let Some(context) = value["context"].as_array() {
                        value["checkpoint_input"] = json!(context);
                        value["checkpoint_through"] = json!(0);
                    }
                }
                let destination = planes.for_key(&key);
                sqlx::query("INSERT OR IGNORE INTO docs(key,value) VALUES(?,?)")
                    .bind(&key).bind(codec.seal(&value)?).execute(destination).await?;
                let existing: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
                    .bind(&key).fetch_one(destination).await?;
                if codec.unseal(&existing)? != value {
                    if upgrading_v1 && (std::ptr::eq(destination, planes.application)
                        || std::ptr::eq(destination, planes.agent)) {
                        continue;
                    }
                    return Err(anyhow!("Conflicting migrated document: {key}"));
                }
                if let Some(sid) = key.strip_prefix("session:") {
                    migrate_legacy_chat(planes.user, codec, sid, &value).await?;
                }
            }
        }
        let mut last_seq = 0i64;
        loop {
            let rows = sqlx::query("SELECT seq,session_id,kind,payload FROM events WHERE seq>? ORDER BY seq LIMIT 128")
                .bind(last_seq).fetch_all(&legacy).await?;
            if rows.is_empty() { break; }
            for row in rows {
                let seq: i64 = row.get("seq");
                let sid: String = row.get("session_id");
                let kind: String = row.get("kind");
                let raw: Vec<u8> = row.get("payload");
                codec.unseal(&raw)?;
                last_seq = seq;
                let destination = if sid.is_empty() { planes.application } else { planes.user };
                migrate_legacy_event(codec, destination, seq, &sid, &kind, &raw, upgrading_v1).await?;
            }
        }
        let mut last_id: Option<String> = None;
        loop {
            let rows = sqlx::query("SELECT id,session_id,status,updated_at FROM run_index WHERE (? IS NULL OR id>?) ORDER BY id LIMIT 128")
                .bind(&last_id).bind(&last_id).fetch_all(&legacy).await?;
            if rows.is_empty() { break; }
            for row in rows {
                let id: String = row.get("id");
                let sid: String = row.get("session_id");
                let status: String = row.get("status");
                let updated_at: i64 = row.get("updated_at");
                last_id = Some(id.clone());
                sqlx::query("INSERT OR IGNORE INTO run_index(id,session_id,status,updated_at) VALUES(?,?,?,?)")
                    .bind(&id).bind(&sid).bind(&status).bind(updated_at).execute(planes.user).await?;
                let existing = sqlx::query("SELECT session_id,status,updated_at FROM run_index WHERE id=?")
                    .bind(&id).fetch_one(planes.user).await?;
                if existing.get::<String,_>("session_id") != sid
                    || existing.get::<String,_>("status") != status
                    || existing.get::<i64,_>("updated_at") != updated_at
                {
                    return Err(anyhow!("Conflicting migrated run index: {id}"));
                }
            }
        }
        let mut connection = planes.application.acquire().await?;
        docs::upsert(&mut connection, MARKER, &codec.seal(&json!({"version":2}))?).await
    }.await;
    legacy.close().await;
    result
}

/// Copy legacy public messages idempotently and reject conflicting destination identities or contents.
async fn migrate_legacy_chat<C: ScopedCodec>(
    user: &SqlitePool,
    codec: &C,
    sid: &str,
    session: &Value,
) -> Result<()> {
    let Some(messages) = session["messages"].as_array() else {
        return Ok(());
    };
    for (ordinal, message) in messages.iter().enumerate() {
        let role = message["role"]
            .as_str()
            .ok_or_else(|| anyhow!("Legacy chat role is missing"))?;
        if role != "user" && role != "assistant" {
            return Err(anyhow!("Unsupported legacy chat role"));
        }
        let content = message
            .get("content")
            .ok_or_else(|| anyhow!("Legacy chat content is missing"))?;
        let id = format!("legacy:{sid}:{ordinal}");
        let run_id = message["run_id"].as_str().unwrap_or("");
        let created_at = message["created_at"]
            .as_i64()
            .or_else(|| session["created_at"].as_i64())
            .unwrap_or(0);
        sqlx::query("INSERT OR IGNORE INTO chat(id,session_id,run_id,role,content,created_at,terminal_run_id) VALUES(?,?,?,?,?,?,NULL)")
            .bind(&id).bind(sid).bind(run_id).bind(role).bind(codec.encode_user_record(content)?)
            .bind(created_at).execute(user).await?;
        let row =
            sqlx::query("SELECT session_id,run_id,role,content,created_at FROM chat WHERE id=?")
                .bind(&id)
                .fetch_one(user)
                .await?;
        if row.get::<String, _>("session_id") != sid
            || row.get::<String, _>("run_id") != run_id
            || row.get::<String, _>("role") != role
            || row.get::<i64, _>("created_at") != created_at
            || codec.unseal(&row.get::<Vec<u8>, _>("content"))? != *content
        {
            return Err(anyhow!("Conflicting migrated chat record"));
        }
    }
    Ok(())
}

/// Preserve legacy event identity across split-storage retries without reusing another mapped sequence.
async fn migrate_legacy_event<C: ScopedCodec>(
    codec: &C,
    destination: &SqlitePool,
    legacy_seq: i64,
    sid: &str,
    kind: &str,
    raw: &[u8],
    upgrading_v1: bool,
) -> Result<()> {
    let value = codec.unseal(raw)?;
    if !upgrading_v1 {
        sqlx::query("INSERT OR IGNORE INTO events(seq,session_id,kind,payload) VALUES(?,?,?,?)")
            .bind(legacy_seq)
            .bind(sid)
            .bind(kind)
            .bind(raw)
            .execute(destination)
            .await?;
        let existing = sqlx::query("SELECT session_id,kind,payload FROM events WHERE seq=?")
            .bind(legacy_seq)
            .fetch_one(destination)
            .await?;
        if existing.get::<String, _>("session_id") != sid
            || existing.get::<String, _>("kind") != kind
            || codec.unseal(&existing.get::<Vec<u8>, _>("payload"))? != value
        {
            return Err(anyhow!("Conflicting migrated event: {legacy_seq}"));
        }
        return Ok(());
    }
    let mut tx = destination.begin().await?;
    let mapped =
        sqlx::query_scalar::<_, i64>("SELECT new_seq FROM legacy_event_map WHERE legacy_seq=?")
            .bind(legacy_seq)
            .fetch_optional(&mut *tx)
            .await?;
    let new_seq = if let Some(seq) = mapped {
        seq
    } else {
        let sequence_claimed =
            sqlx::query_scalar::<_, i64>("SELECT legacy_seq FROM legacy_event_map WHERE new_seq=?")
                .bind(legacy_seq)
                .fetch_optional(&mut *tx)
                .await?
                .is_some();
        let same_seq = sqlx::query("SELECT session_id,kind,payload FROM events WHERE seq=?")
            .bind(legacy_seq)
            .fetch_optional(&mut *tx)
            .await?;
        let already_copied = if let Some(row) = same_seq {
            !sequence_claimed
                && row.get::<String, _>("session_id") == sid
                && row.get::<String, _>("kind") == kind
                && codec.unseal(&row.get::<Vec<u8>, _>("payload"))? == value
        } else {
            false
        };
        let seq = if already_copied {
            legacy_seq
        } else {
            sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,?,?)")
                .bind(sid)
                .bind(kind)
                .bind(raw)
                .execute(&mut *tx)
                .await?
                .last_insert_rowid()
        };
        sqlx::query("INSERT INTO legacy_event_map(legacy_seq,new_seq) VALUES(?,?)")
            .bind(legacy_seq)
            .bind(seq)
            .execute(&mut *tx)
            .await?;
        seq
    };
    let existing = sqlx::query("SELECT session_id,kind,payload FROM events WHERE seq=?")
        .bind(new_seq)
        .fetch_one(&mut *tx)
        .await?;
    if existing.get::<String, _>("session_id") != sid
        || existing.get::<String, _>("kind") != kind
        || codec.unseal(&existing.get::<Vec<u8>, _>("payload"))? != value
    {
        return Err(anyhow!("Conflicting migrated event mapping: {legacy_seq}"));
    }
    tx.commit().await?;
    Ok(())
}
