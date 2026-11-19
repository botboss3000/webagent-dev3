//! knowledge_file_get: one knowledge file with its content.
use super::file_id;
use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use webagent_host_core::StoreCore;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GetArgs {
    file_id: String,
}

pub(super) async fn run(store: &StoreCore, args: &Value, prefix: &str) -> Result<Value> {
    let get: GetArgs = webagent_abilities::tool_arguments::read("knowledge_file_get", args)?;
    let id = file_id(&get.file_id)?;
    let output = store
        .get(&format!("{prefix}{id}"))
        .await?
        .ok_or_else(|| anyhow!("Knowledge file not found"))?;
    Ok(json!({"status":"success","output":output}))
}
