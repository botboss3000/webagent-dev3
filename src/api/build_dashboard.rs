//! Read-only administrator projection of the protected build broker.
use super::*;

pub(crate) async fn list(State(app): State<App>) -> Api {
    broker(&app, "v3_build_dashboard", json!({})).await
}

pub(crate) async fn result(State(app): State<App>, Path(job_id): Path<String>) -> Api {
    if uuid::Uuid::parse_str(&job_id).is_err() {
        return Err(bad("Invalid build job identifier"));
    }
    let value=broker(&app,"v3_build_result",json!({"job_id":job_id})).await?;
    if value.0["job"].is_null() {
        return Err(ApiError(StatusCode::NOT_FOUND,"Build job not found".into()));
    }
    Ok(value)
}

async fn broker(app: &App, name: &str, arguments: Value) -> Api {
    const DASHBOARD_SESSION: &str = "00000000-0000-0000-0000-000000000000";
    let user_id = app.store.current_user_id();
    let trusted = json!({
        "user_id": user_id,
        "session_id": DASHBOARD_SESSION,
        "lease_until": now() + 30,
        "access_fingerprint": digest(&json!(["v3_build_dashboard", user_id])),
    });
    let response = tools::execute_trusted(
        name,
        &arguments,
        DASHBOARD_SESSION,
        std::path::Path::new("/run/v3/build/build.sock"),
        Some(trusted),
        CancellationToken::new(),
    ).await?;
    if response["status"] != "success"
        || (name=="v3_build_dashboard" && !response["output"]["jobs"].is_array())
        || (name=="v3_build_result" && response["output"].get("job").is_none()) {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Build status is unavailable".into(),
        ));
    }
    Ok(Json(response["output"].clone()))
}
