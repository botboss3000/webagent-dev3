//! wiki_history: an article's revisions, newest first.
use super::{revision_id, slug_arg, summary, REVISIONS};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use webagent_host_core::StoreCore;

pub(super) async fn run(store: &StoreCore, args: &Value, all: &[Value]) -> Result<Value> {
    let slug = slug_arg(args)?;
    let mut revisions = store.list(&format!("{REVISIONS}{slug}:")).await?;
    if revisions.is_empty() {
        bail!("Wiki article not found");
    }
    revisions.sort_by_key(|r| std::cmp::Reverse(r["revision"].as_u64()));
    let current = all.iter().find(|a| a["slug"] == slug);
    Ok(
        json!({"slug":slug,"current_revision":current.map(|a| &a["revision"]),
        "deleted":current.is_some_and(|a| a["deleted"] == true),"revisions":revisions.iter().take(50).map(|r| {
        let mut item = summary(r);
        item["revision_id"] = json!(revision_id(&slug, r["revision"].as_u64().unwrap_or(0)));
        item
    }).collect::<Vec<_>>()}),
    )
}
