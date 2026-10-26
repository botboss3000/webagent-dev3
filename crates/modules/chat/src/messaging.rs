//! Native Base cross-session delivery schemas, argument validation and deterministic identity helpers.
use crate::{digest, RecordCodec};
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{Sqlite, Transaction};

pub fn starts(name: &str) -> bool {
    matches!(
        name,
        "start_session" | "spawn_agent" | "delegate_task_to_agent" | "delegate_to_agent"
    )
}

pub fn owns(name: &str) -> bool {
    starts(name)
        || matches!(
            name,
            "message_other_session" | "kick_user_session" | "message_spawn"
        )
}

/// Project defaults for legacy saved policies; explicit denials remain authoritative.
pub fn policy_defaults(mut policy: Value) -> Value {
    for name in ["message_other_session", "start_session"] {
        if policy["rules"].get(name).is_none() {
            policy["rules"][name] = json!("allow");
        }
    }
    policy
}

pub fn schemas() -> Vec<Value> {
    vec![
        json!({"type":"function","name":"message_other_session","description":"Send an attributed agent message to another owner-private session. Queue by default; steer at the next boundary; interrupt requires exact owner approval. Returns admission, not the recipient answer.","parameters":{"type":"object","properties":{"target_session_id":{"type":"string","format":"uuid"},"text":{"type":"string","minLength":1,"maxLength":65536},"delivery":{"type":"string","enum":["queue","steer","interrupt"]}},"required":["target_session_id","text"],"additionalProperties":false}}),
        json!({"type":"function","name":"start_session","description":"Create an owner-private session and its first attributed agent message atomically. Requires owner approval. Fresh context; omitted agent_id uses this agent. Returns admission, not the answer.","parameters":{"type":"object","properties":{"text":{"type":"string","minLength":1,"maxLength":65536},"agent_id":{"type":"string","maxLength":128},"title":{"type":"string","maxLength":200}},"required":["text"],"additionalProperties":false}}),
    ]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageArgs {
    pub target_session_id: String,
    pub text: String,
    #[serde(default = "queue")]
    pub delivery: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartArgs {
    pub text: String,
    pub agent_id: Option<String>,
    pub title: Option<String>,
}

fn queue() -> String {
    "queue".into()
}

pub fn checked_text(text: &str) -> Result<()> {
    if text.trim().is_empty()
        || text.len() > 65536
        || text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        bail!("Invalid message text");
    }
    Ok(())
}

pub fn stable_id(domain: &str, value: &Value) -> String {
    let hash = digest(&json!([domain, value]));
    let mut bytes = [0u8; 16];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hash[i * 2..i * 2 + 2], 16).unwrap();
    }
    bytes[6] = (bytes[6] & 15) | 80;
    bytes[8] = (bytes[8] & 63) | 128;
    uuid::Uuid::from_bytes(bytes).to_string()
}

#[derive(Debug)]
pub struct AuthorityChanged(pub &'static str);
impl std::fmt::Display for AuthorityChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for AuthorityChanged {}

pub fn authority_changed(reason: &'static str) -> anyhow::Error {
    AuthorityChanged(reason).into()
}

/// Capacity is checked under the same SQLite writer as child creation, including competing loop processes.
pub async fn require_spawn_capacity<C: RecordCodec>(
    codec: &C,
    tx: &mut Transaction<'_, Sqlite>,
    source: &str,
    limits: &Value,
) -> Result<()> {
    if limits.is_null() {
        return Ok(());
    }
    let depth = limits["depth"]
        .as_u64()
        .ok_or_else(|| anyhow!("Helper depth missing"))?;
    let max_depth = limits["max_depth"]
        .as_u64()
        .filter(|v| *v <= 5)
        .ok_or_else(|| anyhow!("Invalid helper depth limit"))?;
    let max_children = limits["max_children"]
        .as_u64()
        .filter(|v| *v <= 10)
        .ok_or_else(|| anyhow!("Invalid helper breadth limit"))?;
    let max_active = limits["max_active"]
        .as_u64()
        .filter(|v| (1..=20).contains(v))
        .ok_or_else(|| anyhow!("Invalid helper capacity"))?;
    if depth > max_depth {
        bail!("Helper recursion depth reached");
    }
    let prefix = format!("receipt:agent_message:{source}:");
    let rows = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT value FROM docs WHERE substr(key,1,length(?))=? LIMIT 1001",
    )
    .bind(&prefix)
    .bind(&prefix)
    .fetch_all(&mut **tx)
    .await?;
    if rows.len() > 1000 {
        bail!("Parent helper history quota reached");
    }
    let mut total = 0u64;
    let mut active = 0u64;
    for raw in rows {
        let receipt = codec.unseal(&raw)?;
        if receipt["result"]["session_created"] != true {
            continue;
        }
        total += 1;
        let id = receipt["result"]["session_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Child identity missing"))?;
        let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM run_index WHERE session_id=? AND status NOT IN ('completed','cancelled','failed','stopped')")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
        let q = crate::followup::queue_read_tx(codec, tx, id).await?;
        if runs > 0
            || q["entries"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|v| matches!(v["status"].as_str(), Some("pending" | "reviewing" | "held")))
        {
            active += 1;
        }
    }
    if active >= max_active || depth > 0 && total >= max_children {
        bail!("Helper capacity reached");
    }
    Ok(())
}
