//! wiki_update: the live article with the given fields replaced.
use super::{body_arg, category_arg, keywords_arg, status_arg, tags_arg, title_arg};
use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdateArgs {
    pub(super) slug: String,
    pub(super) expected_revision: u64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    keywords: Option<Vec<String>>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

pub(super) fn article(args: &UpdateArgs, live: Option<&Value>) -> Result<Value> {
    let mut article = live.cloned().unwrap_or_default();
    for (field, value) in [
        (
            "title",
            args.title
                .as_deref()
                .map(title_arg)
                .transpose()?
                .map(Value::from),
        ),
        (
            "body",
            args.body
                .as_deref()
                .map(body_arg)
                .transpose()?
                .map(Value::from),
        ),
        ("tags", args.tags.as_deref().map(tags_arg).transpose()?),
        (
            "keywords",
            args.keywords.as_deref().map(keywords_arg).transpose()?,
        ),
        (
            "category",
            args.category
                .as_deref()
                .map(category_arg)
                .transpose()?
                .map(Value::from),
        ),
        (
            "status",
            args.status
                .as_deref()
                .map(status_arg)
                .transpose()?
                .map(Value::from),
        ),
    ] {
        if let Some(value) = value {
            article[field] = value;
        }
    }
    Ok(article)
}
