//! knowledge_file_delete: remove a knowledge file at its expected revision.
use super::file_id;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use webagent_host_core::StoreCore;

pub(super) async fn run(store: &StoreCore, args: &Value, prefix: &str) -> Result<Value> {
    let id = file_id(&args["file_id"])?;
    let file_key = format!("{prefix}{id}");
    let output = match store.get(&file_key).await? {
        None => json!({"file_id":id,"deleted":true}),
        Some(file) if file["revision"].as_u64() != args["expected_revision"].as_u64() => {
            bail!("Knowledge file changed; list it again before deleting")
        }
        Some(_) => {
            store.delete(&file_key).await?;
            json!({"file_id":id,"deleted":true})
        }
    };
    Ok(json!({"status":"success","output":output}))
}
