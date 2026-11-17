//! wiki_restore: an earlier revision of the article, undeleted.
use super::REVISIONS;
use anyhow::{anyhow, Result};
use serde_json::Value;
use webagent_host_core::StoreCore;

pub(super) async fn article(store: &StoreCore, args: &Value, slug: &str) -> Result<Value> {
    let wanted = args["revision_id"]
        .as_str()
        .and_then(|id| id.strip_prefix(&format!("{slug}:")))
        .and_then(|n| n.parse::<u64>().ok())
        .ok_or_else(|| anyhow!("Invalid revision ID for this article"))?;
    let mut old = store
        .get(&format!("{REVISIONS}{slug}:{wanted:06}"))
        .await?
        .ok_or_else(|| anyhow!("Wiki revision not found"))?;
    if let Some(map) = old.as_object_mut() {
        map.remove("deleted");
    }
    Ok(old)
}
