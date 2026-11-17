//! list_sessions: one page of the sessions a session may reach.
use super::{reachable, run_status, Args};
use crate::NativeHost;
use anyhow::Result;
use serde_json::{json, Value};

/// One page of the sessions `sid` may reach, newest first.
pub(super) async fn run(host: &dyn NativeHost, sid: &str, q: &Args) -> Result<Value> {
    let (mut rows, owned) = reachable(host, sid).await?;
    let active = host.active_runs().await?;
    for row in &mut rows {
        row["run_status"] = json!(run_status(
            host,
            &active,
            row["session_id"].as_str().unwrap_or("")
        ));
    }
    let start = q.cursor.unwrap_or(0);
    let limit = q.limit.unwrap_or(20).clamp(1, 50);
    let page: Vec<Value> = rows.iter().skip(start).take(limit).cloned().collect();
    let next = (start + page.len() < rows.len()).then_some(start + page.len());
    Ok(
        json!({"sessions":page,"total":rows.len(),"next_cursor":next,
        "scope":if owned {"owned"} else {"started"}}),
    )
}
