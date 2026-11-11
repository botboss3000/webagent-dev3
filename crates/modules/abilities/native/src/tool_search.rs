//! Exact and word-ranked tool discovery. Rust alone controls exposure, grants and approvals.
// @code-index {"version":1,"feature":"conversation-tool-selection","aliases":["live context","source correspondence"],"purpose":"Identity-only conversation tool selection never grants access. Current encrypted policy, visibility and model appointment control eligibility; native fences and endpoint confinement are fixed invariants.","span_lines":80,"configuration":{"status":"not_applicable","references":[],"opportunities":[],"rationale":"Model appointment and encrypted tool policy are resolved by their owners. Native receipts, fixed-endpoint confinement and current-permission fences cannot be made optional by another setting."}}
use crate::{Call, NativeHost};
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio_util::sync::CancellationToken;
use webagent_abilities::{near_word, search_words, tool_match};
use webagent_access::Permission;
use webagent_host_core::{key, StoreCore};
/// Route only fixed native discovery operations.
#[must_use]
pub fn owns(name: &str) -> bool {
    matches!(name, "list_tools" | "load_tool")
}
/// Describe bounded conversation selection without granting execution.
#[must_use]
pub fn schemas() -> Vec<Value> {
    vec![
        json!({"type":"function","name":"list_tools","description":"Find permitted tools. query: a few task words (for example \"edit file\"); results are ranked with a score, best_match first, never empty, and include tools you already have. Without query: the tools that need loading, optionally in one group (ability ID from the index); include_available=true adds tools you already have.","parameters":{"type":"object","properties":{"query":{"type":"string","maxLength":4096},"group":{"type":"string","maxLength":128},"include_available":{"type":"boolean"}},"required":[],"additionalProperties":false}}),
        json!({"type":"function","name":"load_tool","description":"Load one permitted tool by its exact name for this conversation; call it several times in parallel to load several. Already supplied tools return an acknowledgment. Live policy still applies.","parameters":{"type":"object","properties":{"tool_name":{"type":"string","minLength":1,"maxLength":128}},"required":["tool_name"],"additionalProperties":false}}),
    ]
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    include_available: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoadArgs {
    tool_name: String,
}
/// Validate exact-catalog query text without accepting extra scope fields.
fn list_args(value: &Value) -> Result<ListArgs> {
    let args: ListArgs = serde_json::from_value(value.clone())?;
    if args.query.as_ref().is_some_and(|q| {
        q.len() > 4096 || q.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
    }) {
        bail!("Invalid query");
    }
    Ok(args)
}
/// Accept one bounded exact native or plugin tool name.
fn load_args(value: &Value) -> Result<LoadArgs> {
    let args: LoadArgs = serde_json::from_value(value.clone())?;
    if args.tool_name.is_empty()
        || args.tool_name.len() > 128
        || !args
            .tool_name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
    {
        bail!("Invalid tool_name");
    }
    Ok(args)
}
pub use webagent_abilities::selection::{apply_selection, marker};
/// The stored run `id`.
async fn run(store: &StoreCore, id: &str) -> Result<Value> {
    store
        .get(&key("run", id))
        .await?
        .ok_or_else(|| anyhow!("Run not found"))
}
/// Whether the dispatch shutoff is engaged; a missing control record counts as engaged.
async fn shutoff(store: &StoreCore) -> Result<bool> {
    Ok(store
        .get("control")
        .await?
        .unwrap_or(json!({"shutoff":true}))["shutoff"]
        .as_bool()
        .unwrap_or(true))
}
/// Rebuild permitted availability from current authority and call-local identities.
async fn eligible(
    host: &dyn NativeHost,
    sid: &str,
    agent: &str,
    loaded: &Value,
) -> Result<Vec<Value>> {
    let access = host.session_access(sid, agent).await?;
    if !access.admitted() {
        bail!("Agent access unavailable");
    }
    let (policy, _, visibility, state) = host.discovery_controls(sid, agent).await?;
    // Native catalog reads share the model call's conversation activation view.
    let state = host.selected_state(&visibility, &state, loaded);
    let read_only = host.session_mode(sid).await?["mode"] == "read_only";
    let mut tools = BTreeMap::new();
    for entry in host.catalog_with(&policy, &visibility, &state) {
        if access.ability_permission(entry["id"].as_str().unwrap_or("")) == Permission::Deny {
            continue;
        }
        for tool in entry["tools"].as_array().context("Invalid tool catalog")? {
            let name = tool["name"].as_str().context("Invalid tool name")?;
            if owns(name) {
                continue;
            }
            let decision = host.live_tool_decision(
                &access,
                name,
                &policy,
                &visibility,
                &state,
                &json!([marker(name)]),
                read_only,
            );
            if decision.permission == Permission::Deny {
                continue;
            }
            let description = host
                .all_schemas()
                .into_iter()
                .find(|schema| schema["name"] == name)
                .and_then(|schema| schema["description"].as_str().map(str::to_owned))
                .unwrap_or_default();
            let (_, visible) = host.tool_gate(name, &visibility, &state, loaded);
            let selected = loaded
                .as_array()
                .is_some_and(|items| items.contains(&json!(marker(name))));
            let availability = if visible {
                if selected {
                    "activated"
                } else {
                    "visible"
                }
            } else {
                "discoverable"
            };
            tools.entry(name.to_owned()).or_insert_with(||json!({"availability":availability,"requires_activation":!visible,"name":name,"description":description.chars().take(1024).collect::<String>(),"ability_id":entry["id"],"approval_required":decision.permission==Permission::Ask}));
        }
    }
    if tools.len() > 512 {
        bail!("Tool catalog exceeds bound");
    }
    Ok(tools.into_values().collect())
}
/// Rank permitted tools for a task query: matcher scores when the matcher answers, otherwise
/// query-word overlap. Already-available tools stay in, marked by their `availability`.
async fn ranked(
    host: &dyn NativeHost,
    query: &str,
    tools: Vec<Value>,
) -> (Vec<Value>, &'static str) {
    let hints: Vec<(Vec<String>, Vec<String>)> = tools
        .iter()
        .map(|tool| tool_match::hints(tool["name"].as_str().unwrap_or_default()))
        .collect();
    let items = tools
        .iter()
        .zip(&hints)
        .map(|(tool, (examples, _))| {
            tool_match::texts(
                tool["name"].as_str().unwrap_or_default(),
                tool["description"].as_str().unwrap_or_default(),
                examples,
            )
        })
        .collect();
    let (scores, ranked_by) = match host.rank_scores(query, items).await {
        Some(scores) if scores.len() == tools.len() => (scores, "matcher"),
        _ => {
            let mut words = search_words(query);
            words.retain(|word| word.len() > 2); // "a", "to", "of" would match every description
                                                 // Word hits land between the weak and best gates; all words in the name reach best.
            let scale = |hits: usize| {
                if hits == 0 {
                    0.0
                } else {
                    let share = (hits as f32 / (3 * words.len().max(1)) as f32).min(1.0);
                    tool_match::WEAK + (tool_match::BEST - tool_match::WEAK) * share
                }
            };
            (
                tools
                    .iter()
                    .map(|tool| scale(word_score(tool, &words)))
                    .collect(),
                "words",
            )
        }
    };
    let candidates: Vec<(&str, &[String])> = tools
        .iter()
        .zip(&hints)
        .map(|(tool, (_, keywords))| (tool["name"].as_str().unwrap_or_default(), &keywords[..]))
        .collect();
    let rows = tool_match::rank(query, &candidates, &scores)
        .into_iter()
        .map(|ranked| {
            let mut row = tools[ranked.index].clone();
            row["score"] = json!((ranked.score * 100.0).round() / 100.0);
            row["best_match"] = json!(ranked.best);
            if ranked.weak {
                row["match"] = json!("weak");
            }
            row
        })
        .collect();
    (rows, ranked_by)
}
/// Count query words found in a tool's name (weighted), description or group.
fn word_score(tool: &Value, words: &[String]) -> usize {
    let text = |field: &str| search_words(tool[field].as_str().unwrap_or_default());
    let (name, rest) = (
        text("name"),
        [text("description"), text("ability_id")].concat(),
    );
    words
        .iter()
        .map(|word| {
            if name.iter().any(|w| near_word(word, w)) {
                3
            } else {
                usize::from(rest.iter().any(|w| near_word(word, w)))
            }
        })
        .sum()
}
/// Route discovery locally.
///
/// # Errors
/// Never for a known tool: failures become a denied or failed result.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    match exact(host, call.sid, call.run_id, name, args, &cancel).await {
        Ok(result) => Ok(result),
        Err(_) => Ok(
            json!({"status":"denied","output":"Invalid arguments, unavailable tool, or changed authority. Selection unchanged."}),
        ),
    }
}
// Exact catalog operations use fresh Rust authority, without a scoring model.
async fn exact(
    host: &dyn NativeHost,
    sid: &str,
    run_id: &str,
    name: &str,
    args: &Value,
    cancel: &CancellationToken,
) -> Result<Value> {
    let list = if name == "list_tools" {
        Some(list_args(args)?)
    } else {
        None
    };
    let load = if name == "load_tool" {
        Some(load_args(args)?)
    } else {
        None
    };
    let guard = host.gate().lock().await;
    let agent = host.session_agent_id(sid).await?;
    let run = run(host.store(), run_id).await?;
    if cancel.is_cancelled()
        || run["session_id"] != sid
        || run["agent_id"] != agent
        || run["status"] != "running"
        || run["pending"]["name"] != name
        || shutoff(host.store()).await?
    {
        bail!("Run unavailable");
    }
    let access = host.session_access(sid, &agent).await?;
    let (policy, _, visibility, state) = host.discovery_controls(sid, &agent).await?;
    let state = host.selected_state(&visibility, &state, &run["loaded_abilities"]);
    let read_only = host.session_mode(sid).await?["mode"] == "read_only";
    if host
        .live_tool_decision(
            &access,
            name,
            &policy,
            &visibility,
            &state,
            &run["loaded_abilities"],
            read_only,
        )
        .permission
        == Permission::Deny
    {
        bail!("Catalog operation denied");
    }
    let tools = eligible(host, sid, &agent, &run["loaded_abilities"]).await?;
    if let Some(args) = list {
        let query = args.query.as_deref().unwrap_or_default().trim().to_owned();
        let tools = tools
            .into_iter()
            .filter(|tool| {
                args.group
                    .as_ref()
                    .is_none_or(|group| tool["ability_id"] == *group)
            })
            .collect::<Vec<_>>();
        if query.is_empty() {
            let tools = tools
                .into_iter()
                .filter(|tool| args.include_available || tool["requires_activation"] == true)
                .collect::<Vec<_>>();
            return Ok(
                json!({"status":"success","output":{"tools":tools,"include_available":args.include_available,"scope":"conversation","permissions_changed":false}}),
            );
        }
        // Ranking reads no authority; release the gate while the matcher answers.
        drop(guard);
        let (tools, ranked_by) = ranked(host, &query, tools).await;
        return Ok(
            json!({"status":"success","output":{"tools":tools,"query":query,"ranked_by":ranked_by,"scope":"conversation","permissions_changed":false}}),
        );
    }
    let args = load.context("Missing load arguments")?;
    let tool = tools
        .into_iter()
        .find(|tool| tool["name"] == args.tool_name)
        .context("Tool unavailable")?;
    if tool["requires_activation"] != true {
        return Ok(
            json!({"status":"already_available","output":{"tool_name":args.tool_name,"ability_id":tool["ability_id"],"availability":tool["availability"],"activated":false,"selected_tools":[],"scope":"conversation","permissions_changed":false,"message":"This tool is already supplied to the current model context."}}),
        );
    }
    Ok(
        json!({"status":"success","output":{"tool_name":args.tool_name,"ability_id":tool["ability_id"],"availability":"activated","activated":true,"selected_tools":[args.tool_name],"scope":"conversation","permissions_changed":false,"message":"Tool selected for this conversation. Its current definition is supplied in each model context."}}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    /// Accumulate successful identities without removing previous selections.
    #[test]
    fn selection_adds_markers_and_preserves_previous_tools() {
        let mut run = json!({"pending":{"name":"load_tool"},"loaded_abilities":["base","tool:old"],"selected_tools":["old"]});
        let result =
            json!({"status":"success","output":{"activated":true,"selected_tools":["file_read"]}});
        apply_selection(&mut run, &result);
        assert_eq!(
            run["loaded_abilities"],
            json!(["base", "tool:old", "tool:file_read"])
        );
        assert_eq!(run["selected_tools"], json!(["old", "file_read"]));
        let saved = run.clone();
        apply_selection(&mut run, &result);
        assert_eq!(run, saved);
        apply_selection(&mut run, &json!({"status":"failed"}));
        assert_eq!(run, saved);
        apply_selection(
            &mut run,
            &json!({"status":"success","output":{"activated":false,"selected_tools":["file_edit"]}}),
        );
        assert_eq!(run, saved);
        apply_selection(
            &mut run,
            &json!({"status":"success","output":{"activated":true,"selected_tools":[]}}),
        );
        assert_eq!(run, saved);
    }
    /// Reject extra scope, invalid names and unbounded exact-catalog queries.
    #[test]
    fn exact_catalog_argument_validation() {
        assert!(list_args(&json!({})).is_ok());
        assert!(list_args(&json!({"include_available":true,"query":"file"})).is_ok());
        for value in [
            json!({"include_available":"true"}),
            json!({"activate":true}),
            json!({"query":"x".repeat(4097)}),
            json!({"query":"\u{0}"}),
        ] {
            assert!(list_args(&value).is_err());
        }
        assert!(load_args(&json!({"tool_name":"file_read"})).is_ok());
        for value in [
            json!({}),
            json!({"tool_name":""}),
            json!({"tool_name":"file read"}),
            json!({"tool_name":"file_read","activate":true}),
        ] {
            assert!(load_args(&value).is_err());
        }
    }
    /// Keep available acknowledgments inert and repeated selections idempotent.
    #[test]
    fn exact_load_selection_and_acknowledgment() {
        let mut run =
            json!({"pending":{"name":"load_tool"},"loaded_abilities":["base"],"selected_tools":[]});
        let saved = run.clone();
        apply_selection(
            &mut run,
            &json!({"status":"already_available","output":{"activated":false,"selected_tools":["file_read"]}}),
        );
        assert_eq!(run, saved);
        let success =
            json!({"status":"success","output":{"activated":true,"selected_tools":["file_read"]}});
        apply_selection(&mut run, &success);
        assert_eq!(run["loaded_abilities"], json!(["base", "tool:file_read"]));
        let saved = run.clone();
        apply_selection(&mut run, &success);
        assert_eq!(run, saved);
    }
}
