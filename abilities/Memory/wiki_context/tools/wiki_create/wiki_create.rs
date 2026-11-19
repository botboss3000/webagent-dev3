//! wiki_create: slug from the title and the new article's fields.
use super::{body_arg, category_arg, keywords_arg, status_arg, tags_arg, title_arg, ARTICLE_LIMIT};
use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use webagent_host_core::{digest, now};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateArgs {
    pub(super) title: String,
    body: String,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    keywords: Option<Vec<String>>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

/// The slug for `title`.
pub(super) fn slug(title: &str) -> Result<String> {
    let title = title_arg(title)?;
    let slug = slugify(&title);
    // Titles without ASCII words still get a stable slug.
    Ok(if slug.is_empty() {
        digest(&json!(title))[..12].to_owned()
    } else {
        slug
    })
}

pub(super) fn article(
    args: &CreateArgs,
    slug: &str,
    live: Option<&Value>,
    visible: usize,
    actor: &str,
) -> Result<Value> {
    if live.is_some() {
        bail!("A Wiki article with this title exists: {slug}; use wiki_update");
    }
    if visible >= ARTICLE_LIMIT {
        bail!("Wiki article limit (1000) reached");
    }
    let tags = args.tags.as_deref().map(tags_arg).transpose()?;
    let keywords = args.keywords.as_deref().map(keywords_arg).transpose()?;
    let category = args.category.as_deref().map(category_arg).transpose()?;
    let status = args.status.as_deref().map(status_arg).transpose()?;
    Ok(
        json!({"slug":slug,"title":title_arg(&args.title)?,"body":body_arg(&args.body)?,
        "tags":tags.unwrap_or_else(|| json!([])),"keywords":keywords.unwrap_or_else(|| json!([])),"category":category.unwrap_or_default(),
        "status":status.unwrap_or("draft"),"created_by":actor,"created_at":now()}),
    )
}

/// Lowercase ASCII words of `title` joined by hyphens.
fn slugify(title: &str) -> String {
    let words: Vec<String> = title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let mut slug = words.join("-");
    slug.truncate(80);
    slug.trim_end_matches('-').to_owned()
}

#[cfg(test)]
mod tests {
    use super::super::valid_slug;
    use super::*;
    #[test]
    fn slugs_are_bounded_ascii_words() {
        assert_eq!(
            slugify("  Café Opening Hours (2026)! "),
            "caf-opening-hours-2026"
        );
        assert_eq!(slugify("!!!"), "");
        assert!(valid_slug("opening-hours"));
        assert!(!valid_slug("../x"));
        assert!(!valid_slug("Upper"));
        assert!(slugify(&"word ".repeat(40)).len() <= 80);
    }
}
