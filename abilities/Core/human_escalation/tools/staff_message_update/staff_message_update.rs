//! staff_message_update: change a staff message's status and add a staff note.
use super::text;
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use webagent_host_core::{now, StoreCore};

const MESSAGE_STATUSES: [&str; 4] = ["new", "contacted", "resolved", "closed"];

pub(super) async fn run(
    store: &StoreCore,
    args: &Value,
    agent: &str,
    actor: &str,
    pending: &str,
) -> Result<Value> {
    let id = args["message_id"]
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| anyhow!("Invalid message ID"))?;
    let status = args["status"]
        .as_str()
        .filter(|s| MESSAGE_STATUSES.contains(s))
        .ok_or_else(|| anyhow!("Invalid status"))?;
    let key = format!("agent:{agent}:staff_message:{id}");
    let mut message = store
        .get(&key)
        .await?
        .ok_or_else(|| anyhow!("Message not found"))?;
    if message["last_pending_id"] == pending {
        return Ok(json!({"status":"success","output":message}));
    }
    if message["revision"].as_u64() != args["expected_revision"].as_u64() {
        bail!("Message changed; list it again before updating");
    }
    message["status"] = json!(status);
    message["revision"] = json!(message["revision"].as_u64().unwrap_or(1) + 1);
    message["last_pending_id"] = json!(pending);
    let note = text(args["note"].as_str().unwrap_or(""), 1000)?;
    if !note.is_empty() {
        let notes = message["notes"]
            .as_array_mut()
            .ok_or_else(|| anyhow!("Invalid message notes"))?;
        if notes.len() >= 50 {
            notes.remove(0);
        }
        notes.push(json!({"by":actor,"at":now(),"text":note}));
    }
    store.put(&key, &message).await?;
    Ok(json!({"status":"success","output":message}))
}
