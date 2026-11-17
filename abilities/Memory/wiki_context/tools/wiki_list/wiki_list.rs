//! wiki_list: visible articles, newest first, optionally by category.
use super::summary;
use serde_json::{json, Value};

pub(super) fn run(args: &Value, visible: &[&Value]) -> Value {
    let category = args["category"].as_str().filter(|c| !c.is_empty());
    let mut items: Vec<&Value> = visible
        .iter()
        .filter(|a| category.is_none_or(|c| a["category"] == c))
        .copied()
        .collect();
    items.sort_by(|a, b| b["updated_at"].as_str().cmp(&a["updated_at"].as_str()));
    json!({"articles":items.iter().take(100).map(|a| summary(a)).collect::<Vec<_>>(),"total":items.len()})
}
