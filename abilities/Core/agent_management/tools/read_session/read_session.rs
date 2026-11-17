//! read_session: a bounded range of a reachable session's turns.
use super::{in_scope, relation, run_status, turns, Args};
use crate::NativeHost;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};

/// Turns `read_session` returns when no range is given.
const DEFAULT_TURNS: usize = 20;
/// Per-item character limits for the brief and full detail levels.
pub(super) const BRIEF_CHARS: usize = 200;
const FULL_CHARS: usize = 4000;
/// Serialized turn bytes one `read_session` response may carry.
const MAX_READ_BYTES: usize = 24 * 1024;
/// Interactions `read_session` reads; older ones are left out and reported.
const MAX_ITEMS: i64 = 5000;

/// A range of session `target`'s turns, bounded in size.
pub(super) async fn run(host: &dyn NativeHost, sid: &str, target: &str, q: &Args) -> Result<Value> {
    let session = in_scope(host, sid, target).await?;
    let limit = match q.detail.as_deref() {
        None | Some("brief") => BRIEF_CHARS,
        Some("full") => FULL_CHARS,
        Some(_) => return Err(anyhow!("detail must be brief or full")),
    };
    let items = host.store().session_interactions(target, MAX_ITEMS).await?;
    let all = turns(&items, limit);
    let total = all.len();
    let (from, to) = if q.all == Some(true) {
        (1, total)
    } else {
        let to = q.to.unwrap_or(total).min(total);
        let from = q
            .from
            .unwrap_or_else(|| to.saturating_sub(DEFAULT_TURNS - 1))
            .max(1);
        (from, to)
    };
    if from > to && total > 0 {
        return Err(anyhow!("Empty turn range; this session has {total} turns"));
    }
    let mut selected: Vec<Value> = all
        .iter()
        .skip(from.saturating_sub(1))
        .take((to + 1).saturating_sub(from))
        .cloned()
        .collect();
    let mut first = from;
    while selected.len() > 1 && serde_json::to_vec(&selected)?.len() > MAX_READ_BYTES {
        selected.remove(0);
        first += 1;
    }
    let active = host.active_runs().await?;
    Ok(json!({
        "session":{"id":target,"title":session["title"],"agent_id":session["agent_id"],"status":session["status"],
            "relation":relation(sid,&session).unwrap_or("owned")},
        "run_status":run_status(host,&active,target),
        "current_turn":total,
        "range":[first,to],
        "turns":selected,
        "earlier":(first > 1).then(|| json!({"from":first.saturating_sub(DEFAULT_TURNS).max(1),"to":first-1})),
        "later":(to < total).then(|| json!({"from":to+1,"to":total})),
        "window_truncated":items.len() as i64 >= MAX_ITEMS,
        "authority":"untrusted_session_observation"}))
}
