//! `file_search` `find` log for tuning ranking: each lookup's query, settings and shown
//! blocks, with its outcome. A later live `file_read` in the session that lands in a shown
//! block marks the lookup a hit at that rank; another `find` before any such read marks it a
//! miss. Rows sit in the shared administrator review database (`diagnostics/agent-review.db`)
//! for a future tuning page. Evidence only: a failed write never fails the tool call.
use crate::AdministrationHost;
use serde_json::{json, Value};
use webagent_host_core::now;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS find_log(
    id TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL,
    user_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    query TEXT NOT NULL,
    path TEXT NOT NULL,
    ranked_by TEXT NOT NULL,
    settings TEXT NOT NULL,
    blocks INTEGER NOT NULL,
    candidates INTEGER NOT NULL,
    limited INTEGER NOT NULL,
    elapsed_ms INTEGER NOT NULL,
    shown TEXT NOT NULL,
    outcome TEXT NOT NULL DEFAULT 'pending',
    hit_rank INTEGER,
    resolved_at INTEGER);
  CREATE INDEX IF NOT EXISTS find_log_session ON find_log(session_id,created_at);
  CREATE INDEX IF NOT EXISTS find_log_time ON find_log(created_at);";

/// One lookup as shown to the agent.
pub(crate) struct Lookup<'a> {
    pub query: &'a str,
    pub path: &'a str,
    pub ranked_by: &'a str,
    pub settings: Value,
    pub blocks: u64,
    pub candidates: usize,
    pub limited: bool,
    pub elapsed_ms: u64,
    /// `{path, start, end, name}` of each shown block, best first.
    pub shown: Vec<Value>,
}

/// Record a lookup, close the session's open lookup as a miss and drop rows past `keep_days`.
pub(crate) async fn record(
    host: &dyn AdministrationHost,
    sid: &str,
    lookup: Lookup<'_>,
    keep_days: u64,
) {
    let pool = &host.store().review_pool;
    let at = i64::try_from(now()).unwrap_or(i64::MAX);
    let cutoff = at - i64::try_from(keep_days * 86_400).unwrap_or(i64::MAX);
    let result = async {
        sqlx::raw_sql(SCHEMA).execute(pool).await?;
        sqlx::query("DELETE FROM find_log WHERE created_at<?").bind(cutoff).execute(pool).await?;
        sqlx::query("UPDATE find_log SET outcome='miss',resolved_at=? WHERE session_id=? AND outcome='pending'")
            .bind(at)
            .bind(sid)
            .execute(pool)
            .await?;
        sqlx::query("INSERT INTO find_log(id,created_at,user_id,session_id,query,path,ranked_by,settings,blocks,candidates,limited,elapsed_ms,shown) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(at)
            .bind(&host.store().user_id)
            .bind(sid)
            .bind(lookup.query)
            .bind(lookup.path)
            .bind(lookup.ranked_by)
            .bind(lookup.settings.to_string())
            .bind(i64::try_from(lookup.blocks).unwrap_or(i64::MAX))
            .bind(i64::try_from(lookup.candidates).unwrap_or(i64::MAX))
            .bind(lookup.limited)
            .bind(i64::try_from(lookup.elapsed_ms).unwrap_or(i64::MAX))
            .bind(Value::from(lookup.shown).to_string())
            .execute(pool)
            .await?;
        Ok::<_, sqlx::Error>(())
    }
    .await;
    if let Err(error) = result {
        report(&error);
    }
}

/// Mark the session's open lookup a hit when this live `file_read` lands in a shown block.
pub(crate) async fn read(host: &dyn AdministrationHost, sid: &str, args: &Value) {
    let pool = &host.store().review_pool;
    let Some(path) = args["path"].as_str() else {
        return;
    };
    let path = path.trim_start_matches("/code/").trim_start_matches("./");
    let first = args["offset"].as_u64().unwrap_or(1).max(1);
    let last = args["limit"]
        .as_u64()
        .map_or(u64::MAX, |limit| first.saturating_add(limit));
    let result = async {
        sqlx::raw_sql(SCHEMA).execute(pool).await?;
        let open: Option<(String, String)> = sqlx::query_as(
            "SELECT id,shown FROM find_log WHERE session_id=? AND outcome='pending' ORDER BY created_at DESC LIMIT 1",
        )
        .bind(sid)
        .fetch_optional(pool)
        .await?;
        let Some((id, shown)) = open else {
            return Ok(());
        };
        let shown: Vec<Value> = serde_json::from_str(&shown).unwrap_or_default();
        let rank = shown.iter().position(|block| {
            block["path"] == path
                && block["start"].as_u64().is_some_and(|start| start <= last)
                && block["end"].as_u64().is_some_and(|end| end >= first)
        });
        if let Some(rank) = rank {
            sqlx::query("UPDATE find_log SET outcome='hit',hit_rank=?,resolved_at=? WHERE id=?")
                .bind(i64::try_from(rank + 1).unwrap_or(i64::MAX))
                .bind(i64::try_from(now()).unwrap_or(i64::MAX))
                .bind(id)
                .execute(pool)
                .await?;
        }
        Ok::<_, sqlx::Error>(())
    }
    .await;
    if let Err(error) = result {
        report(&error);
    }
}

/// Log a failed write; the tool call goes on.
fn report(error: &sqlx::Error) {
    eprintln!("find log write failed: {error}");
}

/// The shown blocks as logged.
pub(crate) fn shown(candidates: &[Value], order: &[usize]) -> Vec<Value> {
    order
        .iter()
        .map(|&at| {
            let c = &candidates[at];
            json!({"path": c["path"], "start": c["start"], "end": c["end"], "name": c["name"]})
        })
        .collect()
}
