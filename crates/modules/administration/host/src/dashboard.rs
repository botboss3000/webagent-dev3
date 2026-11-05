//! The administrator build dashboard: a projection of the protected build broker and its
//! targeted controls. Administrator authentication and CSRF are enforced by the root's
//! router before these run.
use crate::{trusted, AdministrationHost};
use axum::http::StatusCode;
use serde_json::{json, Value};
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

fn valid_task_id(value: &str) -> bool {
    (8..=80).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// One broker task for the Broker card page: its recent jobs (newest first) from the
/// dashboard feed, each with its stage snapshot, in the shape the chat's Broker card
/// reads. The broker answers the dashboard's progress reads for any job.
pub async fn task(host: &dyn AdministrationHost, task_id: &str) -> Result<Value, ApiError> {
    if !valid_task_id(task_id) {
        return Err(bad("Invalid broker task identifier"));
    }
    let feed = broker(host, "v3_build_dashboard", json!({})).await?;
    let rows: Vec<&Value> = feed["jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|job| job["task_id"] == task_id)
        .take(TASK_JOBS)
        .collect();
    let Some(newest) = rows.first() else {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "No recent jobs for this broker task".into(),
        ));
    };
    let read_at = webagent_host_core::now();
    let mut jobs = Vec::new();
    for row in &rows {
        let job_id = row["job_id"].as_str().unwrap_or_default();
        // A job without a readable snapshot still lists with its status.
        let snapshot = broker(host, "v3_build_progress", json!({"job_id":job_id}))
            .await
            .unwrap_or(Value::Null);
        let kind = if row["operation"] == "check" {
            "check"
        } else {
            "build"
        };
        jobs.push(json!({"job_id":job_id,"kind":kind,"status":row["status"],
            "snapshot":snapshot,"snapshot_at":read_at}));
    }
    let summary = newest["summary"].as_str().unwrap_or(task_id);
    let name = summary.split(" [origin=").next().unwrap_or(summary).trim();
    let active = matches!(newest["status"].as_str(), Some("queued" | "running"));
    let stalled = active && jobs[0]["snapshot"]["idle_seconds"].as_u64().unwrap_or(0) > 900;
    Ok(
        json!({"task":{"task_id":task_id,"name":name,"status":newest["status"],
        "warning":stalled.then_some("stalled"),"jobs":jobs},"now":read_at}),
    )
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
