//! knowledge_file_save: create or replace a knowledge file at its expected revision.
use super::{file_id, summary};
use crate::Call;
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use webagent_host_core::{digest, now, StoreCore};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SaveArgs {
    name: String,
    content: String,
    expected_revision: u64,
    #[serde(default)]
    file_id: Option<String>,
}

const FILE_MAX_BYTES: usize = 200 * 1024;
const FILE_LIMIT: usize = 100;

pub(super) async fn run(
    store: &StoreCore,
    call: Call<'_>,
    args: &Value,
    agent: &str,
    actor: &str,
    prefix: &str,
) -> Result<Value> {
    let save: SaveArgs = webagent_abilities::tool_arguments::read("knowledge_file_save", args)?;
    let file_name = Some(save.name.trim())
        .filter(|n| !n.is_empty() && n.len() <= 120 && !n.chars().any(char::is_control))
        .ok_or_else(|| anyhow!("Invalid file name"))?;
    let content = save.content.as_str();
    if content.trim().is_empty() || content.len() > FILE_MAX_BYTES {
        bail!("Content must be 1 byte to 200 KiB of text");
    }
    let expected = save.expected_revision;
    let id = match &save.file_id {
        Some(value) => file_id(value)?,
        None => digest(&json!([agent, call.sid, call.run_id, call.pending_id]))[..24].to_owned(),
    };
    let file_key = format!("{prefix}{id}");
    // shortcut: read-compare-write is not atomic across the API and loop processes;
    // acceptable for staff edits, move to a conditional agent-plane write if edits become concurrent.
    let current = store.get(&file_key).await?;
    if let Some(file) = current
        .as_ref()
        .filter(|f| f["pending_id"] == call.pending_id)
    {
        return Ok(json!({"status":"success","output":summary(file)}));
    }
    let revision = current
        .as_ref()
        .and_then(|f| f["revision"].as_u64())
        .unwrap_or(0);
    if revision != expected {
        bail!("Knowledge file changed; list it again before saving");
    }
    if current.is_none() && store.list(prefix).await?.len() >= FILE_LIMIT {
        bail!("Knowledge file limit (100) reached");
    }
    let saved = json!({"id":id,"name":file_name,"content":content,"revision":revision+1,"updated_by":actor,"updated_at":now(),"pending_id":call.pending_id});
    store.put(&file_key, &saved).await?;
    Ok(json!({"status":"success","output":summary(&saved)}))
}
