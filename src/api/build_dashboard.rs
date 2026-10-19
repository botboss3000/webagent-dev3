//! Administrator projection and targeted cancellation of the protected build broker.
use super::*;

pub(crate) async fn list(State(app): State<App>) -> Api {
    broker(&app, "v3_build_dashboard", json!({})).await
}


pub(crate) async fn review_leftover(State(app): State<App>, Path(id): Path<String>) -> Api {
    if id.len() != 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(bad("Invalid build notice identifier"));
    }
    let value = broker(&app, "v3_build_leftover_review", json!({"notice_id":id})).await?;
    if value.0["status"] != "reviewed" || value.0["notice_id"] != id {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Build notice changed; refresh before reviewing".into(),
        ));
    }
    Ok(value)
}

pub(crate) async fn result(State(app): State<App>, Path(job_id): Path<String>) -> Api {
    if uuid::Uuid::parse_str(&job_id).is_err() {
        return Err(bad("Invalid build job identifier"));
    }
    let value = broker(&app, "v3_build_result", json!({"job_id":job_id})).await?;
    if value.0["job"].is_null() {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "Build job not found".into(),
        ));
    }
    Ok(value)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CancelRequest {
    request_id: String,
    #[serde(default)]
    expected_job_ids: Vec<String>,
}

pub(crate) async fn cancel(
    State(app): State<App>,
    Path(job_id): Path<String>,
    Json(request): Json<CancelRequest>,
) -> Api {
    if uuid::Uuid::parse_str(&job_id).is_err()
        || request.expected_job_ids.is_empty()
        || request.expected_job_ids.len() > 8
        || request
            .expected_job_ids
            .iter()
            .any(|id| uuid::Uuid::parse_str(id).is_err())
        || request.request_id.is_empty()
        || request.request_id.len() > 80
        || !request
            .request_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err(bad("Invalid cancellation identifier"));
    }
    let value = broker(
        &app,
        "v3_build_cancel",
        json!({"job_id": job_id, "request_id": request.request_id, "expected_job_ids": request.expected_job_ids}),
    )
    .await?;
    if value.0["job"].is_null() {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "Build job not found".into(),
        ));
    }
    Ok(value)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SchedulerRequest {
    action: String,
    task_id: Option<String>,
    task_key: Option<String>,
    summary: Option<String>,
    job_id: Option<String>,
    request_id: Option<String>,
    expected_revision: Option<u64>,
    expected_epoch: Option<u64>,
    phase_id: Option<String>,
    priority: Option<i8>,
    offset: Option<u64>,
    limit: Option<u64>,
    paths: Option<Vec<String>>,
    target_task_id: Option<String>,
    reason: Option<String>,
    commit: Option<String>,
    acceptance: Option<Vec<Value>>,
}

