//! wiki_update: the live article with the given fields replaced.
use super::{body_arg, category_arg, given, keywords_arg, status_arg, tags_arg, title_arg};
use anyhow::Result;
use serde_json::Value;

pub(super) fn article(args: &Value, live: Option<&Value>) -> Result<Value> {
    let mut article = live.cloned().unwrap_or_default();
    for (field, value) in [
        (
            "title",
            given(args, "title")
                .map(title_arg)
                .transpose()?
                .map(Value::from),
        ),
        (
            "body",
            given(args, "body")
                .map(body_arg)
                .transpose()?
                .map(Value::from),
        ),
        ("tags", given(args, "tags").map(tags_arg).transpose()?),
        (
            "keywords",
            given(args, "keywords").map(keywords_arg).transpose()?,
        ),
        (
            "category",
            given(args, "category")
                .map(category_arg)
                .transpose()?
                .map(Value::from),
        ),
        (
            "status",
            given(args, "status")
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
