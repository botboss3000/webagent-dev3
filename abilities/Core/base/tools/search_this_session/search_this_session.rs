//! search_this_session: ranked excerpts of this session's published chat.
use super::NativeHost;
use crate::text_search;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;

/// Chat rows `search_this_session` reads per page and in total.
const SEARCH_PAGE: i64 = 50;
const SEARCH_ROWS: usize = 5000;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SearchArgs {
    pub(super) query: String,
    #[serde(default)]
    pub(super) limit: Option<usize>,
}
pub(super) fn search_args(args: &SearchArgs) -> Result<BTreeSet<String>> {
    if args.query.trim().is_empty()
        || args.query.chars().count() > 500
        || args.query.chars().any(char::is_control)
        || args.limit.is_some_and(|limit| !(1..=20).contains(&limit))
    {
        bail!("Invalid session search arguments");
    }
    let terms = text_search::query_terms(&args.query);
    if terms.is_empty() {
        bail!("Session search needs at least one word that is not a common stopword");
    }
    Ok(terms)
}
/// Ranked excerpts of the published user and assistant rows in `rows`, oldest first.
pub(super) fn search_rows(
    rows: &[Value],
    terms: &BTreeSet<String>,
    mut limits: text_search::Limits,
) -> Vec<Value> {
    // One conversation: the per-session limit does not apply.
    limits.per_session = limits.total;
    let docs: Vec<text_search::Doc<'_>> = rows
        .iter()
        .rev()
        // Search only published user/assistant chat. Private tool evidence never
        // enters this helper, and unknown row roles cannot expand that contract.
        .filter(|row| matches!(row["role"].as_str(), Some("user" | "assistant")))
        .filter_map(|row| {
            Some(text_search::Doc {
                group: "",
                meta: json!({"seq":row["seq"],"id":row["id"],"run_id":row["run_id"],
                    "role":row["role"],"created_at":row["created_at"]}),
                text: row["content"].as_str()?,
            })
        })
        .collect();
    text_search::search(terms, &docs, &limits)
}
pub(super) async fn run(host: &dyn NativeHost, sid: &str, args: &Value) -> Result<Value> {
    let query: SearchArgs =
        serde_json::from_value(args.clone()).context("Invalid session search arguments")?;
    let terms = search_args(&query)?;
    // The settings only bound excerpt sizes and counts; without readable ability
    // controls (no account record) the defaults apply and the read stays authorized.
    let settings = host
        .ability_controls(sid)
        .await
        .map(|(_, visibility, _)| visibility["settings"]["agent_management"].clone())
        .unwrap_or_default();
    let mut limits = text_search::Limits::from_settings(&settings);
    if let Some(limit) = query.limit {
        limits.total = limit.min(limits.total);
    }
    let mut rows = Vec::new();
    let mut after = 0;
    let mut complete = false;
    while rows.len() < SEARCH_ROWS {
        let page = host
            .model_chat_page(host.store(), sid, after, SEARCH_PAGE)
            .await
            .context("Current-session chat unavailable")?;
        let messages = page["messages"]
            .as_array()
            .context("Chat page unavailable")?;
        for message in messages {
            let seq = message["seq"].as_i64().context("Chat cursor unavailable")?;
            if seq <= after {
                bail!("Chat cursor order conflict");
            }
            after = seq;
            rows.push(message.clone());
        }
        if messages.is_empty() || page["has_more"] != true {
            complete = true;
            break;
        }
    }
    Ok(
        json!({"results":search_rows(&rows,&terms,limits),"scanned":rows.len(),
        "scan_complete":complete,"limits":limits.describe(),"source_kind":"public_chat"}),
    )
}
