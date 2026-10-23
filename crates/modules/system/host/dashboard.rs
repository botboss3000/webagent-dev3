//! Scoped dashboard telemetry. SQL probes are read-only; history contains sealed numeric samples.
use super::*;
use axum::extract::Query;
use serde::Deserialize;
use sqlx::Row;
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use webagent_system::dashboard::{quantile, rate, scoped_runs, window_seconds as seconds};

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct Window {
    window: Option<String>,
}

type Cached = HashMap<String, (Instant, Value)>;
fn cache() -> &'static Mutex<Cached> {
    static CACHE: OnceLock<Mutex<Cached>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Administrator middleware supplies the caller's existing Store; no caller-selected state paths.
pub(super) async fn read(State(app): State<App>, Query(args): Query<Window>) -> Api {
    let window = args.window.as_deref().unwrap_or("1h");
    let span = seconds(window).ok_or_else(|| bad("Invalid dashboard time window"))?;
    let key = format!(
        "{}:{}:{}:{window}",
        app.store.root.display(),
        app.store.current_user_id(),
        app.store.current_agent_id()
    );
    if let Some(value) = cache().lock().ok().and_then(|c| {
        c.get(&key)
            .filter(|(at, _)| at.elapsed() < Duration::from_secs(5))
            .map(|(_, v)| v.clone())
    }) {
        return Ok(Json(value));
    }
    let value = collect(&app, span, window).await?;
    if let Ok(mut cache) = cache().lock() {
        if cache.len() >= 32 {
            cache.clear();
        }
        cache.insert(key, (Instant::now(), value.clone()));
    }
    Ok(Json(value))
}

