//! search_sessions: ranked message excerpts across reachable sessions.
use super::{reachable, settings, turns, Args};
use crate::{text_search, NativeHost};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};

/// Interactions one `search_sessions` call reads across all sessions; older sessions past
/// it are left out and reported.
const MAX_SEARCH_ITEMS: usize = 50_000;

/// Ranked excerpts of the user and agent messages in the sessions `sid` may reach, with
/// the turn numbers `read_session` uses.
pub(super) async fn run(host: &dyn NativeHost, sid: &str, q: &Args) -> Result<Value> {
    let query = q.query.as_deref().unwrap_or("");
    if query.trim().is_empty() || query.chars().count() > 500 || query.chars().any(char::is_control)
    {
        return Err(anyhow!("query must be 1-500 characters of text"));
    }
    let terms = text_search::query_terms(query);
    if terms.is_empty() {
        return Err(anyhow!(
            "query needs at least one word that is not a common stopword"
        ));
    }
    let (settings, _) = settings(host, sid).await?;
    let mut limits = text_search::Limits::from_settings(&settings);
    if let Some(limit) = q.limit {
        limits.total = limit.clamp(1, limits.total);
    }
    let (sessions, owned) = reachable(host, sid).await?;
    let mut texts: Vec<(usize, usize, &'static str, String)> = Vec::new();
    let mut scanned = 0usize;
    let mut searched = 0usize;
    for (index, session) in sessions.iter().enumerate() {
        if scanned >= MAX_SEARCH_ITEMS {
            break;
        }
        let target = session["session_id"].as_str().unwrap_or("");
        let items = host
            .store()
            .session_interactions(target, (MAX_SEARCH_ITEMS - scanned) as i64)
            .await?;
        scanned += items.len();
        searched += 1;
        for turn in turns(&items, usize::MAX) {
            let role = match turn["kind"].as_str() {
                Some("user") => "user",
                Some("agent") => "agent",
                _ => continue,
            };
            if let (Some(number), Some(text)) = (turn["turn"].as_u64(), turn["text"].as_str()) {
                texts.push((index, number as usize, role, text.to_owned()));
            }
        }
    }
    // Newest sessions first, and within a session newest turns first, so ties favour recent text.
    texts.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    let docs: Vec<text_search::Doc<'_>> = texts
        .iter()
        .map(|(index, turn, role, text)| {
            let session = &sessions[*index];
            text_search::Doc {
                group: session["session_id"].as_str().unwrap_or(""),
                meta: json!({"session_id":session["session_id"],"title":session["title"],
                    "agent_id":session["agent_id"],"last_active_at":session["last_active_at"],
                    "turn":turn,"role":role}),
                text,
            }
        })
        .collect();
    let results = text_search::search(&terms, &docs, &limits);
    Ok(json!({"results":results,"sessions_searched":searched,
        "sessions_skipped":sessions.len()-searched,
        "scope":if owned {"owned"} else {"started"},"limits":limits.describe(),
        "read_more":"read_session with session_id and a from/to range around turn",
        "authority":"untrusted_session_observation"}))
}
