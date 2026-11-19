//! wiki_delete: the tombstone that replaces a deleted article.
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteArgs {
    pub(super) slug: String,
    pub(super) expected_revision: u64,
}

pub(super) fn article(slug: &str) -> Value {
    json!({"slug":slug,"deleted":true})
}