/// Fixed section names project the same authenticated snapshot and window contract.
pub(super) async fn section(
    State(app): State<App>,
    Path(name): Path<String>,
    Query(args): Query<Window>,
) -> Api {
    if ![
        "history",
        "database",
        "devices",
        "git",
        "loop",
        "throughput",
        "runtime",
        "usage",
    ]
    .contains(&name.as_str())
    {
        return Err(bad("Unknown dashboard section"));
    }
    let value = read(State(app), Query(args)).await?.0;
    Ok(Json(
        json!({"schema_version":1,"scope":value["scope"],"window":value["window"],"sampled_at_unix_ms":value["sampled_at_unix_ms"],"data":value[name]}),
    ))
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

/// The broker reads private checkout metadata; the app never gains source filesystem permissions.
async fn git_status(app: &App) -> Value {
    match crate::api::build_dashboard::list(State(app.clone())).await {
        Ok(view) => view.0.get("source_git").cloned().unwrap_or_else(
            || json!({"available":false,"reason":"source_status_bridge_unavailable"}),
        ),
        Err(_) => json!({"available":false,"reason":"source_status_bridge_unavailable"}),
    }
}

async fn collect(app: &App, span: i64, window: &str) -> Result<Value, ApiError> {
    let sampled = tokio::task::spawn_blocking(webagent_system::snapshot)
        .await
        .map_err(anyhow::Error::from)?;
    let at = i64::try_from(crate::now()).map_err(anyhow::Error::from)?;
    let bucket = at - at % 30;
    let actor = app.store.current_agent_id();
    sqlx::query("CREATE TABLE IF NOT EXISTS system_dashboard_history(agent TEXT NOT NULL,bucket INTEGER NOT NULL,payload BLOB NOT NULL,PRIMARY KEY(agent,bucket))").execute(&app.store.pool).await.map_err(anyhow::Error::from)?;
    let database = vec![
        probe(&app.store.app_pool, "application").await,
        probe(&app.store.pool, "current user").await,
        probe(&app.store.agent_pool, "current agent").await,
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
        let (mut page, next) = app
            .store
            .session_statistics_page(false, offset, 100)
            .await?;
        // Keep the existing live membership boundary when aggregating cross-agent caller-owned sessions.
        for item in page.drain(..) {
            if let Some(agent) = item["agent_id"].as_str() {
                if app
                    .store
                    .live_access(app.store.current_user_id(), agent)
                    .await
                    .is_ok_and(|a| a.admitted())
                {
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
        .fetch_all(&app.store.pool).await.map_err(anyhow::Error::from)?;
    let mut tool_counts = std::collections::BTreeMap::<String, u64>::new();
    for row in rows {
        let sid: String = row.get("session_id");
        if !admitted.contains(sid.as_str()) {
            continue;
        }
        let item = app.store.unseal(&row.get::<Vec<u8>, _>("item"))?;
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
    let runs = scoped_runs(app.store.active_runs().await?, &sessions, actor);
    let running = runs
        .iter()
        .filter(|r| r["status"] == "running")
        .collect::<Vec<_>>();
    let model = running.iter().filter(|r| r["phase"] == "model").count();
    let tools = running.iter().filter(|r| r["phase"] == "tools").count();
    let mut usage_sessions = admitted.iter().copied().collect::<Vec<_>>();
    usage_sessions.sort_unstable();
    let usage_scope = crate::digest(&json!(usage_sessions));
    let point = json!({"at":at,"usage_scope":usage_scope,"cpu":sampled["metrics"]["cpu"]["utilization_percent"],"memory":sampled["metrics"]["memory"]["utilization_percent"],"db_ms":db_mean,
        "input_tokens":input,"output_tokens":output,"reported_calls":calls,"known_cost_usd":cost,"active_runs":running.len(),
        "phase_model":model,"phase_tools":tools,"phase_other":running.len().saturating_sub(model+tools)});
    let previous=sqlx::query_scalar::<_,Vec<u8>>("SELECT payload FROM system_dashboard_history WHERE agent=? AND bucket<? ORDER BY bucket DESC LIMIT 1")
        .bind(actor).bind(bucket).fetch_optional(&app.store.pool).await.map_err(anyhow::Error::from)?
        .map(|raw|app.store.unseal(&raw)).transpose()?
        .filter(|previous| previous["usage_scope"] == point["usage_scope"]);
    let elapsed = previous
        .as_ref()
        .and_then(|v| v["at"].as_i64())
        .map(|t| (at - t) as f64)
        .unwrap_or(0.0);
    let payload = app.store.seal(&point)?;
    sqlx::query("INSERT INTO system_dashboard_history(agent,bucket,payload) VALUES(?,?,?) ON CONFLICT(agent,bucket) DO UPDATE SET payload=excluded.payload")
        .bind(actor).bind(bucket).bind(payload).execute(&app.store.pool).await.map_err(anyhow::Error::from)?;
    sqlx::query("DELETE FROM system_dashboard_history WHERE bucket<?")
        .bind(at - 604800)
        .execute(&app.store.pool)
        .await
        .map_err(anyhow::Error::from)?;
    let records=sqlx::query_scalar::<_,Vec<u8>>("SELECT payload FROM system_dashboard_history WHERE agent=? AND bucket>=? ORDER BY bucket LIMIT 20161")
        .bind(actor).bind(at-span).fetch_all(&app.store.pool).await.map_err(anyhow::Error::from)?;
    let points = records
        .into_iter()
        .map(|raw| app.store.unseal(&raw))
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
    let (device_data, git) = tokio::join!(tokio::task::spawn_blocking(devices), git_status(app));
    let config = serde_json::to_value(loop_runtime::Config::read(&app.runtime_path)?)
        .map_err(anyhow::Error::from)?;
    let fds = std::fs::read_dir("/proc/self/fd")
        .ok()
        .map(|entries| entries.filter_map(|entry| entry.ok()).count());
    Ok(
        json!({"schema_version":1,"sampled_at_unix_ms":sampled["sampled_at_unix_ms"],"window":window,
            "scope":{"user":app.store.current_user_id(),"agent":actor,"usage":"caller-owned active sessions admitted by live membership","runs":"current agent","namespace":"server process namespace","session_limit":2000,"sessions_truncated":truncated},
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
mod tests {
    use super::*;
    #[tokio::test]
    async fn dashboard_sections_validate_and_history_is_scoped_and_sealed() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let _ = collect(&app, 3600, "1h").await.unwrap();
        let foreign = app
            .store
            .seal(&json!({"at":crate::now()-60,"foreign":true}))
            .unwrap();
        sqlx::query("INSERT INTO system_dashboard_history(agent,bucket,payload) VALUES(?,?,?)")
            .bind("another-agent")
            .bind(i64::try_from(crate::now() - 60).unwrap())
            .bind(foreign)
            .execute(&app.store.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM system_dashboard_history WHERE agent=?")
            .bind(app.store.current_agent_id())
            .execute(&app.store.pool)
            .await
            .unwrap();
        let old = app.store.seal(&json!({"at":crate::now()-120,"usage_scope":"different-scope","output_tokens":10,"reported_calls":1})).unwrap();
        sqlx::query("INSERT INTO system_dashboard_history(agent,bucket,payload) VALUES(?,?,?)")
            .bind(app.store.current_agent_id())
            .bind(i64::try_from(crate::now() - 120).unwrap())
            .bind(old)
            .execute(&app.store.pool)
            .await
            .unwrap();
        let value = collect(&app, 3600, "1h").await.unwrap();
        assert_eq!(value["throughput"]["interval_seconds"], 0.0);
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["history"]["sealed"], true);
        assert_eq!(value["history"]["points"].as_array().unwrap().len(), 2);
        assert!(value["database"]["probes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["available"] == true));
        assert!(read(
            State(app.clone()),
            Query(Window {
                window: Some("invalid".into())
            })
        )
        .await
        .is_err());
        assert!(section(
            State(app.clone()),
            Path("secrets".into()),
            Query(Window::default())
        )
        .await
        .is_err());
        let raw: Vec<u8> =
            sqlx::query_scalar("SELECT payload FROM system_dashboard_history LIMIT 1")
                .fetch_one(&app.store.pool)
                .await
                .unwrap();
        assert!(serde_json::from_slice::<Value>(&raw).is_err());
        assert!(app.store.unseal(&raw).is_ok());
    }
}
