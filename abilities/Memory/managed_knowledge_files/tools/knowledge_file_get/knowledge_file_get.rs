//! knowledge_file_get: one knowledge file with its content.
use super::file_id;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use webagent_host_core::StoreCore;

pub(super) async fn run(store: &StoreCore, args: &Value, prefix: &str) -> Result<Value> {
    let id = file_id(&args["file_id"])?;
    let output = store
        .get(&format!("{prefix}{id}"))
        .await?
        .ok_or_else(|| anyhow!("Knowledge file not found"))?;
    Ok(json!({"status":"success","output":output}))
}
