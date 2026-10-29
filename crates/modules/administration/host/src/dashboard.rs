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