/// The existing administrator/CSRF middleware protects all scheduler controls.
pub(crate) async fn scheduler(State(app): State<App>, Json(request): Json<SchedulerRequest>) -> Api {
    if !matches!(request.action.as_str(), "reservations" | "create" | "reserve" | "heartbeat" | "continue"
        | "acceptance" | "complete" | "handoff" | "takeover_preview" | "takeover" | "recover"
        | "resources" | "pause" | "resume" | "priority" | "reconcile")
        || request.paths.as_ref().is_some_and(|p| p.is_empty() || p.len() > 128 || p.iter().any(|s| s.len() > 240))
        || request.reason.as_ref().is_some_and(|s| s.is_empty() || s.len() > 500)
        || request.summary.as_ref().is_some_and(|s| s.len() > 200)
        || request.acceptance.as_ref().is_some_and(|a| a.is_empty() || a.len() > 32)
        || request.priority.is_some_and(|p| !(-2..=2).contains(&p))
        || request.offset.is_some_and(|o| o > 1000)
        || request.limit.is_some_and(|n| n == 0 || n > 50)
    { return Err(bad("Invalid scheduler request")); }
    let mut args = json!({"action":request.action});
    for (key, value) in [
        ("task_id", json!(request.task_id)), ("task_key", json!(request.task_key)), ("summary", json!(request.summary)), ("job_id", json!(request.job_id)),
        ("request_id", json!(request.request_id)), ("expected_revision", json!(request.expected_revision)),
        ("expected_epoch", json!(request.expected_epoch)), ("phase_id", json!(request.phase_id)),
        ("offset", json!(request.offset)), ("limit", json!(request.limit)),
        ("priority", json!(request.priority)), ("paths", json!(request.paths)), ("target_task_id", json!(request.target_task_id)),
        ("reason", json!(request.reason)), ("commit", json!(request.commit)), ("acceptance", json!(request.acceptance)),
    ] { if !value.is_null() { args[key] = value; } }
    broker(&app, "v3_build_scheduler", args).await
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
    let response = tools::execute_host_trusted(
        name,
        &arguments,
        DASHBOARD_SESSION,
        &crate::tools::administrator::build_socket(&app),
        Some(trusted),
        CancellationToken::new(),
    )
    .await?;
    if name == "v3_build_scheduler" && response["status"] != "success" {
        return Err(ApiError(StatusCode::CONFLICT,
            response["output"].as_str().unwrap_or("Scheduler conflict; refresh ownership and job status").to_owned()));
    }
    if response["status"] != "success"
        || (name == "v3_build_dashboard" && !response["output"]["jobs"].is_array())
        || (matches!(name, "v3_build_result" | "v3_build_cancel")
            && response["output"].get("job").is_none())
    {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Build status is unavailable".into(),
        ));
    }
    Ok(Json(response["output"].clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;
    #[tokio::test]
    async fn cancellation_route_requires_auth_csrf_and_admin() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let token = uuid();
        let auth_key = format!("auth:{}", digest(&json!(token)));
        app.store
            .put(
                &auth_key,
                &json!({"csrf":"expected","expires":now()+3600,"generation":"fixture-generation"}),
            )
            .await
            .unwrap();
        let job_id = uuid();
        let path = format!("/api/v3/admin/builds/{job_id}/cancel");
        let request = |cookie: bool, csrf: &str| {
            let mut builder = Request::builder()
                .method("POST")
                .uri(&path)
                .header("content-type", "application/json")
                .header("x-csrf-token", csrf);
            if cookie {
                builder = builder.header("cookie", format!("v3_session={token}"));
            }
            builder
                .body(Body::from(
                    json!({"request_id":"test-cancel","expected_job_ids":[job_id]}).to_string(),
                ))
                .unwrap()
        };
        assert_eq!(
            crate::api::router(app.clone())
                .oneshot(request(false, "expected"))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            crate::api::router(app.clone())
                .oneshot(request(true, "wrong"))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        // Validation fails before contacting any host broker.
        let invalid = Request::builder()
            .method("POST")
            .uri("/api/v3/admin/builds/invalid/cancel")
            .header("cookie", format!("v3_session={token}"))
            .header("x-csrf-token", "expected")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"request_id":"test-cancel"}"#))
            .unwrap();
        assert_eq!(
            crate::api::router(app.clone())
                .oneshot(invalid)
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        let user = app
            .store
            .create_user_account("cancel_user", "fixture-only-password-hash")
            .await
            .unwrap();
        let uid = user["user_id"].as_str().unwrap();
        app.store
            .update_account_access(
                uid,
                1,
                access::AccountStatus::Active,
                access::Role::User,
                "basic",
            )
            .await
            .unwrap();
        let generation = app.store.account_by_id(uid).await.unwrap().unwrap()["generation"].clone();
        app.store.put(&auth_key, &json!({"user_id":uid,"csrf":"expected","expires":now()+3600,"generation":generation})).await.unwrap();
        assert_eq!(
            crate::api::router(app)
                .oneshot(request(true, "expected"))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }

    /// Scheduler controls require authenticated administrators and CSRF before any broker effect.
    #[tokio::test]
    async fn scheduler_route_requires_auth_csrf_and_admin() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let token = uuid();
        let auth_key = format!("auth:{}", digest(&json!(token)));
        app.store.put(&auth_key, &json!({
            "csrf":"expected","expires":now()+3600,"generation":"fixture-generation"
        })).await.unwrap();
        let request = |cookie: bool, csrf: &str, body: Value| {
            let mut builder = Request::builder().method("POST")
                .uri("/api/v3/admin/builds/scheduler")
                .header("content-type", "application/json").header("x-csrf-token", csrf);
            if cookie { builder = builder.header("cookie", format!("v3_session={token}")); }
            builder.body(Body::from(body.to_string())).unwrap()
        };
        let read = json!({"action":"reservations"});
        assert_eq!(crate::api::router(app.clone()).oneshot(request(false,"expected",read.clone()))
            .await.unwrap().status(), StatusCode::UNAUTHORIZED);
        assert_eq!(crate::api::router(app.clone()).oneshot(request(true,"wrong",read.clone()))
            .await.unwrap().status(), StatusCode::FORBIDDEN);
        assert_eq!(crate::api::router(app.clone()).oneshot(request(true,"expected",json!({
            "action":"priority","priority":3
        }))).await.unwrap().status(), StatusCode::BAD_REQUEST);
        assert!(serde_json::from_value::<SchedulerRequest>(json!({
            "action":"reservations","untrusted_owner":"admin"
        })).is_err());
        let user = app.store.create_user_account("scheduler_user","fixture-only-password-hash")
            .await.unwrap();
        let uid = user["user_id"].as_str().unwrap();
        app.store.update_account_access(uid,1,access::AccountStatus::Active,access::Role::User,"basic")
            .await.unwrap();
        let generation = app.store.account_by_id(uid).await.unwrap().unwrap()["generation"].clone();
        app.store.put(&auth_key,&json!({
            "user_id":uid,"csrf":"expected","expires":now()+3600,"generation":generation
        })).await.unwrap();
        assert_eq!(crate::api::router(app).oneshot(request(true,"expected",read))
            .await.unwrap().status(), StatusCode::FORBIDDEN);
    }
}
