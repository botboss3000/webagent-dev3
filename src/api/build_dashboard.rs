//! Administrator build dashboard routes over the protected build broker. The
//! Administration module owns request checks and broker calls; the router enforces
//! administrator authentication and CSRF first.
use super::*;
use crate::tools::administrator::Host;
use webagent_administration::dashboard::{CancelRequest, SchedulerRequest};
use webagent_administration_host::dashboard;

/// The administrator Builds page routes; the caller adds administrator authentication.
pub(super) fn admin_router() -> Router<App> {
    Router::new()
        .route("/admin/builds", get(list))
        .route("/admin/builds/scheduler", post(scheduler))
        .route("/admin/builds/{job_id}/result", get(result))
        .route("/admin/builds/{job_id}/cancel", post(cancel))
        .route("/admin/builds/leftovers/{id}/review", post(review_leftover))
        .route("/admin/production", get(production))
        .route(
            "/admin/production/{release_id}/decision",
            post(production_decide),
        )
        // The Builds page reads one task; task labels are guessable, so it needs an
        // administrator sign-in like the rest of the Builds data.
        .route("/builds/tasks/{task_id}", get(task))
}

pub(crate) async fn list(State(app): State<App>) -> Api {
    Ok(Json(dashboard::list(&Host(&app)).await?))
}

pub(crate) async fn review_leftover(State(app): State<App>, Path(id): Path<String>) -> Api {
    Ok(Json(dashboard::review_leftover(&Host(&app), &id).await?))
}

pub(crate) async fn task(State(app): State<App>, Path(task_id): Path<String>) -> Api {
    Ok(Json(dashboard::task(&Host(&app), &task_id).await?))
}

pub(crate) async fn result(State(app): State<App>, Path(job_id): Path<String>) -> Api {
    Ok(Json(dashboard::result(&Host(&app), &job_id).await?))
}

pub(crate) async fn cancel(
    State(app): State<App>,
    Path(job_id): Path<String>,
    Json(request): Json<CancelRequest>,
) -> Api {
    Ok(Json(
        dashboard::cancel(&Host(&app), &job_id, &request).await?,
    ))
}

pub(crate) async fn production(State(app): State<App>) -> Api {
    Ok(Json(dashboard::production(&Host(&app)).await?))
}

/// A production release decision from the Host Admin page.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProductionDecision {
    decision: String,
}

pub(crate) async fn production_decide(
    State(app): State<App>,
    Path(release_id): Path<String>,
    Json(request): Json<ProductionDecision>,
) -> Api {
    Ok(Json(
        dashboard::production_decide(&Host(&app), &release_id, &request.decision).await?,
    ))
}

pub(crate) async fn scheduler(
    State(app): State<App>,
    Json(request): Json<SchedulerRequest>,
) -> Api {
    Ok(Json(dashboard::scheduler(&Host(&app), &request).await?))
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
            .update_account_access(uid, 1, access::AccountStatus::Active, "free")
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

    /// The Builds page's task read needs an administrator sign-in: an anonymous read is
    /// refused before any broker call, and a signed-in administrator's malformed ID is refused.
    #[tokio::test]
    async fn task_route_requires_administrator_and_checks_the_task_id() {
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
        let status = |path: &str, cookie: bool| {
            let app = app.clone();
            let mut builder = Request::builder().method("GET").uri(path);
            if cookie {
                builder = builder.header("cookie", format!("v3_session={token}"));
            }
            let request = builder.body(Body::empty()).unwrap();
            async move {
                crate::api::router(app)
                    .oneshot(request)
                    .await
                    .unwrap()
                    .status()
            }
        };
        let path = "/api/v3/builds/tasks/broker-card-task";
        assert_eq!(status(path, false).await, StatusCode::UNAUTHORIZED);
        assert_eq!(
            status("/api/v3/builds/tasks/bad", true).await,
            StatusCode::BAD_REQUEST
        );
        // The fixture has no broker: a valid ID reaches it instead of an auth refusal.
        let reached = status(path, true).await;
        assert!(![StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN].contains(&reached));
    }

    /// Scheduler controls require authenticated administrators and CSRF before any broker effect.
    #[tokio::test]
    async fn scheduler_route_requires_auth_csrf_and_admin() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let token = uuid();
        let auth_key = format!("auth:{}", digest(&json!(token)));
        app.store
            .put(
                &auth_key,
                &json!({
                    "csrf":"expected","expires":now()+3600,"generation":"fixture-generation"
                }),
            )
            .await
            .unwrap();
        let request = |cookie: bool, csrf: &str, body: Value| {
            let mut builder = Request::builder()
                .method("POST")
                .uri("/api/v3/admin/builds/scheduler")
                .header("content-type", "application/json")
                .header("x-csrf-token", csrf);
            if cookie {
                builder = builder.header("cookie", format!("v3_session={token}"));
            }
            builder.body(Body::from(body.to_string())).unwrap()
        };
        let read = json!({"action":"reservations"});
        assert_eq!(
            crate::api::router(app.clone())
                .oneshot(request(false, "expected", read.clone()))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            crate::api::router(app.clone())
                .oneshot(request(true, "wrong", read.clone()))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            crate::api::router(app.clone())
                .oneshot(request(
                    true,
                    "expected",
                    json!({
                        "action":"priority","priority":3
                    })
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert!(
            serde_json::from_value::<webagent_administration::dashboard::SchedulerRequest>(json!({
                "action":"reservations","untrusted_owner":"admin"
            }))
            .is_err()
        );
        let user = app
            .store
            .create_user_account("scheduler_user", "fixture-only-password-hash")
            .await
            .unwrap();
        let uid = user["user_id"].as_str().unwrap();
        app.store
            .update_account_access(uid, 1, access::AccountStatus::Active, "free")
            .await
            .unwrap();
        let generation = app.store.account_by_id(uid).await.unwrap().unwrap()["generation"].clone();
        app.store
            .put(
                &auth_key,
                &json!({
                    "user_id":uid,"csrf":"expected","expires":now()+3600,"generation":generation
                }),
            )
            .await
            .unwrap();
        assert_eq!(
            crate::api::router(app)
                .oneshot(request(true, "expected", read))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
}
