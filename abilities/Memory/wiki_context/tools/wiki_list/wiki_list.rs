//! wiki_list: visible articles, newest first, optionally by category.
use super::summary;
use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListArgs {
    #[serde(default)]
    category: Option<String>,
}

pub(super) fn run(args: &Value, visible: &[&Value]) -> Result<Value> {
    let list: ListArgs = webagent_abilities::tool_arguments::read("wiki_list", args)?;
    let category = list.category.as_deref().filter(|c| !c.is_empty());
    let mut items: Vec<&Value> = visible
        .iter()
        .filter(|a| category.is_none_or(|c| a["category"] == c))
        .copied()
        .collect();
    items.sort_by(|a, b| b["updated_at"].as_str().cmp(&a["updated_at"].as_str()));
    Ok(
        json!({"articles":items.iter().take(100).map(|a| summary(a)).collect::<Vec<_>>(),"total":items.len()}),
    )
}
