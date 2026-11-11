//! Saved scripts: one sealed user-plane document per user and agent. Agent proposals (loop
//! process) and the user's decisions (API process) both change it under the user pool's
//! writer lock, so neither overwrites the other.
use anyhow::{bail, Result};
use serde_json::Value;
use webagent_host_core::StoreCore;

pub(crate) fn key(agent: &str) -> String {
    format!("saved_scripts:{agent}")
}

pub(crate) async fn change(
    store: &StoreCore,
    agent: &str,
    expected: Option<u64>,
    change: &mut (dyn FnMut(&mut Value) -> Result<Value> + Send),
) -> Result<Value> {
    let key = key(agent);
    let mut tx = store.pool_for(&key).begin().await?;
    // Take the writer lock before reading, as the own-skill catalog does.
    sqlx::query("UPDATE docs SET value=value WHERE 0")
        .execute(&mut *tx)
        .await?;
    let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(&key)
        .fetch_optional(&mut *tx)
        .await?;
    let mut doc = match raw {
        Some(raw) => store.decode_document(&mut tx, &raw).await?,
        // The caller supplies the empty shape (`webagent_abilities::scripts::saved_empty`).
        None => Value::Null,
    };
    if expected.is_some_and(|e| doc["revision"].as_u64().unwrap_or(0) != e) {
        bail!("Saved scripts changed; reload before deciding");
    }
    let result = change(&mut doc)?;
    store.write_document(&mut tx, &key, &doc).await?;
    tx.commit().await?;
    store.invalidate_document(&key);
    Ok(result)
}
