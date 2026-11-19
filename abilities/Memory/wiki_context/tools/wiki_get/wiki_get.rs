//! wiki_get: one visible article without its write marker.
use super::{article_slug, find};
use anyhow::{anyhow, Result};
use serde_json::Value;

pub(super) fn run(args: &Value, visible: &[&Value]) -> Result<Value> {
    let slug = article_slug("wiki_get", args)?;
    let article = find(visible, &slug).ok_or_else(|| anyhow!("Wiki article not found"))?;
    let mut article = article.clone();
    if let Some(map) = article.as_object_mut() {
        map.remove("pending_id");
    }
    Ok(article)
}
