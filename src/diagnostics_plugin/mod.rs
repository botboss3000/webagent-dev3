//! Admin-only evidence. No global server/HTTP content or public-user events.
mod config;
#[cfg(test)]
mod tests;
use crate::{context_assembly::Prepared, context_input::Request, Store};
use axum::{extract::Query, routing::get, Extension, Json, Router};
use config::Config;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, OnceLock, RwLock,
    },
    time::Duration,
};
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};
static RECORDER: OnceLock<Recorder> = OnceLock::new();
const SNAPSHOT_QUEUE_BYTES: usize = 64 * 1024 * 1024;
#[derive(Clone)]
struct Settings {
    config: Config,
    digest: Option<String>,
    error: Option<&'static str>,
}
impl Settings {
    fn read(path: &Path) -> Self {
        match Config::read(path) {
            Ok((config, digest)) => Self {
                config,
                digest: Some(digest),
                error: None,
            },
            Err(_) => Self {
                config: Config::disabled(),
                digest: None,
                error: Some("invalid_or_missing_configuration"),
            },
        }
    }
}
struct Recorder {
    pool: SqlitePool,
    settings: RwLock<Settings>,
    tx: mpsc::Sender<Value>,
    snapshots: mpsc::Sender<Snapshot>,
    budget: Arc<Semaphore>,
    dropped: AtomicU64,
    write_errors: AtomicU64,
    skipped: AtomicU64,
    role: String,
}
struct Snapshot {
    store: Store,
    payload: Value,
    bytes: usize,
    _permit: OwnedSemaphorePermit,
}
fn identifier(value: &Value) -> Value {
    value
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-:.".contains(&b))
        })
        .map(|s| json!(s))
        .unwrap_or(Value::Null)
}
fn scope(store: &Store) -> bool {
    store.current_user_id() == "admin"
}
fn settings() -> Option<Settings> {
    RECORDER.get()?.settings.read().ok().map(|s| s.clone())
}
fn permits(settings: &Settings, category: &str) -> bool {
    settings.error.is_none()
        && settings.config.active()
        && match category {
            "provider" => settings.config.categories.provider,
            "context_assembly" => settings.config.categories.context_assembly,
            "run" => settings.config.categories.run,
            "tool" => settings.config.categories.tool,
            "recovery" => settings.config.categories.recovery,
            _ => false,
        }
}
pub(crate) async fn init(root: &Path, _store: &Store, role: &str) {
    if RECORDER.get().is_some() {
        return;
    }
    let path = root.join("config/diagnostics.json");
    // First start seeds once, preserving every existing operator setting.
    if let Err(error) = seed(&path) {
        tracing::warn!(kind=?error.kind(),"Diagnostics configuration seed unavailable");
    }
    let pool = match open(root).await {
        Ok(pool) => pool,
        Err(_) => {
            tracing::error!("Diagnostic archive unavailable");
            return;
        }
    };
    let (tx, mut rx) = mpsc::channel(4096);
    let (snapshots, mut sx) = mpsc::channel::<Snapshot>(8);
    if RECORDER
        .set(Recorder {
            pool: pool.clone(),
            settings: RwLock::new(Settings::read(&path)),
            tx,
            snapshots,
            budget: Arc::new(Semaphore::new(SNAPSHOT_QUEUE_BYTES)),
            dropped: AtomicU64::new(0),
            write_errors: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            role: role.into(),
        })
        .is_err()
    {
        return;
    }
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let Some(s) = settings() else {
                continue;
            };
            if !event_allowed(&s, &event) {
                continue;
            }
            if persist(&pool, &event).await.is_err() {
                RECORDER
                    .get()
                    .unwrap()
                    .write_errors
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    });
    let pool = RECORDER.get().unwrap().pool.clone();
    tokio::spawn(async move {
        while let Some(snapshot) = sx.recv().await {
            if persist_snapshot(&pool, snapshot).await.is_err() {
                RECORDER
                    .get()
                    .unwrap()
                    .write_errors
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    });
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        let mut ticks = 0u64;
        loop {
            interval.tick().await;
            let p = path.clone();
            let refreshed = tokio::task::spawn_blocking(move || Settings::read(&p)).await;
            let Some(r) = RECORDER.get() else {
                return;
            };
            if let Ok(s) = refreshed {
                if let Ok(mut state) = r.settings.write() {
                    *state = s;
                }
            }
            if heartbeat(r).await.is_err() {
                r.write_errors.fetch_add(1, Ordering::Relaxed);
            }
            if ticks.is_multiple_of(24) {
                if let Some(s) = settings() {
                    if prune(&r.pool, &s.config).await.is_err() {
                        r.write_errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            ticks = ticks.wrapping_add(1);
        }
    });
}
fn seed(path: &Path) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(path.parent().unwrap())?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => file.write_all(include_bytes!("../../config/diagnostics.json")),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}
async fn open(root: &Path) -> anyhow::Result<SqlitePool> {
    let dir = root.join("diagnostics");
    tokio::fs::create_dir_all(&dir).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).await?;
    }
    let options = SqliteConnectOptions::new()
        .filename(dir.join("logs.db"))
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(2));
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await?;
    for statement in [
  "CREATE TABLE IF NOT EXISTS diagnostics(id INTEGER PRIMARY KEY,ts INTEGER NOT NULL,category TEXT NOT NULL,run_id TEXT,session_id TEXT,payload TEXT NOT NULL)",
  "CREATE INDEX IF NOT EXISTS diagnostics_ts ON diagnostics(ts)",
  "CREATE INDEX IF NOT EXISTS diagnostics_run ON diagnostics(run_id,id)",
  "CREATE INDEX IF NOT EXISTS diagnostics_session ON diagnostics(session_id,id)",
  "CREATE TABLE IF NOT EXISTS context_snapshots(attempt_id TEXT PRIMARY KEY,ts INTEGER NOT NULL,expires_at INTEGER NOT NULL,session_id TEXT NOT NULL,run_id TEXT NOT NULL,bytes INTEGER NOT NULL,sealed BLOB NOT NULL)",
  "CREATE INDEX IF NOT EXISTS context_snapshots_age ON context_snapshots(ts)",
  "CREATE TABLE IF NOT EXISTS diagnostic_processes(role TEXT PRIMARY KEY,ts INTEGER NOT NULL,payload TEXT NOT NULL)",
 ] {sqlx::query(statement).execute(&pool).await?;}
    Ok(pool)
}
fn event_allowed(s: &Settings, event: &Value) -> bool {
    event["owner"] == "admin"
        && event["config_digest"].as_str() == s.digest.as_deref()
        && permits(s, event["category"].as_str().unwrap_or(""))
}
async fn persist(pool: &SqlitePool, event: &Value) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO diagnostics(ts,category,run_id,session_id,payload) VALUES(?,?,?,?,?)")
        .bind(crate::now() as i64)
        .bind(event["category"].as_str().unwrap_or("context_assembly"))
        .bind(event["run_id"].as_str())
        .bind(event["session_id"].as_str())
        .bind(event.to_string())
        .execute(pool)
        .await?;
    Ok(())
}
async fn persist_snapshot(pool: &SqlitePool, snapshot: Snapshot) -> anyhow::Result<()> {
    let Some(s) = settings() else {
        return Ok(());
    };
    if !snapshot_allowed(&s, &snapshot.payload) {
        return Ok(());
    }
    let Snapshot {
        store,
        payload,
        bytes,
        _permit,
    } = snapshot;
    let (payload, sealed) =
        tokio::task::spawn_blocking(move || store.seal(&payload).map(|sealed| (payload, sealed)))
            .await??;
    // Configuration can change during encryption; disable withdraws pending capture.
    let Some(s) = settings() else {
        return Ok(());
    };
    if !snapshot_allowed(&s, &payload) {
        return Ok(());
    }
    insert_snapshot(pool, &payload, &sealed, bytes, &s.config).await?;
    prune(pool, &s.config).await
}
fn snapshot_allowed(s: &Settings, payload: &Value) -> bool {
    event_allowed(s, payload)
        && s.config
            .selected(payload["session_id"].as_str().unwrap_or(""), crate::now())
}
async fn insert_snapshot(
    pool: &SqlitePool,
    payload: &Value,
    sealed: &[u8],
    bytes: usize,
    config: &Config,
) -> anyhow::Result<()> {
    sqlx::query("INSERT OR IGNORE INTO context_snapshots(attempt_id,ts,expires_at,session_id,run_id,bytes,sealed) VALUES(?,?,?,?,?,?,?)")
  .bind(payload["attempt_id"].as_str()).bind(crate::now() as i64)
  .bind((crate::now()+config.full_context.retention_hours*3600) as i64)
  .bind(payload["session_id"].as_str()).bind(payload["run_id"].as_str()).bind(bytes as i64).bind(sealed).execute(pool).await?;
    Ok(())
}
async fn prune(pool: &SqlitePool, config: &Config) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM diagnostics WHERE id IN (SELECT id FROM diagnostics WHERE ts<? ORDER BY id LIMIT 1000)")
  .bind(crate::now() as i64-(config.retention.hours*3600) as i64).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM diagnostics WHERE id IN (SELECT id FROM diagnostics WHERE id<=COALESCE((SELECT id FROM diagnostics ORDER BY id DESC LIMIT 1 OFFSET ?),0) ORDER BY id LIMIT 1000)")
  .bind(config.retention.max_rows as i64).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM context_snapshots WHERE attempt_id IN (SELECT attempt_id FROM context_snapshots WHERE expires_at<=? OR ts<? ORDER BY ts LIMIT 1000)")
  .bind(crate::now() as i64).bind(crate::now() as i64-(config.full_context.retention_hours*3600) as i64).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM context_snapshots WHERE attempt_id IN (SELECT attempt_id FROM (SELECT attempt_id,SUM(bytes) OVER (ORDER BY ts DESC,attempt_id DESC) retained FROM context_snapshots) WHERE retained>? LIMIT 1000)")
  .bind(config.full_context.max_retained_bytes as i64).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
