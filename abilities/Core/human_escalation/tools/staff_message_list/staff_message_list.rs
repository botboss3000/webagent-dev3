//! staff_message_list: newest staff messages for the agent, optionally by status.
use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};
use webagent_host_core::StoreCore;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListArgs {
    #[serde(default)]
    limit: Option<u64>,
    #[serde(default)]
    status: Option<String>,
}

pub(super) async fn run(store: &StoreCore, args: &Value, agent: &str) -> Result<Value> {
    let query: ListArgs = webagent_abilities::tool_arguments::read("staff_message_list", args)?;
    let limit = query.limit.unwrap_or(20).clamp(1, 50) as usize;
    let mut messages = store.list(&format!("agent:{agent}:staff_message:")).await?;
    if let Some(status) = query.status {
        messages.retain(|m| m["status"] == status.as_str());
    }
    messages.sort_by_key(|m| std::cmp::Reverse(m["created_at"].as_u64().unwrap_or(0)));
    let total = messages.len();
    messages.truncate(limit);
    let output = json!({"messages":messages,"total":total,"authority":"visitor_supplied_data"});
    Ok(json!({"status":"success","output":output}))
}
