//! Attachment record writes on the caller's borrowed user-plane transaction. Storage keeps the pool,
//! cache and codec; Files decides which records may change and how the quota follows them.
use crate::{attachment_key, RecordCodec, QUOTA_KEY};
use anyhow::{anyhow, Result};
use serde_json::json;
use sqlx::{Sqlite, Transaction};

/// Remove only an unclaimed upload owned by session `sid` and decrement the quota in `tx`.
/// Returns false when the record is already absent. The caller commits, then invalidates
/// the attachment and quota cache entries.
pub async fn delete_pending_upload(
    codec: &impl RecordCodec,
    tx: &mut Transaction<'_, Sqlite>,
    sid: &str,
    aid: &str,
) -> Result<bool> {
    // Take the write lock before reading so concurrent uploads and deletions serialize on the quota.
    sqlx::query("UPDATE docs SET value=value WHERE key=?")
        .bind(QUOTA_KEY)
        .execute(&mut **tx)
        .await?;
    let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(attachment_key(aid))
        .fetch_optional(&mut **tx)
        .await?;
    let Some(raw) = raw else { return Ok(false) };
    let record = codec.unseal(&raw)?;
    if record["session_id"] != sid || !record["run_id"].is_null() {
        return Err(anyhow!("Attachment removal unavailable"));
    }
    let quota: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(QUOTA_KEY)
        .fetch_optional(&mut **tx)
        .await?;
    let used = quota
        .map(|raw| codec.unseal(&raw))
        .transpose()?
        .and_then(|v| v["bytes"].as_u64())
        .unwrap_or(0);
    let remaining = used.saturating_sub(record["bytes"].as_u64().unwrap_or(0));
    sqlx::query("UPDATE docs SET value=? WHERE key=?")
        .bind(codec.encode_user_record(&json!({"bytes":remaining}))?)
        .bind(QUOTA_KEY)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM docs WHERE key=?")
        .bind(attachment_key(aid))
        .execute(&mut **tx)
        .await?;
    Ok(true)
}
