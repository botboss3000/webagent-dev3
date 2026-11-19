//! wiki_backlinks: visible articles that link to one article.
use super::{article_slug, find};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};

pub(super) fn run(args: &Value, visible: &[&Value]) -> Result<Value> {
    let slug = article_slug("wiki_backlinks", args)?;
    let target = find(visible, &slug).ok_or_else(|| anyhow!("Wiki article not found"))?;
    let title = target["title"].as_str().unwrap_or("");
    Ok(json!({"slug":slug,"linked_from":visible.iter()
        .filter(|a| a["slug"] != slug.as_str() && links_to(a["body"].as_str().unwrap_or(""), &slug, title))
        .map(|a| json!({"slug":a["slug"],"title":a["title"]})).collect::<Vec<_>>()}))
}

/// Whether `body` links `[[slug]]` or `[[title]]`, ignoring case.
fn links_to(body: &str, slug: &str, title: &str) -> bool {
    let body = body.to_lowercase();
    [slug, title]
        .iter()
        .any(|target| body.contains(&format!("[[{}]]", target.to_lowercase())))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backlinks_match_slug_or_title() {
        assert!(links_to(
            "See [[Refund Policy]].",
            "refunds",
            "Refund policy"
        ));
        assert!(links_to("See [[refunds]].", "refunds", "Refund policy"));
        assert!(!links_to("See refunds.", "refunds", "Refund policy"));
    }
}
