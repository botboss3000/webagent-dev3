// The workspace has no clippy.toml with `allow-unwrap-in-tests` yet.
#![allow(clippy::unwrap_used)]

use super::*;
use webagent_host_core::test_support::store_core;

/// No sessions or runs; the dashboard still samples, probes and records history.
struct Quiet(StoreCore);

impl DashboardHost for Quiet {
    fn store(&self) -> &StoreCore {
        &self.0
    }
    fn session_page(&self, _offset: usize) -> Pending<'_, Result<(Vec<Value>, Option<usize>)>> {
        Box::pin(async { Ok((Vec::new(), None)) })
    }
    fn admitted<'a>(&'a self, _agent: &'a str) -> Pending<'a, bool> {
        Box::pin(async { true })
    }
    fn active_runs(&self) -> Pending<'_, Result<Vec<Value>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn git_status(&self) -> Pending<'_, Value> {
        Box::pin(async { json!({"available":false}) })
    }
    fn runtime_config(&self) -> Result<Value> {
        Ok(json!({"scheduler":{"max_active_runs":2}}))
    }
}

fn at(seconds_ago: u64) -> i64 {
    i64::try_from(now() - seconds_ago).unwrap()
}

#[tokio::test]
async fn dashboard_sections_validate_and_history_is_scoped_and_sealed() {
    let root = tempfile::tempdir().unwrap();
    let host = Quiet(store_core(root.path(), "admin").await);
    let store = host.store();
    let _ = collect(&host, 3600, "1h").await.unwrap();
    let foreign = store.seal(&json!({"at":now()-60,"foreign":true})).unwrap();
    sqlx::query("INSERT INTO system_dashboard_history(agent,bucket,payload) VALUES(?,?,?)")
        .bind("another-agent")
        .bind(at(60))
        .bind(foreign)
        .execute(&store.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM system_dashboard_history WHERE agent=?")
        .bind(store.current_agent_id())
        .execute(&store.pool)
        .await
        .unwrap();
    let old = store.seal(&json!({"at":now()-120,"usage_scope":"different-scope","output_tokens":10,"reported_calls":1})).unwrap();
    sqlx::query("INSERT INTO system_dashboard_history(agent,bucket,payload) VALUES(?,?,?)")
        .bind(store.current_agent_id())
        .bind(at(120))
        .bind(old)
        .execute(&store.pool)
        .await
        .unwrap();
    let value = collect(&host, 3600, "1h").await.unwrap();
    assert_eq!(value["throughput"]["interval_seconds"], 0.0);
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["history"]["sealed"], true);
    assert_eq!(value["history"]["points"].as_array().unwrap().len(), 2);
    assert_eq!(value["runtime"]["concurrency"], 2);
    assert!(value["database"]["probes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v["available"] == true));
    assert!(read(&host, Some("invalid")).await.is_err());
    assert!(section(&host, "secrets", None).await.is_err());
    let usage = section(&host, "usage", None).await.unwrap();
    assert_eq!(usage["data"]["session_count"], 0);
    let raw: Vec<u8> = sqlx::query_scalar("SELECT payload FROM system_dashboard_history LIMIT 1")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert!(serde_json::from_slice::<Value>(&raw).is_err());
    assert!(store.unseal(&raw).is_ok());
}
