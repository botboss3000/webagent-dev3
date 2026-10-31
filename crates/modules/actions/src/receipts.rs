//! Native agent receipts: the sealed `{arguments_digest, result}` record a native self-edit
//! writes in the same agent transaction as its mutation, keyed by the original pending call.
//! Reconciliation reads it to settle an uncertain action without executing again.
use crate::RecordCodec;
use anyhow::Result;
use serde_json::{json, Value};
use sqlx::{Sqlite, SqlitePool, Transaction};

/// Receipt key of a native action of `name` for `agent` and pending call `pending`.
#[must_use]
pub fn key(agent: &str, name: &str, pending: &str) -> String {
    if name == "prompt_slot_write" {
        format!("agent:{agent}:action_prompt:{pending}")
    } else {
        format!("agent:{agent}:base_self_edit:{pending}")
    }
}

/// Read receipt `key` inside the caller's agent transaction.
pub async fn read_in(
    codec: &impl RecordCodec,
    tx: &mut Transaction<'_, Sqlite>,
    key: &str,
) -> Result<Option<Value>> {
    let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(key)
        .fetch_optional(&mut **tx)
        .await?;
    raw.map(|v| codec.unseal(&v)).transpose()
}

/// Insert receipt `key` inside the caller's agent transaction; fails if it already exists.
pub async fn write_in(
    codec: &impl RecordCodec,
    tx: &mut Transaction<'_, Sqlite>,
    key: &str,
    hash: &str,
    result: &Value,
) -> Result<()> {
    sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
        .bind(key)
        .bind(codec.seal(&json!({"arguments_digest":hash,"result":result}))?)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Read receipt `key` from the agent pool.
pub async fn read(
    codec: &impl RecordCodec,
    agent_pool: &SqlitePool,
    key: &str,
) -> Result<Option<Value>> {
    let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(key)
        .fetch_optional(agent_pool)
        .await?;
    raw.map(|v| codec.unseal(&v)).transpose()
}
