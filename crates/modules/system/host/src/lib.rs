//! Scoped System dashboard telemetry over the shared storage handle. SQL probes are
//! read-only; history contains sealed numeric samples. Lookups owned by other modules
//! (sessions, live access, runs, build status, loop settings) come through
//! `DashboardHost`, which the root implements; a trait object keeps this code compiled
//! here rather than instantiated in the root crate.
use anyhow::Result;
use serde_json::{json, Value};
use sqlx::Row;
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use webagent_host_core::{bad, digest, now, ApiError, StoreCore};
use webagent_system::dashboard::{quantile, rate, scoped_runs, window_seconds as seconds};

/// A boxed `Send` future borrowed from the host.
pub type Pending<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What the dashboard needs from the rest of the application for one caller.
pub trait DashboardHost: Sync {
    /// The caller's storage handle; administrator middleware selects it.
    fn store(&self) -> &StoreCore;
    /// One page (at most 100) of the caller's active session statistics from `offset`,
    /// with the next offset when more remain.
    fn session_page(&self, offset: usize) -> Pending<'_, Result<(Vec<Value>, Option<usize>)>>;
    /// Whether the caller's live membership still admits them to `agent`.
    fn admitted<'a>(&'a self, agent: &'a str) -> Pending<'a, bool>;
    /// The current agent's unresolved runs.
    fn active_runs(&self) -> Pending<'_, Result<Vec<Value>>>;
    /// The broker's source Git summary, or an unavailable marker.
    fn git_status(&self) -> Pending<'_, Value>;
    /// The loop runtime settings as JSON.
    fn runtime_config(&self) -> Result<Value>;
}

/// Dashboard sections a caller may request on their own.
pub const SECTIONS: [&str; 8] = [
    "history",
    "database",
    "devices",
    "git",
    "loop",
    "throughput",
    "runtime",
    "usage",
];

type Cached = HashMap<String, (Instant, Value)>;
fn cache() -> &'static Mutex<Cached> {
    static CACHE: OnceLock<Mutex<Cached>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The whole dashboard for `window` (default `1h`), reused for 5 seconds per caller.
pub async fn read(host: &dyn DashboardHost, window: Option<&str>) -> Result<Value, ApiError> {
    let window = window.unwrap_or("1h");
    let span = seconds(window).ok_or_else(|| bad("Invalid dashboard time window"))?;
    let store = host.store();
    let key = format!(
        "{}:{}:{}:{window}",
        store.root.display(),
        store.current_user_id(),
        store.current_agent_id()
    );
    if let Some(value) = cache().lock().ok().and_then(|c| {
        c.get(&key)
            .filter(|(at, _)| at.elapsed() < Duration::from_secs(5))
            .map(|(_, v)| v.clone())
    }) {
        return Ok(value);
    }
    let value = collect(host, span, window).await?;
    if let Ok(mut cache) = cache().lock() {
        if cache.len() >= 32 {
            cache.clear();
        }
        cache.insert(key, (Instant::now(), value.clone()));
    }
    Ok(value)
}

/// One fixed section of the same snapshot and window contract.
pub async fn section(
    host: &dyn DashboardHost,
    name: &str,
    window: Option<&str>,
) -> Result<Value, ApiError> {
    if !SECTIONS.contains(&name) {
        return Err(bad("Unknown dashboard section"));
    }
    let value = read(host, window).await?;
    Ok(
        json!({"schema_version":1,"scope":value["scope"],"window":value["window"],"sampled_at_unix_ms":value["sampled_at_unix_ms"],"data":value[name]}),
    )
}

/// Probe the existing pools without writes, checkpoints or database inspection exports.
async fn probe(pool: &sqlx::SqlitePool, name: &str) -> Value {
    let start = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        sqlx::query_scalar::<_, i64>("SELECT 1")
            .fetch_one(pool)
            .await?;
        sqlx::query_scalar::<_, String>("PRAGMA journal_mode")
            .fetch_one(pool)
            .await
    })
    .await;
    match result {
        Ok(Ok(mode)) => {
            json!({"name":name,"available":true,"journal_mode":mode,"latency_ms":start.elapsed().as_secs_f64()*1000.0})
        }
        _ => json!({"name":name,"available":false,"reason":"database_probe_failed"}),
    }
}

