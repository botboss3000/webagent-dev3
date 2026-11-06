//! The administrator build dashboard: a projection of the protected build broker and its
//! targeted controls. Administrator authentication and CSRF are enforced by the root's
//! router before these run, except for `task`, the public Broker card page's read.
use crate::{trusted, AdministrationHost};
use axum::http::StatusCode;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use webagent_administration::dashboard::{
    reply_complete, valid_job_id, CancelRequest, SchedulerRequest, DASHBOARD_SESSION,
};
use webagent_administration::notices::{reviewed, valid_notice_id};
use webagent_host_core::{bad, digest, ApiError};

async fn broker(
    host: &dyn AdministrationHost,
    name: &str,
    arguments: Value,
) -> Result<Value, ApiError> {
    let user = host.store().current_user_id();
    let fingerprint = json!(digest(&json!(["v3_build_dashboard", user])));
    let identity = trusted(user, DASHBOARD_SESSION, &fingerprint);
    let response = host
        .broker(
            name,
            &arguments,
            DASHBOARD_SESSION,
            Some(identity),
            CancellationToken::new(),
        )
        .await?;
    if name == "v3_build_scheduler" && response["status"] != "success" {
        let message = response["output"]
            .as_str()
            .unwrap_or("Scheduler conflict; refresh ownership and job status");
        return Err(ApiError(StatusCode::CONFLICT, message.to_owned()));
    }
    if !reply_complete(name, &response) {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Build status is unavailable".into(),
        ));
    }
    Ok(response["output"].clone())
}

fn job_not_found(value: &Value) -> Option<ApiError> {
    value["job"]
        .is_null()
        .then(|| ApiError(StatusCode::NOT_FOUND, "Build job not found".into()))
}

/// Jobs, tasks and source status.
pub async fn list(host: &dyn AdministrationHost) -> Result<Value, ApiError> {
    broker(host, "v3_build_dashboard", json!({})).await
}

/// Mark one retained-work notice reviewed.
pub async fn review_leftover(host: &dyn AdministrationHost, id: &str) -> Result<Value, ApiError> {
    if !valid_notice_id(id) {
        return Err(bad("Invalid build notice identifier"));
    }
    let value = broker(host, "v3_build_leftover_review", json!({"notice_id":id})).await?;
    if !reviewed(&value, id) {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Build notice changed; refresh before reviewing".into(),
        ));
    }
    Ok(value)
}

/// Most jobs of one task the Broker card page shows, newest first.
const TASK_JOBS: usize = 10;
/// The public Broker card page's reads are served from memory for this long, so anonymous
/// visits cost at most one broker feed read (and one task's progress reads) per interval.
const PUBLIC_CACHE: Duration = Duration::from_secs(10);
/// Cached entries kept before the cache is emptied.
const PUBLIC_CACHE_ENTRIES: usize = 256;
const FEED_KEY: &str = "";

static CACHE: LazyLock<Mutex<HashMap<String, (Instant, Value)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn cached(key: &str) -> Option<Value> {
    let cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    cache
        .get(key)
        .filter(|(at, _)| at.elapsed() < PUBLIC_CACHE)
        .map(|(_, value)| value.clone())
}

fn remember(key: &str, value: &Value) {
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if cache.len() >= PUBLIC_CACHE_ENTRIES {
        cache.clear();
    }
    cache.insert(key.to_owned(), (Instant::now(), value.clone()));
}

fn valid_task_id(value: &str) -> bool {
    (8..=80).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A job summary without its `[origin=...; session=...]` provenance, which names chats.
fn public_name(summary: &str) -> &str {
    summary.split(" [origin=").next().unwrap_or(summary).trim()
}

/// One broker task for the public Broker card page: its recent jobs (newest first) from the
/// dashboard feed, each with its stage snapshot, in the shape the chat's Broker card reads.
/// It is served without sign-in, so it carries only names, states and timings: no session
/// names, failure reasons, logs or paths. `queued_behind` names the tasks ahead in the queue.
pub async fn task(host: &dyn AdministrationHost, task_id: &str) -> Result<Value, ApiError> {
    if !valid_task_id(task_id) {
        return Err(bad("Invalid broker task identifier"));
    }
    if let Some(value) = cached(task_id) {
        return Ok(value);
    }
    let feed = match cached(FEED_KEY) {
        Some(feed) => feed,
        None => {
            let feed = broker(host, "v3_build_dashboard", json!({})).await?;
            remember(FEED_KEY, &feed);
            feed
        }
    };
    let feed_jobs = feed["jobs"].as_array().map_or(&[][..], Vec::as_slice);
    let rows: Vec<&Value> = feed_jobs
        .iter()
        .filter(|job| job["task_id"] == task_id)
        .take(TASK_JOBS)
        .collect();
    let Some(newest) = rows.first() else {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "No recent jobs for this broker task".into(),
        ));
    };
    let name_of = |id: &Value| {
        feed_jobs
            .iter()
            .find(|job| job["task_id"] == *id)
            .and_then(|job| job["summary"].as_str())
            .map(public_name)
    };
    let read_at = webagent_host_core::now();
    let mut jobs = Vec::new();
    for row in &rows {
        let job_id = row["job_id"].as_str().unwrap_or_default();
        // A job without a readable snapshot still lists with its status.
        let mut snapshot = broker(host, "v3_build_progress", json!({"job_id":job_id}))
            .await
            .unwrap_or(Value::Null);
        if let Some(ahead) = snapshot["queued_behind"].as_array_mut() {
            for other in ahead.iter_mut() {
                let name = name_of(&other["task_id"]);
                other["name"] = json!(name);
                if let Some(other) = other.as_object_mut() {
                    other.remove("session");
                }
            }
        }
        let kind = if row["operation"] == "check" {
            "check"
        } else {
            "build"
        };
        jobs.push(json!({"job_id":job_id,"kind":kind,"status":row["status"],
            "snapshot":snapshot,"snapshot_at":read_at}));
    }
    let name = public_name(newest["summary"].as_str().unwrap_or(task_id));
    let active = matches!(newest["status"].as_str(), Some("queued" | "running"));
    let stalled = active && jobs[0]["snapshot"]["idle_seconds"].as_u64().unwrap_or(0) > 900;
    let value = json!({"task":{"task_id":task_id,"name":name,"status":newest["status"],
        "warning":stalled.then_some("stalled"),"jobs":jobs},"now":read_at});
    remember(task_id, &value);
    Ok(value)
}

/// One job's result.
pub async fn result(host: &dyn AdministrationHost, job_id: &str) -> Result<Value, ApiError> {
    if !valid_job_id(job_id) {
        return Err(bad("Invalid build job identifier"));
    }
    let value = broker(host, "v3_build_result", json!({"job_id":job_id})).await?;
    job_not_found(&value).map_or(Ok(value), Err)
}

/// Cancel one job.
pub async fn cancel(
    host: &dyn AdministrationHost,
    job_id: &str,
    request: &CancelRequest,
) -> Result<Value, ApiError> {
    let arguments = request
        .arguments(job_id)
        .ok_or_else(|| bad("Invalid cancellation identifier"))?;
    let value = broker(host, "v3_build_cancel", arguments).await?;
    job_not_found(&value).map_or(Ok(value), Err)
}

/// One scheduler control.
pub async fn scheduler(
    host: &dyn AdministrationHost,
    request: &SchedulerRequest,
) -> Result<Value, ApiError> {
    let arguments = request
        .arguments()
        .ok_or_else(|| bad("Invalid scheduler request"))?;
    broker(host, "v3_build_scheduler", arguments).await
}
