//! Cross-session Core reads use the authenticated user's encrypted store.
//! Session ownership and live agent admission are checked before returning text.
use crate::{Call, NativeHost};
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use tokio_util::sync::CancellationToken;
use webagent_access::Permission;
use webagent_host_core::{digest, key, StoreCore};

/// The cross-session search tool names.
pub const TOOLS: &[&str] = &["session_search", "similar_sessions"];
/// Whether `name` is a cross-session search tool.
#[must_use]
pub fn owns(name: &str) -> bool {
    matches!(name, "session_search" | "similar_sessions")
}
/// The cross-session search tool schemas.
#[must_use]
pub fn schemas() -> Vec<Value> {
    vec![
        json!({"type":"function","name":"session_search",
            "description":"Search caller-owned public chat and titles by query, or read one exact owned session by session_id. Set unused selectors to null. Private tool transcripts and checkpoints are excluded.",
            "parameters":{"type":"object","properties":{
                "query":{"type":["string","null"],"maxLength":256},
                "session_id":{"type":["string","null"],"maxLength":128},
                "after_seq":{"type":["integer","null"],"minimum":0},
                "limit":{"type":["integer","null"],"minimum":1,"maximum":20},
                "continuation":{"type":["string","null"],"maxLength":4096}},
                "required":[],"additionalProperties":false}}),
        json!({"type":"function","name":"similar_sessions",
            "description":"Find other accessible sessions by literal topic text. V3 has keyword matching, not semantic similarity.",
            "parameters":{"type":"object","properties":{
                "query":{"type":"string","minLength":1,"maxLength":256},
                "limit":{"type":["integer","null"],"minimum":1,"maximum":20},
                "continuation":{"type":["string","null"],"maxLength":4096}},
                "required":["query"],"additionalProperties":false}}),
    ]
}
fn search_limit() -> usize {
    10
}
fn similar_limit() -> usize {
    8
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    after_seq: Option<i64>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    continuation: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SimilarArgs {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    continuation: Option<String>,
}
fn valid_query(q: &str) -> bool {
    !q.trim().is_empty()
        && q.len() <= 256
        && q.chars().count() <= 128
        && !q.chars().any(char::is_control)
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
fn preview(text: &str) -> String {
    text.chars()
        .take(400)
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}
fn public_page(
    page: Value,
    sid: &str,
    after_seq: i64,
    limit: usize,
    query: Option<&str>,
) -> Result<Value> {
    let rows = page["messages"]
        .as_array()
        .context("Chat page unavailable")?;
    if rows.len() > 50 {
        bail!("Chat page exceeds bound");
    }
    let mut results = Vec::new();
    let mut next = after_seq;
    let mut scanned = 0;
    for row in rows {
        next = row["seq"].as_i64().context("Chat cursor unavailable")?;
        scanned += 1;
        if !matches!(row["role"].as_str(), Some("user" | "assistant")) {
            continue;
        }
        let Some(text) = row["content"].as_str() else {
            continue;
        };
        if query.is_some_and(|q| !text.to_lowercase().contains(&q.to_lowercase())) {
            continue;
        }
        results.push(json!({"session_id":sid,"seq":next,"role":row["role"],
            "content_preview":preview(text),"preview_truncated":text.chars().count()>400,
            "source_kind":"public_chat"}));
        if results.len() == limit {
            break;
        }
    }
    Ok(json!({"results":results,"next_seq":next,
        "has_more":scanned<rows.len() || page["has_more"]==true,
        "scanned":scanned,"scan_limit":50,"source_kind":"public_chat",
        "private_tool_transcripts_included":false}))
}
/// Run `id` from `store`.
async fn run(store: &StoreCore, id: &str) -> Result<Value> {
    store
        .get(&key("run", id))
        .await?
        .ok_or_else(|| anyhow!("Run not found"))
}
struct View {
    store: StoreCore,
    fingerprint: String,
}
async fn view(host: &dyn NativeHost, sid: &str, run_id: &str, name: &str) -> Result<View> {
    if host.actor() != host.store().current_user_id() {
        bail!("Shared chat membership does not authorize cross-session reads");
    }
    // The current session is resolved in the authenticated user's data file.
    let agent = host.session_agent_id(sid).await?;
    let scoped = host.agent_store(&agent).await?;
    let run = run(&scoped, run_id).await?;
    if run["session_id"] != sid || run["agent_id"].as_str().unwrap_or("shared_default") != agent {
        bail!("Run does not belong to current session and agent");
    }
    let (policy, revision, visibility, state) = host.scoped_ability_controls(&scoped, sid).await?;
    let mode = host.scoped_session_mode(&scoped, sid).await?;
    let access = host.live_access(scoped.current_user_id(), &agent).await?;
    if !access.admitted()
        || host
            .live_tool_decision(
                &access,
                name,
                &policy,
                &visibility,
                &state,
                &run["loaded_abilities"],
                mode["mode"] == "read_only",
            )
            .permission
            == Permission::Deny
    {
        bail!("Cross-session read unavailable under current access");
    }
    let fingerprint = digest(&json!([
        agent,
        policy,
        revision,
        visibility,
        state,
        mode,
        run["loaded_abilities"],
        access.context_fingerprint(),
        host.abilities_version()
    ]));
    Ok(View {
        store: scoped,
        fingerprint,
    })
}
async fn admitted(host: &dyn NativeHost, view: &View, agent: &str) -> bool {
    host.live_access(view.store.current_user_id(), agent)
        .await
        .map(|access| access.admitted())
        .unwrap_or(false)
}
async fn work(
    host: &dyn NativeHost,
    sid: &str,
    run_id: &str,
    name: &str,
    args: &Value,
) -> Result<Value> {
    let current = view(host, sid, run_id, name).await?;
    let output = match name {
        "session_search" => {
            let q: SearchArgs =
                serde_json::from_value(args.clone()).context("Invalid session search arguments")?;
            let query = q.query.as_deref().filter(|s| !s.is_empty());
            let source = q.session_id.as_deref().filter(|s| !s.is_empty());
            let continuation = q.continuation.as_deref().filter(|s| !s.is_empty());
            let after_seq = q.after_seq.unwrap_or(0);
            let limit = q.limit.unwrap_or_else(search_limit);
            if !(1..=20).contains(&limit)
                || after_seq < 0
                || query.is_some_and(|s| !valid_query(s))
                || source.is_some_and(|s| !valid_id(s))
                || continuation.is_some_and(|s| s.len() > 4096)
                || (source.is_none() && query.is_none())
                || (source.is_some() && continuation.is_some())
                || (source.is_none() && after_seq != 0)
            {
                bail!("Invalid session search selector");
            }
            if let Some(source) = source {
                let doc = current
                    .store
                    .get(&format!("session:{source}"))
                    .await?
                    .context("Source session unavailable")?;
                let agent = doc["agent_id"].as_str().unwrap_or("shared_default");
                if !admitted(host, &current, agent).await {
                    bail!("Source session unavailable");
                }
                let page = host
                    .model_chat_page(&current.store, source, after_seq, 50)
                    .await?;
                let mut result = public_page(page, source, after_seq, limit, query)?;
                result["mode"] = json!("exact_owned_session");
                result
            } else {
                let page = host
                    .search_private_sessions(
                        &current.store,
                        query.unwrap(),
                        "active",
                        limit,
                        continuation,
                    )
                    .await?;
                let mut hits = Vec::new();
                for hit in page["results"]
                    .as_array()
                    .context("Search page unavailable")?
                {
                    if !host.model_search_hit_allowed(&current.store, hit).await? {
                        continue;
                    }
                    let agent = hit["agent_id"].as_str().unwrap_or("shared_default");
                    if admitted(host, &current, agent).await {
                        hits.push(hit.clone());
                    }
                }
                json!({"results":hits,"continuation":page["continuation"],
                    "scan_complete":page["scan_complete"],"mode":"literal_keyword",
                    "source_kind":"public_chat_and_titles","private_tool_transcripts_included":false})
            }
        }
        "similar_sessions" => {
            let q: SimilarArgs = serde_json::from_value(args.clone())
                .context("Invalid similar-session arguments")?;
            let limit = q.limit.unwrap_or_else(similar_limit);
            let continuation = q.continuation.as_deref().filter(|s| !s.is_empty());
            if !valid_query(&q.query)
                || !(1..=20).contains(&limit)
                || continuation.is_some_and(|s| s.len() > 4096)
            {
                bail!("Invalid similar-session selector");
            }
            // The bounded encrypted search is literal. Deduplicate title/message
            // hits and do not invent a semantic score without a V3 embedder.
            let page = host
                .search_private_sessions(&current.store, &q.query, "active", limit, continuation)
                .await?;
            let mut seen = HashSet::new();
            let mut results = Vec::new();
            for hit in page["results"]
                .as_array()
                .context("Search page unavailable")?
            {
                if !host.model_search_hit_allowed(&current.store, hit).await? {
                    continue;
                }
                let Some(source) = hit["session_id"].as_str() else {
                    continue;
                };
                if source == sid || !seen.insert(source.to_owned()) {
                    continue;
                }
                let agent = hit["agent_id"].as_str().unwrap_or("shared_default");
                if !admitted(host, &current, agent).await {
                    continue;
                }
                results.push(
                    json!({"session_id":source,"agent_id":agent,"title":hit["title"],
                    "matched_in":hit["kind"],"snippet":hit["snippet"]}),
                );
                if results.len() == limit {
                    break;
                }
            }
            json!({"count":results.len(),"results":results,
                "continuation":page["continuation"],"scan_complete":page["scan_complete"],
                "mode":"literal_keyword","semantic_similarity":false,
                "source_kind":"public_chat_and_titles","excluded_session_id":sid,
                "private_tool_transcripts_included":false})
        }
        _ => bail!("Unknown cross-session Core tool"),
    };
    if view(host, sid, run_id, name).await?.fingerprint != current.fingerprint {
        bail!("Cross-session read access changed during read");
    }
    // Membership on a source agent can change independently of this run.
    if let Some(source) = args["session_id"].as_str() {
        let doc = current
            .store
            .get(&format!("session:{source}"))
            .await?
            .context("Source session unavailable")?;
        if !admitted(
            host,
            &current,
            doc["agent_id"].as_str().unwrap_or("shared_default"),
        )
        .await
        {
            bail!("Source session access changed during read");
        }
    }
    for hit in output["results"].as_array().into_iter().flatten() {
        if let Some(agent) = hit["agent_id"].as_str() {
            if !admitted(host, &current, agent).await {
                bail!("Source agent access changed during read");
            }
        }
    }
    Ok(output)
}
/// Run cross-session search tool `name` at `call`'s position; rejections are tool results.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    if cancel.is_cancelled() {
        bail!("tool execution cancelled");
    }
    let result = tokio::select! {
        _ = cancel.cancelled() => bail!("tool execution cancelled"),
        result = work(host, call.sid, call.run_id, name, args) => result,
    };
    if cancel.is_cancelled() {
        bail!("tool execution cancelled");
    }
    Ok(match result {
        Ok(output) => json!({"status":"success","output":output}),
        Err(error) => json!({"status":"error","error_kind":"session_search_rejected",
            "output":error.to_string()}),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_page_excludes_private_roles_and_bounds_preview() {
        let page = json!({"messages":[
            {"seq":1,"role":"tool","content":"secret"},
            {"seq":2,"role":"assistant","content":"needle"},
            {"seq":3,"role":"user","content":"x".repeat(500)}],"has_more":false});
        let output = public_page(page, "owned", 0, 2, None).unwrap();
        assert_eq!(output["results"].as_array().unwrap().len(), 2);
        assert_eq!(output["results"][0]["seq"], 2);
        assert_eq!(output["results"][1]["preview_truncated"], true);
        assert!(!output.to_string().contains("secret"));
        assert_eq!(output["private_tool_transcripts_included"], false);
    }
    #[test]
    fn selectors_reject_cross_authority_fields() {
        assert!(!valid_query("\n"));
        assert!(!valid_id("../other"));
        assert!(
            serde_json::from_value::<SearchArgs>(json!({"query":"x","user_id":"other"})).is_err()
        );
        assert!(
            serde_json::from_value::<SimilarArgs>(json!({"query":"x","agent_id":"other"})).is_err()
        );
    }
    #[test]
    fn provider_nullable_selectors_deserialize() {
        let args: SearchArgs = serde_json::from_value(json!({
            "session_id":"owned","query":null,"after_seq":null,"limit":null,"continuation":null
        }))
        .unwrap();
        assert_eq!(args.session_id.as_deref(), Some("owned"));
        assert_eq!(args.after_seq.unwrap_or(0), 0);
        assert_eq!(args.limit.unwrap_or_else(search_limit), 10);
    }
}
