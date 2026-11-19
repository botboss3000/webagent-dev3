//! wiki_get_revision: one revision snapshot by revision ID.
use super::{revision_id, valid_slug, REVISIONS};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use webagent_host_core::StoreCore;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RevisionArgs {
    revision_id: String,
}

pub(super) async fn run(store: &StoreCore, args: &Value) -> Result<Value> {
    let wanted: RevisionArgs = webagent_abilities::tool_arguments::read("wiki_get_revision", args)?;
    let (slug, revision) = wanted
        .revision_id
        .rsplit_once(':')
        .and_then(|(slug, n)| Some((slug, n.parse::<u64>().ok()?)))
        .filter(|(slug, _)| valid_slug(slug))
        .ok_or_else(|| anyhow!("Invalid revision ID; use one from wiki_history"))?;
    let mut snapshot = store
        .get(&format!("{REVISIONS}{slug}:{revision:06}"))
        .await?
        .ok_or_else(|| anyhow!("Wiki revision not found"))?;
    if let Some(map) = snapshot.as_object_mut() {
        map.remove("pending_id");
    }
    snapshot["revision_id"] = json!(revision_id(slug, revision));
    Ok(snapshot)
}
