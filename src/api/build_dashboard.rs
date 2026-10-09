//! Administrator projection and targeted cancellation of the protected build broker.
use super::*;

pub(crate) async fn list(State(app): State<App>) -> Api {
    broker(&app, "v3_build_dashboard", json!({})).await
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
    )
    .await?;
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
}
