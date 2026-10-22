//! Administrator-only metrics routes; the parent router owns authentication and authorization.
use super::*;

/// Collect bounded proc counters on the blocking pool instead of the HTTP executor.
async fn summary() -> Api {
    let sample = tokio::task::spawn_blocking(webagent_system::snapshot)
        .await
        .map_err(anyhow::Error::from)?;
    Ok(Json(sample))
}

/// Select only the fixed counter groups; requests cannot supply filesystem paths.
async fn metric(Path(name): Path<String>) -> Api {
    if !["cpu", "memory", "disks", "network", "uptime"].contains(&name.as_str()) {
        return Err(bad("Unknown system metric"));
    }
    let sample = tokio::task::spawn_blocking(webagent_system::snapshot)
        .await
        .map_err(anyhow::Error::from)?;
    let value =
        webagent_system::select(sample, &name).ok_or_else(|| bad("System metrics unavailable"))?;
    Ok(Json(value))
}

/// Mount beneath the existing administrator authentication boundary.
pub(crate) fn router() -> Router<App> {
    Router::new()
        .route("/summary", get(summary))
        .route("/{metric}", get(metric))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn system_routes_require_admin_and_return_each_counter_group() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let token = crate::uuid();
        let key = format!("auth:{}", crate::digest(&json!(token)));
        app.store
            .put(
                &key,
                &json!({"csrf":"fixture","expires":crate::now()+3600,
            "generation":"fixture-generation"}),
            )
            .await
            .unwrap();
        let request = |path: &str, token: Option<&str>, method: &str| {
            let mut builder = Request::builder()
                .uri(format!("/api/v3/admin/system/{path}"))
                .method(method);
            if let Some(token) = token {
                builder = builder.header("cookie", format!("v3_session={token}"));
            }
            builder.body(Body::empty()).unwrap()
        };
        for name in ["summary", "cpu", "memory", "disks", "network", "uptime"] {
            let denied = crate::router(app.clone())
                .oneshot(request(name, None, "GET"))
                .await
                .unwrap();
            assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
            let response = crate::router(app.clone())
                .oneshot(request(name, Some(&token), "GET"))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{name}");
            let body = axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap();
            let value: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["schema_version"], 1);
            assert_eq!(value["scope"]["cgroup_limits_included"], false);
            assert_eq!(
                value["metrics"].as_object().unwrap().len(),
                if name == "summary" { 5 } else { 1 }
            );
            if name != "summary" {
                assert!(value["metrics"][name].is_object());
            }
        }
        let unknown = crate::router(app.clone())
            .oneshot(request("processes", Some(&token), "GET"))
            .await
            .unwrap();
        assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
        let post = crate::router(app.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/v3/admin/system/summary")
                    .method("POST")
                    .header("cookie", format!("v3_session={token}"))
                    .header("x-csrf-token", "fixture")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post.status(), StatusCode::METHOD_NOT_ALLOWED);
        let member = app
            .store
            .create_user_account("system-metrics-member", "fixture-only-password-hash")
            .await
            .unwrap();
        let user_id = member["user_id"].as_str().unwrap();
        app.store
            .update_account_access(
                user_id,
                1,
                crate::access::AccountStatus::Active,
                crate::access::Role::User,
                "basic",
            )
            .await
            .unwrap();
        let account = app.store.account_by_id(user_id).await.unwrap().unwrap();
        app.store
            .put(
                &key,
                &json!({"csrf":"fixture","expires":crate::now()+3600,
            "generation":account["generation"],"user_id":user_id}),
            )
            .await
            .unwrap();
        let denied = crate::router(app.clone())
            .oneshot(request("summary", Some(&token), "GET"))
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    }
}