fn emit(event: Value) {
    if let Some(r) = RECORDER.get() {
        if r.tx.try_send(event).is_err() {
            r.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}
pub(crate) fn provider_enabled(store: &Store) -> bool {
    scope(store) && settings().is_some_and(|s| permits(&s, "provider"))
}
pub(crate) fn failure(store: &Store, run: &Value, error: &anyhow::Error, action: &str) {
    if !scope(store) {
        return;
    }
    let Some(s) = settings().filter(|s| permits(s, "provider")) else {
        return;
    };
    let failure = error.downcast_ref::<crate::provider::ProviderFailure>();
    emit(
        json!({"category":"provider","level":"error","owner":"admin","config_digest":s.digest,"message":"Model request failed",
  "run_id":identifier(&run["id"]),"session_id":identifier(&run["session_id"]),
  "provider":serde_json::from_value::<crate::provider::ProviderId>(run["provider_selection"]["provider"].clone()).ok(),
  "model":identifier(&run["provider_selection"]["model"]),"kind":failure.map(|f|f.kind),
  "response_started":failure.map(|f|f.response_started),"http_status":failure.and_then(|f|f.http_status),"transport":failure.and_then(|f|f.transport.clone()),
  "action":match action {"retry"=>"retry","backup"=>"backup",_=>"terminal"}}),
    );
}
// Observe only committed transitions. No raw payload, errors, arguments or results enter telemetry.
pub(crate) fn committed(store: &Store, run: &Value, kind: &str, recovery: bool) {
    if !scope(store) {
        return;
    }
    let Some(s) = settings() else {
        return;
    };
    if let Some(event) = transition(&s, run, kind, recovery) {
        emit(event);
    }
}
fn transition(s: &Settings, run: &Value, kind: &str, recovery: bool) -> Option<Value> {
    let category = if recovery {
        "recovery"
    } else {
        match kind {
            "tool_result" | "approval_required" => "tool",
            "run_status" | "resumed" | "context_compacted" => "run",
            _ => return None,
        }
    };
    if !permits(s, category) {
        return None;
    }
    let status = match run["status"].as_str()? {
        v @ ("queued" | "running" | "completed" | "cancelled" | "failed" | "stopped" | "paused"
        | "recovery" | "waiting_approval") => v,
        _ => "other",
    };
    let pending_state = match run["pending"]["state"].as_str() {
        Some(
            v @ ("requested" | "approved" | "denied" | "executing" | "completed" | "uncertain"),
        ) => Some(v),
        _ => None,
    };
    Some(
        json!({"owner":"admin","category":category,"level":if matches!(status,"failed"|"paused"|"recovery") {"warning"} else {"info"},
        "config_digest":s.digest,"message":"Committed execution transition",
        "event_kind":match kind {"tool_result"=>"tool_result","approval_required"=>"approval_required","resumed"=>"resumed","context_compacted"=>"context_compacted",_=>"run_status"},
        "session_id":identifier(&run["session_id"]),"run_id":identifier(&run["id"]),"agent_id":identifier(&run["agent_id"]),
        "status":status,"phase":match run["phase"].as_str() {Some(v @ ("model"|"tool"|"publication"|"approval"))=>Some(v),_=>None},
        "run_revision":run["run_revision"].as_u64(),"generation":run["generation"].as_u64(),"lease_epoch":run["lease_epoch"].as_u64(),
        "turns":run["turns"].as_u64(),"call_index":run["call_index"].as_u64(),
        "call_id":identifier(&run["pending"]["call_id"]),"pending_state":pending_state,"uncertain":run["uncertain"]==true,
        "recovery_override":recovery}),
    )
}

// These unscoped hooks deliberately capture nothing.
pub(crate) fn backend_error() {}
pub(crate) async fn http_errors(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    next.run(request).await
}

#[derive(Serialize)]
struct Capture<'a> {
    owner: &'static str,
    category: &'static str,
    config_digest: &'a Option<String>,
    attempt_id: &'a str,
    session_id: &'a Value,
    run_id: &'a Value,
    provider_selection: Value,
    report: &'a Value,
    assembled_instructions: Option<String>,
    history: &'a [Value],
    base: &'a str,
    guidance: &'a str,
    schemas: &'a [Value],
    contributions: &'a [crate::context_input::Contribution],
    compaction: &'a crate::context_compaction::Config,
    max_bytes: usize,
    turn: u64,
    call: u64,
}
pub(crate) fn assembly(
    store: &Store,
    run: &Value,
    request: &Request<'_>,
    prepared: &anyhow::Result<Prepared<'_>>,
) -> Option<Value> {
    let (outcome, code, stage) = match prepared {
        Ok(Prepared::Ready(_)) => ("ready", "", "assembly"),
        Ok(Prepared::Compact(_)) => ("compaction_required", "", "compaction_plan"),
        Err(e) => {
            let (code, stage) = crate::context_assembly::failure_code(e);
            ("failed", code, stage)
        }
    };
    capture_attempt(store, run, request, outcome, code, stage, Some(prepared))
}
pub(crate) fn summary(
    store: &Store,
    run: &Value,
    request: &Request<'_>,
    result: &anyhow::Result<()>,
) -> Option<Value> {
    capture_attempt(
        store,
        run,
        request,
        if result.is_ok() { "ready" } else { "failed" },
        if result.is_ok() {
            ""
        } else {
            "system_summary_input_unavailable"
        },
        "summary_input",
        None,
    )
}
fn capture_attempt(
    store: &Store,
    run: &Value,
    request: &Request<'_>,
    outcome: &str,
    code: &str,
    stage: &str,
    prepared: Option<&anyhow::Result<Prepared<'_>>>,
) -> Option<Value> {
    if !scope(store) {
        return None;
    }
    let s = settings().filter(|s| permits(s, "context_assembly"))?;
    let selected = s
        .config
        .selected(run["session_id"].as_str().unwrap_or(""), crate::now());
    if outcome != "failed" && !s.config.assembly_successes && !selected {
        return None;
    }
    let attempt = crate::uuid();
    let mut report = measure(request);
    report["outcome"] = json!(outcome);
    report["failure_code"] = json!(code);
    report["stage"] = json!(stage);
    if let Some(Ok(Prepared::Ready(envelope))) = prepared {
        report["successful_envelope"] = serde_json::to_value(&envelope.report).ok()?;
    }
    if let Some(Ok(Prepared::Compact(plan))) = prepared {
        report["plan"] = json!({"drop_until":plan.drop_until,"source_digest":plan.source_digest,"source_bytes":crate::context_accounting::json_size(&plan.source).ok()});
    }
    let mut event = json!({"category":"context_assembly","owner":"admin","config_digest":s.digest,"attempt_id":attempt,
  "level":if outcome=="failed"{"warning"}else{"info"},"run_id":identifier(&run["id"]),"session_id":identifier(&run["session_id"]),
  "report":report,"snapshot_status":if selected{"pending"}else{"not_selected"}});
    if selected {
        let capture = Capture {
            owner: "admin",
            category: "context_assembly",
            config_digest: &s.digest,
            attempt_id: &attempt,
            session_id: &run["session_id"],
            run_id: &run["id"],
            provider_selection: request.selection.value(),
            report: &report,
            assembled_instructions: assembled_instructions(request),
            history: request.history,
            base: request.base,
            guidance: request.guidance,
            schemas: request.schemas,
            contributions: request.contributions,
            compaction: request.compaction,
            max_bytes: request.max_bytes,
            turn: request.identity.turn,
            call: request.identity.call,
        };
        let r = RECORDER.get()?;
        let bytes = crate::context_accounting::json_size(&capture).unwrap_or(usize::MAX);
        if bytes > s.config.full_context.max_snapshot_bytes || bytes > SNAPSHOT_QUEUE_BYTES {
            event["snapshot_status"] = json!("oversized");
        } else if let Ok(permit) = r.budget.clone().try_acquire_many_owned(bytes as u32) {
            // Gate/measure before making any owned copy of private inputs.
            match serde_json::to_value(capture) {
                Ok(payload) => {
                    if r.snapshots
                        .try_send(Snapshot {
                            store: store.clone(),
                            payload,
                            bytes,
                            _permit: permit,
                        })
                        .is_err()
                    {
                        event["snapshot_status"] = json!("queue_full");
                    }
                }
                Err(_) => {
                    event["snapshot_status"] = json!("serialization_failed");
                }
            }
        } else {
            event["snapshot_status"] = json!("queue_byte_limit");
        }
        event["snapshot_bytes"] = json!(bytes);
        if event["snapshot_status"] != "pending" {
            r.skipped.fetch_add(1, Ordering::Relaxed);
        }
    }
    emit(event.clone());
    Some(event)
}
fn assembled_instructions(request: &Request<'_>) -> Option<String> {
    let decisions = crate::context_input::select(request.contributions, request.identity).ok()?;
    let mut instructions = format!("{}{}", request.base, request.guidance);
    for d in decisions {
        if d.disposition == "included" {
            instructions.push('\n');
            instructions.push_str(&d.item.content);
        }
    }
    Some(instructions)
}
fn measure(request: &Request<'_>) -> Value {
    let mut instructions = format!("{}{}", request.base, request.guidance);
    let decisions = crate::context_input::select(request.contributions, request.identity);
    let mut overlays = Vec::new();
    if let Ok(decisions) = decisions {
        for d in decisions {
            if d.disposition == "included" {
                instructions.push('\n');
                instructions.push_str(&d.item.content);
            }
            overlays.push(json!({"id":identifier(&json!(d.item.id)),"disposition":d.disposition,"bytes":d.item.content.len()}));
        }
    }
    let instruction = crate::context_accounting::json_size(&instructions).unwrap_or(0);
    let schemas = crate::context_accounting::json_size(request.schemas).unwrap_or(0);
    let history = crate::context_accounting::json_size(request.history).unwrap_or(0);
    let framing = b"{\"instructions\":,\"tools\":,\"input\":}".len();
    let budget = request
        .max_bytes
        .checked_sub(framing + instruction + schemas + 2);
    let mut pending = std::collections::HashSet::new();
    let mut seen = std::collections::HashSet::new();
    let mut duplicate = 0;
    let mut unmatched = 0;
    let mut boundaries = 0;
    let mut source_rejections = 0;
    let mut tail_rejections = 0;
    let mut prefix = 2usize;
    let mut valid = true;
    let mut accounting = crate::context_accounting::ContextAccounting::default();
    if accounting.sync(request.history).is_err() {
        return json!({"measurements_unavailable":true});
    }
    let (sizes, characters) = accounting.measurements();
    let mut suffix = vec![2usize; request.history.len() + 1];
    let mut tail_chars = vec![0usize; request.history.len() + 1];
    for i in (0..request.history.len()).rev() {
        suffix[i] = suffix[i + 1] + sizes[i] + usize::from(i + 1 < request.history.len());
        tail_chars[i] = tail_chars[i + 1] + characters[i];
    }
    let mut tail_character_rejections = 0;
    for (i, item) in request.history.iter().enumerate() {
        let prior = i > 0 && item["role"] == "user" && valid && pending.is_empty();
        prefix += sizes[i] + usize::from(i > 0);
        match item["type"].as_str() {
            Some("function_call") => {
                if let Some(id) = item["call_id"].as_str().filter(|s| !s.is_empty()) {
                    if !seen.insert(id) {
                        duplicate += 1;
                        valid = false;
                    } else {
                        pending.insert(id);
                    }
                } else {
                    unmatched += 1;
                    valid = false;
                }
            }
            Some("function_call_output") => {
                if !item["call_id"]
                    .as_str()
                    .is_some_and(|id| pending.remove(id))
                {
                    unmatched += 1;
                    valid = false;
                }
            }
            _ => {}
        }
        let index = if prior {
            Some(i)
        } else if item["type"] == "function_call_output" && pending.is_empty() && valid {
            Some(i + 1)
        } else {
            None
        };
        if let Some(index) = index {
            boundaries += 1;
            let source = if prior {
                prefix - sizes[i] - usize::from(i > 0)
            } else {
                prefix
            };
            if source > request.compaction.max_source_bytes {
                source_rejections += 1;
            }
            let tail = suffix[index];
            if tail_chars[index]
                > request.compaction.tail_tokens * request.compaction.characters_per_token
            {
                tail_character_rejections += 1;
            }
            if budget
                .and_then(|b| b.checked_sub(request.compaction.summary_reserve_bytes))
                .is_none_or(|b| tail > b)
            {
                tail_rejections += 1;
            }
        }
    }
    json!({"accounting":"serialized_json_bytes_not_provider_tokens","model":identifier(&json!(request.selection.model)),
  "max_bytes":request.max_bytes,"instructions_json_bytes":instruction,"base_content_bytes":request.base.len(),
  "guidance_content_bytes":request.guidance.len(),"schemas_json_bytes":schemas,"history_json_bytes":history,"framing_bytes":framing,
  "history_budget_bytes":budget,"summary_reserve_bytes":request.compaction.summary_reserve_bytes,
  "trigger_tokens_estimated":request.compaction.trigger_tokens,"tail_tokens_estimated":request.compaction.tail_tokens,
  "characters_per_token":request.compaction.characters_per_token,"overlays":overlays,
  "compaction":{"candidate_boundaries":boundaries,"pending_calls":pending.len(),"duplicate_calls":duplicate,
   "unmatched_calls_or_results":unmatched,"source_limit_rejections":source_rejections,"tail_byte_rejections":tail_rejections,"tail_character_rejections":tail_character_rejections,"history_triggered":characters.iter().sum::<usize>()>request.compaction.trigger_tokens*request.compaction.characters_per_token}})
}
pub(crate) fn validation(
    store: &Store,
    run: &Value,
    code: &'static str,
    stage: &'static str,
) -> Option<Value> {
    if !scope(store) {
        return None;
    }
    let s = settings().filter(|s| permits(s, "context_assembly"))?;
    let event = json!({"owner":"admin","category":"context_assembly","level":"warning","config_digest":s.digest,
  "attempt_id":crate::uuid(),"parent_attempt_id":run["latest_context_assembly"]["attempt_id"],
  "session_id":identifier(&run["session_id"]),"run_id":identifier(&run["id"]),
  "report":{"outcome":"failed","failure_code":code,"stage":stage}});
    emit(event.clone());
    Some(event)
}
fn status_value(r: &Recorder) -> Value {
    let s = settings().unwrap_or(Settings {
        config: Config::disabled(),
        digest: None,
        error: Some("configuration_lock_unavailable"),
    });
    json!({"role":r.role,"capture_scope":"fixed_admin","enabled":s.config.active(),"config_digest":s.digest,"configuration_error":s.error,
  "categories":s.config.categories,"assembly_successes":s.config.assembly_successes,"full_context":s.config.full_context,
  "retention":s.config.retention,"queue_depth":r.tx.max_capacity()-r.tx.capacity(),
  "snapshot_queue_depth":r.snapshots.max_capacity()-r.snapshots.capacity(),"snapshot_queue_estimated_bytes":SNAPSHOT_QUEUE_BYTES-r.budget.available_permits(),
  "dropped":r.dropped.load(Ordering::Relaxed),"write_errors":r.write_errors.load(Ordering::Relaxed),"skipped_snapshots":r.skipped.load(Ordering::Relaxed)})
}
async fn heartbeat(r: &Recorder) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO diagnostic_processes(role,ts,payload) VALUES(?,?,?) ON CONFLICT(role) DO UPDATE SET ts=excluded.ts,payload=excluded.payload")
  .bind(&r.role).bind(crate::now() as i64).bind(status_value(r).to_string()).execute(&r.pool).await?;
    Ok(())
}
async fn authorize(app: &crate::App, identity: &Value) -> Result<(), crate::ApiError> {
    if !scope(&app.store) {
        return Err(crate::ApiError(
            axum::http::StatusCode::FORBIDDEN,
            "Fixed admin account required".into(),
        ));
    }
    crate::phase3_api::administrator(app, identity).await
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Filter {
    run_id: Option<String>,
    session_id: Option<String>,
    before: Option<i64>,
    limit: Option<i64>,
    category: Option<String>,
    level: Option<String>,
}
async fn read(
    Extension(app): Extension<crate::App>,
    Extension(identity): Extension<Value>,
    Query(filter): Query<Filter>,
) -> crate::Api {
    authorize(&app, &identity).await?;
    let Some(r) = RECORDER.get() else {
        return Ok(Json(json!({"enabled":false,"records":[]})));
    };
    for id in [&filter.run_id, &filter.session_id].into_iter().flatten() {
        if identifier(&json!(id)).is_null() {
            return Err(crate::bad("Invalid diagnostic filter"));
        }
    }
    if filter.category.as_deref().is_some_and(|c| {
        !matches!(
            c,
            "provider" | "context_assembly" | "run" | "tool" | "recovery"
        )
    }) || filter
        .level
        .as_deref()
        .is_some_and(|c| !matches!(c, "info" | "warning" | "error"))
    {
        return Err(crate::bad("Invalid diagnostic filter"));
    }
    let rows:Vec<(i64,i64,String)>=sqlx::query_as("SELECT id,ts,payload FROM diagnostics WHERE (? IS NULL OR run_id=?) AND (? IS NULL OR session_id=?) AND id<? AND json_extract(payload,'$.owner')='admin' AND (? IS NULL OR category=?) AND (? IS NULL OR json_extract(payload,'$.level')=?) ORDER BY id DESC LIMIT ?")
  .bind(&filter.run_id).bind(&filter.run_id).bind(&filter.session_id).bind(&filter.session_id).bind(filter.before.unwrap_or(i64::MAX))
  .bind(&filter.category).bind(&filter.category).bind(&filter.level).bind(&filter.level).bind(filter.limit.unwrap_or(100).clamp(1,500))
  .fetch_all(&r.pool).await.map_err(anyhow::Error::from)?;
    Ok(Json(
        json!({"enabled":settings().is_some_and(|s|s.config.active()),"records":rows.into_iter().map(|(id,ts,payload)|json!({"id":id,"ts":ts,"event":serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null)})).collect::<Vec<_>>()}),
    ))
}
async fn status(
    Extension(app): Extension<crate::App>,
    Extension(identity): Extension<Value>,
) -> crate::Api {
    authorize(&app, &identity).await?;
    let Some(r) = RECORDER.get() else {
        return Ok(Json(json!({"available":false,"enabled":false})));
    };
    let rows: Vec<(String, i64, String)> =
        sqlx::query_as("SELECT role,ts,payload FROM diagnostic_processes")
            .fetch_all(&r.pool)
            .await
            .map_err(anyhow::Error::from)?;
    let metadata_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM diagnostics WHERE json_extract(payload,'$.owner')='admin'",
    )
    .fetch_one(&r.pool)
    .await
    .map_err(anyhow::Error::from)?;
    let snapshots: (i64, i64) =
        sqlx::query_as("SELECT COUNT(*),COALESCE(SUM(bytes),0) FROM context_snapshots")
            .fetch_one(&r.pool)
            .await
            .map_err(anyhow::Error::from)?;
    Ok(Json(
        json!({"available":true,"current_process":status_value(r),"retained":{"metadata_rows":metadata_rows,"snapshot_rows":snapshots.0,"snapshot_payload_bytes":snapshots.1},"processes":rows.into_iter().map(|(role,ts,payload)|json!({"role":role,"updated_at":ts,"stale":crate::now().saturating_sub(ts as u64)>15,"status":serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null)})).collect::<Vec<_>>()}),
    ))
}
pub(crate) async fn context(
    store: &Store,
    sid: &str,
    rid: &str,
    attempt: &str,
) -> Result<Value, crate::ApiError> {
    if !scope(store) {
        return Err(crate::ApiError(
            axum::http::StatusCode::FORBIDDEN,
            "Fixed admin account required".into(),
        ));
    }
    let Some(r) = RECORDER.get() else {
        return Err(crate::ApiError(
            axum::http::StatusCode::NOT_FOUND,
            "Snapshot unavailable".into(),
        ));
    };
    read_snapshot(&r.pool, store, sid, rid, attempt).await
}
async fn read_snapshot(
    pool: &SqlitePool,
    store: &Store,
    sid: &str,
    rid: &str,
    attempt: &str,
) -> Result<Value, crate::ApiError> {
    let row:Option<(Vec<u8>,)>=sqlx::query_as("SELECT sealed FROM context_snapshots WHERE attempt_id=? AND session_id=? AND run_id=? AND expires_at>?")
  .bind(attempt).bind(sid).bind(rid).bind(crate::now() as i64).fetch_optional(pool).await.map_err(anyhow::Error::from)?;
    let Some((sealed,)) = row else {
        return Err(crate::ApiError(
            axum::http::StatusCode::NOT_FOUND,
            "Snapshot unavailable or expired".into(),
        ));
    };
    let value = store.unseal(&sealed)?;
    if value["owner"] != "admin"
        || value["session_id"] != sid
        || value["run_id"] != rid
        || value["attempt_id"] != attempt
    {
        return Err(crate::ApiError(
            axum::http::StatusCode::NOT_FOUND,
            "Snapshot identity mismatch".into(),
        ));
    }
    Ok(value)
}
async fn no_store(mut response: axum::response::Response) -> axum::response::Response {
    response.headers_mut().insert(
        "cache-control",
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}
pub(crate) fn router() -> Router<crate::App> {
    Router::new()
        .route("/admin/diagnostics", get(read))
        .route("/admin/diagnostics/status", get(status))
        .layer(axum::middleware::map_response(no_store))
}
