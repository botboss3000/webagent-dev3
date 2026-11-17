//! wiki_set_status: the live article with a new draft or published status.
use super::status_arg;
use anyhow::Result;
use serde_json::{json, Value};

pub(super) fn article(args: &Value, live: Option<&Value>) -> Result<Value> {
    let mut article = live.cloned().unwrap_or_default();
    article["status"] = json!(status_arg(&args["status"])?);
    Ok(article)
}
