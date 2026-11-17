//! knowledge_file_list: summaries of every knowledge file of the agent.
use super::summary;
use anyhow::Result;
use serde_json::{json, Value};
use webagent_host_core::StoreCore;

pub(super) async fn run(store: &StoreCore, prefix: &str) -> Result<Value> {
    let output = json!({"files":store.list(prefix).await?.iter().map(summary).collect::<Vec<_>>()});
    Ok(json!({"status":"success","output":output}))
}