fn devices() -> Value {
    let mut devices = Vec::new();
    let mut readable = false;
    for (folder, kind) in [("/sys/class/net", "network"), ("/sys/block", "block")] {
        let Ok(entries) = std::fs::read_dir(folder) else {
            continue;
        };
        readable = true;
        let mut paths = entries.flatten().map(|e| e.path()).collect::<Vec<_>>();
        paths.sort();
        for path in paths.into_iter().take(128) {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with("loop") || name.starts_with("ram") {
                continue;
            }
            let state = if kind == "network" {
                std::fs::read_to_string(path.join("operstate"))
                    .ok()
                    .map(|s| s.trim().to_owned())
            } else {
                None
            };
            devices.push(json!({"name":name,"kind":kind,"state":state}));
        }
    }
    json!({"available":readable,"scope":"server process sysfs namespace","devices":devices,"limit_per_kind":128})
}

async fn collect(
    host: &dyn DashboardHost,
    span: i64,
    window: &str,
) -> Result<Value, ApiError> {
    let store = host.store();
    let sampled = tokio::task::spawn_blocking(webagent_system::snapshot)
        .await
        .map_err(anyhow::Error::from)?;
    let at = i64::try_from(now()).map_err(anyhow::Error::from)?;
    let bucket = at - at % 30;
    let actor = store.current_agent_id();
    sqlx::query("CREATE TABLE IF NOT EXISTS system_dashboard_history(agent TEXT NOT NULL,bucket INTEGER NOT NULL,payload BLOB NOT NULL,PRIMARY KEY(agent,bucket))").execute(&store.pool).await.map_err(anyhow::Error::from)?;
    let database = vec![
        probe(&store.app_pool, "application").await,
        probe(&store.pool, "current user").await,
        probe(&store.agent_pool, "current agent").await,
    ];
    let db_values = database
        .iter()
        .filter_map(|v| v["latency_ms"].as_f64())
        .collect::<Vec<_>>();
    let db_mean =
        (!db_values.is_empty()).then(|| db_values.iter().sum::<f64>() / db_values.len() as f64);
    let mut sessions = Vec::new();
    let mut offset = 0;
    let mut truncated = false;
    for _ in 0..20 {
        let (mut page, next) = host.session_page(offset).await?;
        // Keep the existing live membership boundary when aggregating cross-agent caller-owned sessions.
        for item in page.drain(..) {
            if let Some(agent) = item["agent_id"].as_str() {
                if host.admitted(agent).await {
                    sessions.push(item);
                }
            }
        }
        match next {
            Some(next) => {
                offset = next;
                truncated = true;
            }
            None => {
                truncated = false;
                break;
            }
        }
    }
    let mut input = 0u64;
    let mut output = 0u64;
    let mut calls = 0u64;
    let mut unreported = 0u64;
    let mut unknown = 0u64;
    let mut cost = 0.0;
    let mut missing = 0usize;
    for row in &sessions {
        let usage = &row["usage"];
        if !usage.is_object() {
            missing += 1;
            continue;
        }
        input = input.saturating_add(usage["input_tokens"].as_u64().unwrap_or(0));
        output = output.saturating_add(usage["output_tokens"].as_u64().unwrap_or(0));
        calls = calls.saturating_add(usage["reported_calls"].as_u64().unwrap_or(0));
        unreported = unreported.saturating_add(usage["unreported_calls"].as_u64().unwrap_or(0));
        unknown = unknown.saturating_add(usage["unknown_cost_calls"].as_u64().unwrap_or(0));
        cost += usage["known_cost_usd"].as_f64().unwrap_or(0.0);
    }
    let mut agents = std::collections::BTreeMap::<String, u64>::new();
    let admitted = sessions
        .iter()
        .filter_map(|s| s["id"].as_str())
        .collect::<std::collections::HashSet<_>>();
    for session in &sessions {
        if let Some(agent) = session["agent_id"].as_str() {
            let tokens = session["usage"]["input_tokens"]
                .as_u64()
                .unwrap_or(0)
                .saturating_add(session["usage"]["output_tokens"].as_u64().unwrap_or(0));
            let count = agents.entry(agent.to_owned()).or_default();
            *count = count.saturating_add(tokens);
        }
    }
    let rows=sqlx::query("SELECT session_id,item FROM interactions WHERE step LIKE 'model:%' AND length(item)<=65536 ORDER BY seq DESC LIMIT 500")
        .fetch_all(&store.pool).await.map_err(anyhow::Error::from)?;
    let mut tool_counts = std::collections::BTreeMap::<String, u64>::new();
    for row in rows {
        let sid: String = row.get("session_id");
        if !admitted.contains(sid.as_str()) {
            continue;
        }
        let item = store.unseal(&row.get::<Vec<u8>, _>("item"))?;
        if item["type"] == "function_call" {
            if let Some(name) = item["name"].as_str().filter(|n| {
                n.len() <= 128
                    && n.bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_-. ".contains(&c))
            }) {
                *tool_counts.entry(name.to_owned()).or_default() += 1;
            }
        }
    }
    let tool_counts = tool_counts
        .into_iter()
        .map(|(name, calls)| json!({"name":name,"calls":calls}))
        .collect::<Vec<_>>();
    let agents = agents
        .into_iter()
        .map(|(agent, tokens)| json!({"agent":agent,"tokens":tokens}))
        .collect::<Vec<_>>();
    let runs = scoped_runs(host.active_runs().await?, &sessions, actor);
    let running = runs
        .iter()
        .filter(|r| r["status"] == "running")
        .collect::<Vec<_>>();
    let model = running.iter().filter(|r| r["phase"] == "model").count();
    let tools = running.iter().filter(|r| r["phase"] == "tools").count();
    let mut usage_sessions = admitted.iter().copied().collect::<Vec<_>>();
    usage_sessions.sort_unstable();
    let usage_scope = digest(&json!(usage_sessions));
    let point = json!({"at":at,"usage_scope":usage_scope,"cpu":sampled["metrics"]["cpu"]["utilization_percent"],"memory":sampled["metrics"]["memory"]["utilization_percent"],"db_ms":db_mean,
        "input_tokens":input,"output_tokens":output,"reported_calls":calls,"known_cost_usd":cost,"active_runs":running.len(),
        "phase_model":model,"phase_tools":tools,"phase_other":running.len().saturating_sub(model+tools)});
    let previous=sqlx::query_scalar::<_,Vec<u8>>("SELECT payload FROM system_dashboard_history WHERE agent=? AND bucket<? ORDER BY bucket DESC LIMIT 1")
        .bind(actor).bind(bucket).fetch_optional(&store.pool).await.map_err(anyhow::Error::from)?
        .map(|raw|store.unseal(&raw)).transpose()?
        .filter(|previous| previous["usage_scope"] == point["usage_scope"]);
    let elapsed = previous
        .as_ref()
        .and_then(|v| v["at"].as_i64())
        .map(|t| (at - t) as f64)
        .unwrap_or(0.0);
    let payload = store.seal(&point)?;
    sqlx::query("INSERT INTO system_dashboard_history(agent,bucket,payload) VALUES(?,?,?) ON CONFLICT(agent,bucket) DO UPDATE SET payload=excluded.payload")
        .bind(actor).bind(bucket).bind(payload).execute(&store.pool).await.map_err(anyhow::Error::from)?;
    sqlx::query("DELETE FROM system_dashboard_history WHERE bucket<?")
        .bind(at - 604800)
        .execute(&store.pool)
        .await
        .map_err(anyhow::Error::from)?;
    let records=sqlx::query_scalar::<_,Vec<u8>>("SELECT payload FROM system_dashboard_history WHERE agent=? AND bucket>=? ORDER BY bucket LIMIT 20161")
        .bind(actor).bind(at-span).fetch_all(&store.pool).await.map_err(anyhow::Error::from)?;
    let points = records
        .into_iter()
        .map(|raw| store.unseal(&raw))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let p95 = quantile(points.iter().filter_map(|p| p["db_ms"].as_f64()).collect());
    let phases = |key: &str| {
        points
            .iter()
            .map(|p| p[key].as_u64().unwrap_or(0))
            .sum::<u64>()
    };
    let total = phases("phase_model") + phases("phase_tools") + phases("phase_other");
    let stride = points.len().div_ceil(600).max(1);
    let mut plotted = points.iter().step_by(stride).cloned().collect::<Vec<_>>();
    if let Some(last) = points.last() {
        if plotted.last().is_none_or(|point| point["at"] != last["at"]) {
            plotted.push(last.clone());
        }
    }
    let output_rate = rate(
        output as f64,
        previous.as_ref().and_then(|v| v["output_tokens"].as_f64()),
        elapsed,
    )
    .filter(|_| {
        calls > 0
            && previous
                .as_ref()
                .and_then(|v| v["reported_calls"].as_u64())
                .is_some_and(|n| n > 0)
    });
    let call_rate = rate(
        calls as f64,
        previous.as_ref().and_then(|v| v["reported_calls"].as_f64()),
        elapsed,
    )
    .filter(|_| calls > 0);
    let (device_data, git) = tokio::join!(tokio::task::spawn_blocking(devices), host.git_status());
    let config = host.runtime_config()?;
    let fds = std::fs::read_dir("/proc/self/fd")
        .ok()
        .map(|entries| entries.filter_map(|entry| entry.ok()).count());
    Ok(
        json!({"schema_version":1,"sampled_at_unix_ms":sampled["sampled_at_unix_ms"],"window":window,
            "scope":{"user":store.current_user_id(),"agent":actor,"usage":"caller-owned active sessions admitted by live membership","runs":"current agent","namespace":"server process namespace","session_limit":2000,"sessions_truncated":truncated},
            "history":{"points":plotted,"stored_samples":points.len(),"plot_interval_seconds":30*stride,"retention_seconds":604800,"resolution_seconds":30,"sampling":"on dashboard requests; gaps are not interpolated","sealed":true},
            "database":{"available":database.iter().all(|v|v["available"]==true),"probes":database,"window_p95_ms":p95,"measurement":"SELECT 1 and journal-mode reads including pool wait; not application-query latency"},
            "devices":device_data.map_err(anyhow::Error::from)?,"git":git,
            "loop":{"available":true,"model_samples":phases("phase_model"),"tool_samples":phases("phase_tools"),"other_samples":phases("phase_other"),"total_samples":total,"measurement":"sampled running-phase occupancy; not instrumented CPU time"},
            "throughput":{"available":output_rate.is_some()&&call_rate.is_some(),"output_tokens_per_second":output_rate,"calls_per_second":call_rate,"interval_seconds":elapsed,"measurement":"provider-reported cumulative counters; resets remain unavailable"},
            "usage":{"input_tokens":input,"output_tokens":output,"reported_calls":calls,"unreported_calls":unreported,"unknown_cost_calls":unknown,"known_cost_usd":cost,"sessions_without_usage":missing,"session_count":sessions.len(),"tools":tool_counts,"agents":agents,"tool_record_limit":500},
            "runtime":{"active_runs":running.len(),"unresolved_runs":runs.len(),"concurrency":config["scheduler"]["max_active_runs"],"open_descriptors":fds,"load_average":std::fs::read_to_string("/proc/loadavg").ok().map(|s|s.split_whitespace().take(3).collect::<Vec<_>>().join(" "))}
        }),
    )
}

#[cfg(test)]
mod tests;
