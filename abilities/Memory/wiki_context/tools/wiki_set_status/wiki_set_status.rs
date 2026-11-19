//! wiki_set_status: the live article with a new draft or published status.
use super::status_arg;
use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StatusArgs {
    pub(super) slug: String,
    status: String,
    pub(super) expected_revision: u64,
}

pub(super) fn article(args: &StatusArgs, live: Option<&Value>) -> Result<Value> {
    let mut article = live.cloned().unwrap_or_default();
    article["status"] = json!(status_arg(&args.status)?);
    Ok(article)
}
