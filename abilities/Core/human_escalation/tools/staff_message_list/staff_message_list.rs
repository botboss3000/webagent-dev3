//! staff_message_list: newest staff messages for the agent, optionally by status.
use anyhow::Result;
use serde_json::{json, Value};
use webagent_host_core::StoreCore;

pub(super) async fn run(store: &StoreCore, args: &Value, agent: &str) -> Result<Value> {
    let limit = args["limit"].as_u64().unwrap_or(20).clamp(1, 50) as usize;
    let mut messages = store.list(&format!("agent:{agent}:staff_message:")).await?;
    if let Some(status) = args["status"].as_str() {
        messages.retain(|m| m["status"] == status);
    }
    messages.sort_by_key(|m| std::cmp::Reverse(m["created_at"].as_u64().unwrap_or(0)));
    let total = messages.len();
    messages.truncate(limit);
    let output = json!({"messages":messages,"total":total,"authority":"visitor_supplied_data"});
    Ok(json!({"status":"success","output":output}))
}
