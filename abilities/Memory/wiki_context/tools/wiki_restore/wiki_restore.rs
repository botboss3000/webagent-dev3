//! wiki_restore: an earlier revision of the article, undeleted.
use super::REVISIONS;
use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::Value;
use webagent_host_core::StoreCore;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RestoreArgs {
    pub(super) slug: String,
    revision_id: String,
    pub(super) expected_revision: u64,
}

pub(super) async fn article(store: &StoreCore, args: &RestoreArgs, slug: &str) -> Result<Value> {
    let wanted = args
        .revision_id
        .strip_prefix(&format!("{slug}:"))
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
