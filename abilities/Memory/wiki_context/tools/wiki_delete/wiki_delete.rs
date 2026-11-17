//! wiki_delete: the tombstone that replaces a deleted article.
use serde_json::{json, Value};

pub(super) fn article(slug: &str) -> Value {
    json!({"slug":slug,"deleted":true})
}
