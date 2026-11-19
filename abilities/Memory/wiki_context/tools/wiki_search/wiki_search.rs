//! wiki_search: scored search over visible articles with alternate terms.
use super::{excerpt, list_arg, summary};
use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SearchArgs {
    query: String,
    #[serde(default)]
    also: Option<Vec<String>>,
    #[serde(default)]
    limit: Option<u64>,
}

pub(super) fn run(args: &Value, visible: &[&Value]) -> Result<Value> {
    let search_args: SearchArgs = webagent_abilities::tool_arguments::read("wiki_search", args)?;
    let limit = search_args.limit.unwrap_or(5).clamp(1, 10) as usize;
    let also = search_args
        .also
        .as_deref()
        .map(|v| list_arg(v, 8, 100, "alternate terms"))
        .transpose()?;
    // Alternate phrasings join the query; the caller supplies synonyms, not a model call.
    let query = also
        .iter()
        .flat_map(|v| v.as_array().into_iter().flatten())
        .fold(search_args.query, |mut query, term| {
            query.push(' ');
            query.push_str(term.as_str().unwrap_or(""));
            query
        });
    Ok(json!({"results":search(visible, &query, limit)}))
}

/// Score by distinct words of the query and its alternate terms: a title hit counts three,
/// a tag or keyword two, the body one, so an article matching several phrasings ranks first.
fn search(articles: &[&Value], query: &str, limit: usize) -> Vec<Value> {
    let mut words: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1)
        .map(str::to_lowercase)
        .collect();
    words.sort();
    words.dedup();
    let mut hits: Vec<(usize, Value)> = articles
        .iter()
        .filter_map(|article| {
            let title = article["title"].as_str().unwrap_or("").to_lowercase();
            let tags = format!("{} {}", article["tags"], article["keywords"]).to_lowercase();
            let body = article["body"].as_str().unwrap_or("");
            let lower = body.to_lowercase();
            let score: usize = words
                .iter()
                .map(|w| {
                    3 * usize::from(title.contains(w.as_str()))
                        + 2 * usize::from(tags.contains(w.as_str()))
                        + usize::from(lower.contains(w.as_str()))
                })
                .sum();
            if score == 0 {
                return None;
            }
            // Lowercasing can shift byte offsets for some scripts; excerpt() snaps them.
            let at = words
                .iter()
                .filter_map(|w| lower.find(w.as_str()))
                .min()
                .unwrap_or(0);
            let mut hit = summary(article);
            hit["snippet"] = json!(excerpt(body, at.saturating_sub(200), 600));
            hit["score"] = json!(score);
            Some((score, hit))
        })
        .collect();
    hits.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    hits.into_iter().take(limit).map(|(_, hit)| hit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_weights_title_over_body_and_snaps_excerpts() {
        let a = json!({"slug":"refunds","title":"Refund policy","tags":["billing"],"body":"Ünïcode refunds within 30 days."});
        let b =
            json!({"slug":"menu","title":"Menu","tags":[],"body":"Ask about refund on drinks."});
        let hits = search(&[&b, &a], "refund", 5);
        assert_eq!(hits[0]["slug"], "refunds");
        assert_eq!(hits.len(), 2);
        assert!(search(&[&a], "parking", 5).is_empty());
        let c = json!({"slug":"returns","title":"Returns","tags":[],"keywords":["money back"],"body":"Bring a receipt."});
        assert_eq!(search(&[&c, &b], "money", 5)[0]["slug"], "returns");
    }
}
