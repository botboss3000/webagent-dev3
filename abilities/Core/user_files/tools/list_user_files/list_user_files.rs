//! list_user_files: pages through the owner's saved file metadata.
use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};
use webagent_abilities::user_files::Repository;
use webagent_host_core::StoreCore;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct List {
    offset: Option<u32>,
    limit: Option<u32>,
}
pub(super) async fn run(args: &Value, repo: &Repository<'_, StoreCore>) -> Result<Value> {
    let q: List = serde_json::from_value(args.clone())?;
    let offset = q.offset.unwrap_or(0);
    let limit = q.limit.unwrap_or(50);
    let files = repo.list(offset, limit).await?;
    let next = if files.len() == limit as usize {
        Some(offset.saturating_add(limit))
    } else {
        None
    };
    Ok(json!({"files":files,"next_offset":next}))
}
