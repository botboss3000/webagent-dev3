//! knowledge_file_search: rank knowledge files by query words with excerpts.
use super::summary;
use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};
use webagent_host_core::StoreCore;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SearchArgs {
    query: String,
    #[serde(default)]
    limit: Option<u64>,
}

/// Score by how many distinct query words appear; excerpt around the first hit.
fn search(files: &[Value], query: &str, limit: usize) -> Vec<Value> {
    let words: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1)
        .map(str::to_lowercase)
        .collect();
    let mut hits: Vec<(usize, Value)> = files
        .iter()
        .filter_map(|file| {
            let content = file["content"].as_str().unwrap_or("");
            let haystack =
                format!("{}\n{}", file["name"].as_str().unwrap_or(""), content).to_lowercase();
            let score = words
                .iter()
                .filter(|w| haystack.contains(w.as_str()))
                .count();
            if score == 0 {
                return None;
            }
            let lower = content.to_lowercase();
            let at = words
                .iter()
                .filter_map(|w| lower.find(w.as_str()))
                .min()
                .unwrap_or(0);
            // Lowercasing can shift byte offsets for some scripts; snap to char boundaries.
            let mut start = at.saturating_sub(200).min(content.len());
            while !content.is_char_boundary(start) {
                start -= 1;
            }
            let mut end = (start + 600).min(content.len());
            while !content.is_char_boundary(end) {
                end -= 1;
            }
            let mut hit = summary(file);
            hit["excerpt"] = json!(&content[start..end]);
            hit["matched_words"] = json!(score);
            Some((score, hit))
        })
        .collect();
    hits.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    hits.into_iter().take(limit).map(|(_, hit)| hit).collect()
}
pub(super) async fn run(store: &StoreCore, args: &Value, prefix: &str) -> Result<Value> {
    let request: SearchArgs =
        webagent_abilities::tool_arguments::read("knowledge_file_search", args)?;
    let limit = request.limit.unwrap_or(5).clamp(1, 10) as usize;
    let output = json!({"results":search(&store.list(prefix).await?, &request.query, limit),"authority":"operator_supplied_reference"});
    Ok(json!({"status":"success","output":output}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_ranks_by_distinct_words_with_excerpts() {
        let files = vec![
            json!({"id":"a1","name":"Menu","revision":1,"content":"Tonkotsu ramen 16. Vegan miso ramen 15."}),
            json!({"id":"b2","name":"Policies","revision":1,"content":"No reservations; walk-ins only."}),
        ];
        let hits = search(&files, "vegan ramen?", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["file_id"], "a1");
        assert_eq!(hits[0]["matched_words"], 2);
        assert!(hits[0]["excerpt"].as_str().unwrap().contains("Vegan miso"));
        assert!(search(&files, "parking", 5).is_empty());
        assert_eq!(
            search(
                &[json!({"id":"c","name":"x","content":"Ünïcode café menu"})],
                "menu",
                1
            )
            .len(),
            1
        );
    }
}
