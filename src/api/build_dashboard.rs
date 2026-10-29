//! Administrator build dashboard routes over the protected build broker. The
//! Administration module owns request checks and broker calls; the router enforces
//! administrator authentication and CSRF first.
use super::*;
use crate::tools::administrator::Host;
use webagent_administration::dashboard::{CancelRequest, SchedulerRequest};
use webagent_administration_host::dashboard;

pub(crate) async fn list(State(app): State<App>) -> Api {
    Ok(Json(dashboard::list(&Host(&app)).await?))
}

pub(crate) async fn review_leftover(State(app): State<App>, Path(id): Path<String>) -> Api {
    Ok(Json(dashboard::review_leftover(&Host(&app), &id).await?))
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
