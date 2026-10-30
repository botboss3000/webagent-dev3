//! Agent-owned text knowledge files (menus, price lists, policies, FAQs).
//! Staff save and delete; every session of the agent can list, search and read.
use crate::{Call, NativeHost};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_access::Relationship;
use webagent_host_core::{digest, now};

const FILE_MAX_BYTES: usize = 200 * 1024;
const FILE_LIMIT: usize = 100;

#[must_use]
pub fn owns(name: &str) -> bool {
    TOOLS.contains(&name)
}
/// Tools this module executes; their schemas live in the ability's `tools/<tool>/tool.json`.
pub const TOOLS: &[&str] = &[
    "knowledge_file_list",
    "knowledge_file_search",
    "knowledge_file_get",
    "knowledge_file_save",
    "knowledge_file_delete",
];

fn file_id(value: &Value) -> Result<String> {
    value
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("Invalid file ID"))
}
fn summary(file: &Value) -> Value {
    json!({"file_id":file["id"],"name":file["name"],"revision":file["revision"],"bytes":file["content"].as_str().map_or(0,str::len),"updated_at":file["updated_at"]})
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

/// Runs one knowledge file tool in the run's agent plane; writes need an owner or member.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let run = host.authorize(call, name, args, &cancel).await?;
    let agent = run["agent_id"]
        .as_str()
        .unwrap_or("shared_default")
        .to_owned();
    let actor = host.actor().to_owned();
    if !crate::is_read(name)
        && !matches!(
            host.relationship(call.sid, &agent).await?,
            Relationship::Owner | Relationship::Member
        )
    {
        return Ok(json!({"status":"denied","output":"Staff access required"}));
    }
    let store = host.agent_store(&agent).await?;
    let prefix = format!("agent:{agent}:knowledge:");
    let output = match name {
        "knowledge_file_list" => {
            json!({"files":store.list(&prefix).await?.iter().map(summary).collect::<Vec<_>>()})
        }
        "knowledge_file_search" => {
            let limit = args["limit"].as_u64().unwrap_or(5).clamp(1, 10) as usize;
            json!({"results":search(&store.list(&prefix).await?, args["query"].as_str().unwrap_or(""), limit),"authority":"operator_supplied_reference"})
        }
        "knowledge_file_get" => {
            let id = file_id(&args["file_id"])?;
            store
                .get(&format!("{prefix}{id}"))
                .await?
                .ok_or_else(|| anyhow!("Knowledge file not found"))?
        }
        "knowledge_file_save" => {
            let file_name = args["name"]
                .as_str()
                .map(str::trim)
                .filter(|n| !n.is_empty() && n.len() <= 120 && !n.chars().any(char::is_control))
                .ok_or_else(|| anyhow!("Invalid file name"))?;
            let content = args["content"]
                .as_str()
                .ok_or_else(|| anyhow!("Content required"))?;
            if content.trim().is_empty() || content.len() > FILE_MAX_BYTES {
                bail!("Content must be 1 byte to 200 KiB of text");
            }
            let expected = args["expected_revision"]
                .as_u64()
                .ok_or_else(|| anyhow!("Revision required"))?;
            let id = match args.get("file_id").filter(|v| !v.is_null()) {
                Some(value) => file_id(value)?,
                None => {
                    digest(&json!([agent, call.sid, call.run_id, call.pending_id]))[..24].to_owned()
                }
            };
            let file_key = format!("{prefix}{id}");
            // shortcut: read-compare-write is not atomic across the API and loop processes;
            // acceptable for staff edits, move to a conditional agent-plane write if edits become concurrent.
            let current = store.get(&file_key).await?;
            if let Some(file) = current
                .as_ref()
                .filter(|f| f["pending_id"] == call.pending_id)
            {
                return Ok(json!({"status":"success","output":summary(file)}));
            }
            let revision = current
                .as_ref()
                .and_then(|f| f["revision"].as_u64())
                .unwrap_or(0);
            if revision != expected {
                bail!("Knowledge file changed; list it again before saving");
            }
            if current.is_none() && store.list(&prefix).await?.len() >= FILE_LIMIT {
                bail!("Knowledge file limit (100) reached");
            }
            let saved = json!({"id":id,"name":file_name,"content":content,"revision":revision+1,"updated_by":actor,"updated_at":now(),"pending_id":call.pending_id});
            store.put(&file_key, &saved).await?;
            summary(&saved)
        }
        "knowledge_file_delete" => {
            let id = file_id(&args["file_id"])?;
            let file_key = format!("{prefix}{id}");
            match store.get(&file_key).await? {
                None => json!({"file_id":id,"deleted":true}),
                Some(file) if file["revision"].as_u64() != args["expected_revision"].as_u64() => {
                    bail!("Knowledge file changed; list it again before deleting")
                }
                Some(_) => {
                    store.delete(&file_key).await?;
                    json!({"file_id":id,"deleted":true})
                }
            }
        }
        _ => bail!("Unknown knowledge file tool"),
    };
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
    #[test]
    fn file_ids_are_bounded_hex() {
        assert!(file_id(&json!("../x")).is_err());
        assert!(file_id(&json!("")).is_err());
        assert_eq!(file_id(&json!("ab12")).unwrap(), "ab12");
    }
}
