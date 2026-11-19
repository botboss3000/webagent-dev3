//! knowledge_file_delete: remove a knowledge file at its expected revision.
use super::file_id;
use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use webagent_host_core::StoreCore;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteArgs {
    file_id: String,
    expected_revision: u64,
}

pub(super) async fn run(store: &StoreCore, args: &Value, prefix: &str) -> Result<Value> {
    let delete: DeleteArgs =
        webagent_abilities::tool_arguments::read("knowledge_file_delete", args)?;
    let id = file_id(&delete.file_id)?;
    let file_key = format!("{prefix}{id}");
    let output = match store.get(&file_key).await? {
        None => json!({"file_id":id,"deleted":true}),
        Some(file) if file["revision"].as_u64() != Some(delete.expected_revision) => {
            bail!("Knowledge file changed; list it again before deleting")
        }
        Some(_) => {
            store.delete(&file_key).await?;
            json!({"file_id":id,"deleted":true})
        }
    };
    Ok(json!({"status":"success","output":output}))
}
