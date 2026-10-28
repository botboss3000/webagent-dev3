//! HTTP authority stays in the root; Diagnostics owns bounded event semantics.
#[cfg(test)]
use webagent_accounts_host::AccountsStore;
use super::*;
use axum::{extract::DefaultBodyLimit,routing::post};
use package::background::IssueEvent;
async fn configuration(Extension(app):Extension<crate::App>)->crate::Api {Ok(Json(package::background::configuration(RECORDER.get(),&app.store)))}
async fn report(Extension(app):Extension<crate::App>,Extension(identity):Extension<Value>,Json(body):Json<IssueEvent>)->crate::Api {
 authorize(&app,&identity).await?;
 Ok(Json(package::background::report(RECORDER.get(),&app.store,body).await.map_err(api_error)?))
}
pub(super) fn router() -> Router<crate::App> {
    Router::new()
        .route("/diagnostics/background/config", get(configuration))
        .route("/diagnostics/background/events", post(report))
        .layer(DefaultBodyLimit::max(2048))
        .layer(axum::middleware::map_response(no_store))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn event() -> IssueEvent {
        serde_json::from_value(json!({"issue_id":uuid::Uuid::new_v4().to_string(),"event":"issue_opened","operation":"usage","session_id":null,"status":404,"repeat_count":1,"duration_ms":0})).unwrap()
    }
    #[test]
    fn browser_observations_are_bounded_and_allowlisted() {
        let mut e = event();
        assert!(e.validate().is_ok());
        e.operation = "arbitrary secret".into();
        assert!(e.validate().is_err());
        let mut e = event();
        e.session_id = Some("not-a-session".into());
        assert!(e.validate().is_err());
        let mut e = event();
        e.status = Some(999);
        assert!(e.validate().is_err());
        let mut e = event();
        e.repeat_count = 0;
        assert!(e.validate().is_err());
        let mut e = event();
        e.event = "dismissed".into();
        assert!(e.validate().is_err());
        let mut e=serde_json::to_value(json!({"issue_id":uuid::Uuid::new_v4().to_string(),"event":"issue_opened","operation":"usage","repeat_count":1,"duration_ms":0})).unwrap();
        e["message"] = json!("must never be captured");
        assert!(serde_json::from_value::<IssueEvent>(e).is_err());
    }
    #[test]
    fn toast_visibility_does_not_disable_issue_capture() {
        let mut s = Settings {
            config: Config::disabled(),
            digest: Some("fixture".into()),
            error: None,
        };
        s.config.enabled = true;
        s.config.background_issues.show_toasts = false;
        assert!(permits(&s, "app_background"));
        s.config.enabled = false;
        assert!(!permits(&s, "app_background"));
        s.config.enabled = true;
        s.config.background_issues.enabled = false;
        assert!(!permits(&s, "app_background"));
    }

    #[tokio::test]
    async fn browser_reports_retain_auth_csrf_and_fixed_admin_scope() {
        use axum::{
            body::Body,
            http::{Request, StatusCode},
        };
        use tower::ServiceExt;
        let (_root, app) = crate::runtime_tests::fixture().await;
        let token = uuid::Uuid::new_v4().to_string();
        app.store.put(&format!("auth:{}",crate::digest(&json!(token))),
            &json!({"csrf":"fixture","expires":crate::now()+3600,"generation":"fixture-generation"})).await.unwrap();
        let cookie = format!("v3_session={token}");
        let request = |cookie: Option<&str>, csrf: Option<&str>, body: Value| {
            let mut r = Request::builder()
                .method("POST")
                .uri("/api/v3/diagnostics/background/events")
                .header("content-type", "application/json");
            if let Some(c) = cookie {
                r = r.header("cookie", c);
            }
            if let Some(c) = csrf {
                r = r.header("x-csrf-token", c);
            }
            r.body(Body::from(body.to_string())).unwrap()
        };
        let body = json!({"issue_id":uuid::Uuid::new_v4().to_string(),"event":"issue_opened","operation":"usage","repeat_count":1,"duration_ms":0});
        for (cookie, csrf, status) in [
            (None, None, StatusCode::UNAUTHORIZED),
            (Some(cookie.as_str()), None, StatusCode::FORBIDDEN),
            (Some(cookie.as_str()), Some("fixture"), StatusCode::OK),
        ] {
            assert_eq!(
                crate::api::router(app.clone())
                    .oneshot(request(cookie, csrf, body.clone()))
                    .await
                    .unwrap()
                    .status(),
                status
            );
        }
        let mut invalid = body.clone();
        invalid["operation"] = json!("not_allowlisted");
        assert_eq!(
            crate::api::router(app.clone())
                .oneshot(request(Some(&cookie), Some("fixture"), invalid))
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        let account = app
            .store
            .create_user_account("backgroundobserver", "fixture-only-password-hash")
            .await
            .unwrap();
        let uid = account["user_id"].as_str().unwrap();
        app.store
            .update_account_access(
                uid,
                1,
                crate::access::AccountStatus::Active,
                crate::access::Role::Administrator,
                "basic",
            )
            .await
            .unwrap();
        let foreign = app.store.account_by_id(uid).await.unwrap().unwrap();
        let other = uuid::Uuid::new_v4().to_string();
        app.store.put(&format!("auth:{}",crate::digest(&json!(other))),
            &json!({"user_id":uid,"csrf":"fixture","expires":crate::now()+3600,"generation":foreign["generation"]})).await.unwrap();
        assert_eq!(
            crate::api::router(app.clone())
                .oneshot(request(
                    Some(&format!("v3_session={other}")),
                    Some("fixture"),
                    body
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
}
