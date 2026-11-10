//! Administrator review log: run-health records kept in one shared database
//! (`diagnostics/agent-review.db`) so reviewing an issue never reads a user's
//! database. The record is the bounded, content-free form the run view carries:
//! counts, failure kinds, tool names and message templates only.
//!
//! The log is evidence, not authority. A failed write never fails the run, and no
//! run, session or account read ever depends on it.
use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

/// Longest kept review history, in seconds (one year).
const REVIEW_RETENTION_SECONDS: i64 = 365 * 24 * 60 * 60;

/// Create the review tables once per process start and drop rows past retention.
pub(crate) async fn open_review_pool(root: &Path) -> Result<SqlitePool> {
    let directory = root.join("diagnostics");
    std::fs::create_dir_all(&directory)?;
    let pool = open_pool(&directory.join("agent-review.db"), false, false).await?;
    sqlx::raw_sql(
        "CREATE TABLE IF NOT EXISTS run_health(
             run_id TEXT PRIMARY KEY,
             user_id TEXT NOT NULL,
             session_id TEXT NOT NULL,
             root_session_id TEXT NOT NULL,
             agent_id TEXT NOT NULL DEFAULT '',
             status TEXT NOT NULL DEFAULT '',
             updated_at INTEGER NOT NULL,
             record BLOB NOT NULL);
         CREATE INDEX IF NOT EXISTS run_health_updated ON run_health(updated_at);
         CREATE INDEX IF NOT EXISTS run_health_root ON run_health(root_session_id,updated_at);
         CREATE INDEX IF NOT EXISTS run_health_user ON run_health(user_id,updated_at);",
    )
    .execute(&pool)
    .await?;
    sqlx::query("DELETE FROM run_health WHERE updated_at<?")
        .bind(unix_now() - REVIEW_RETENTION_SECONDS)
        .execute(&pool)
        .await?;
    Ok(pool)
}

impl Store {
    /// Copy `run`'s health into the shared review log after a committed save.
    /// Best effort: a failure here is not a run failure and not run authority.
    pub(crate) async fn log_run_health(&self, run: &Value) {
        if !run["run_health"].is_object() {
            return;
        }
        let Some(run_id) = run["id"].as_str() else {
            return;
        };
        let session_id = run["session_id"].as_str().unwrap_or("");
        let root = self.root_session(session_id).await;
        let updated_at = run["updated_at"].as_i64().unwrap_or(0);
        let record = json!({
            "run_id": run_id,
            "session_id": session_id,
            "root_session_id": root,
            "user_id": self.current_user_id(),
            "agent_id": run["agent_id"],
            "status": run["status"],
            "updated_at": if updated_at > 0 { updated_at } else { unix_now() },
            "model": webagent_engine::output::public_run_model(run),
            "run_health": run["run_health"]});
        let Ok(sealed) = self.seal(&record) else {
            return;
        };
        let _ = sqlx::query(
            "INSERT INTO run_health(run_id,user_id,session_id,root_session_id,agent_id,status,updated_at,record)
             VALUES(?,?,?,?,?,?,?,?)
             ON CONFLICT(run_id) DO UPDATE SET
               user_id=excluded.user_id,session_id=excluded.session_id,
               root_session_id=excluded.root_session_id,agent_id=excluded.agent_id,
               status=excluded.status,updated_at=excluded.updated_at,record=excluded.record",
        )
        .bind(run_id)
        .bind(self.current_user_id())
        .bind(session_id)
        .bind(&root)
        .bind(run["agent_id"].as_str().unwrap_or(""))
        .bind(run["status"].as_str().unwrap_or(""))
        .bind(updated_at.max(1))
        .bind(&sealed)
        .execute(&self.review_pool)
        .await;
    }

    /// The session tree root recorded for `session_id`, itself when it has no spawn record.
    async fn root_session(&self, session_id: &str) -> String {
        self.get(&format!("core_spawn_origin:{session_id}"))
            .await
            .ok()
            .flatten()
            .and_then(|origin| origin["root_session_id"].as_str().map(str::to_owned))
            .unwrap_or_else(|| session_id.to_owned())
    }

    /// Decode stored review views for the administrator issues query: one session
    /// tree (`root_session_id` or the session itself), one account, or every account.
    pub(crate) async fn review_issue_views(
        &self,
        session: Option<&str>,
        user: Option<&str>,
        since: i64,
        limit: i64,
    ) -> Result<Vec<Value>> {
        let rows: Vec<Vec<u8>> = sqlx::query_scalar(
            "SELECT record FROM run_health
             WHERE updated_at>=?
               AND (? IS NULL OR root_session_id=? OR session_id=?)
               AND (? IS NULL OR user_id=?)
             ORDER BY updated_at DESC LIMIT ?",
        )
        .bind(since)
        .bind(session)
        .bind(session)
        .bind(session)
        .bind(user)
        .bind(user)
        .bind(limit)
        .fetch_all(&self.review_pool)
        .await?;
        Ok(rows
            .iter()
            .filter_map(|raw| self.unseal(raw).ok())
            .filter(|view| view["run_health"].is_object())
            .collect())
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fixture() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        for sid in ["root", "child"] {
            store
                .put(
                    &format!("session:{sid}"),
                    &json!({"id":sid,"agent_id":"shared_default"}),
                )
                .await
                .unwrap();
        }
        (root, store)
    }

    fn failed_run(id: &str, sid: &str) -> Value {
        json!({"id":id,"session_id":sid,"agent_id":"shared_default","status":"queued",
            "run_revision":0,"lease_epoch":0,"generation":"fixture","input":[],
            "updated_at":1000,
            "run_health":{"steps":2,"calls":3,"failed":1,"repeats":0,"groups_dropped":0,
                "failed_by_kind":{"error/timeout":1},
                "groups":[{"tool":"file_read","class":"error","kind":"timeout",
                    "template":"read timed out after <n>s","count":1,"retries":1,
                    "identical_retries":1,"recovered":0}],
                "rework":{}}})
    }

    /// A save copies the run's health into the shared log once and keeps it current.
    #[tokio::test]
    async fn a_save_reaches_the_shared_review_log_once() {
        let (_root, store) = fixture().await;
        let mut run = failed_run("r1", "root");
        store
            .save_run(&mut run, "run_status", &json!({}))
            .await
            .unwrap();
        store
            .save_run(&mut run, "run_status", &json!({}))
            .await
            .unwrap();
        let views = store.review_issue_views(None, None, 0, 50).await.unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0]["run_health"]["failed"], json!(1));
        assert_eq!(views[0]["user_id"], json!("admin"));
        assert_eq!(views[0]["sessions"].is_null(), true, "raw view only");
    }

    /// A sub-agent's run files under its tree root, so one session filter finds it.
    #[tokio::test]
    async fn a_child_session_files_under_its_root() {
        let (_root, store) = fixture().await;
        store
            .put(
                "core_spawn_origin:child",
                &json!({"parent_session_id":"root","root_session_id":"root","depth":1}),
            )
            .await
            .unwrap();
        let mut run = failed_run("r2", "child");
        store
            .save_run(&mut run, "run_status", &json!({}))
            .await
            .unwrap();
        let by_root = store
            .review_issue_views(Some("root"), None, 0, 50)
            .await
            .unwrap();
        assert_eq!(by_root.len(), 1);
        assert_eq!(by_root[0]["root_session_id"], json!("root"));
        let other = store
            .review_issue_views(Some("unrelated"), None, 0, 50)
            .await
            .unwrap();
        assert!(other.is_empty());
    }
}
