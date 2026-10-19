//! Browser reports are untrusted operational observations, never runtime authority.
use super::*;
use axum::{extract::DefaultBodyLimit, routing::post};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct IssueEvent {
    issue_id: String,
    event: String,
    operation: String,
    session_id: Option<String>,
    status: Option<u16>,
    repeat_count: u32,
    duration_ms: u64,
}
impl IssueEvent {
    fn validate(&self) -> Result<(), crate::ApiError> {
        let uuid = |s: &str| uuid::Uuid::parse_str(s).is_ok();
        if !uuid(&self.issue_id)
            || self.session_id.as_deref().is_some_and(|s| !uuid(s))
            || !matches!(self.event.as_str(), "issue_opened" | "issue_resolved")
            || !matches!(
                self.operation.as_str(),
                "usage"
                    | "shared_snapshot"
                    | "history_refresh"
                    | "message_statistics"
                    | "read_ack"
                    | "manager_status"
                    | "session_controls"
                    | "shared_controls"
                    | "chat_sync"
                    | "status_poll"
                    | "sessions_list"
                    | "activity_display"
                    | "media_display"
                    | "transcript_position"
            )
            || self.status.is_some_and(|s| !(100..=599).contains(&s))
            || self.repeat_count == 0
            || self.repeat_count > 1_000_000
            || self.duration_ms > 31_536_000_000
        {
            return Err(crate::bad("Invalid background issue event"));
        }
        Ok(())
    }
}
/// Exposes presentation policy only. Capture remains restricted to the fixed admin account.
async fn configuration(Extension(app): Extension<crate::App>) -> crate::Api {
    let s = settings();
    Ok(Json(json!({
        "enabled": scope(&app.store) && bridge::available() && s.as_ref().is_some_and(|s| permits(s,"app_background")),
        "show_toasts": s.filter(|s| s.error.is_none()).map(|s|s.config.background_issues.show_toasts).unwrap_or(true)
    })))
}
/// Bounds and allowlists client observations; never accepts raw messages, URLs, headers or credentials.
async fn report(
    Extension(app): Extension<crate::App>,
    Extension(identity): Extension<Value>,
    Json(body): Json<IssueEvent>,
) -> crate::Api {
    authorize(&app, &identity).await?;
    body.validate()?;
    let Some(s) = settings().filter(|s| bridge::available() && permits(s, "app_background")) else {
        return Ok(Json(json!({"accepted":false})));
    };
    // A missing/deleted session can itself be the failure. Do not invent verified ownership.
    let verified_session = if let Some(sid) = body.session_id.as_deref() {
        app.store.get(&format!("session:{sid}")).await?.is_some()
    } else {
        false
    };
    let event = json!({"owner":"admin","category":"app_background","level":if body.event=="issue_resolved"{"info"}else{"warning"},
        "source":"browser_observation","config_digest":s.digest,"issue_id":body.issue_id,"event":body.event,
        "operation":body.operation,"session_id":if verified_session {body.session_id.as_deref()}else{None},
        "client_session_id":body.session_id,"status":body.status,"repeat_count":body.repeat_count,"duration_ms":body.duration_ms});
    // Await the existing bounded recorder acknowledgement so retries cannot silently lose a queued event.
    bridge::send("event", event, None)
        .await
        .map_err(anyhow::Error::from)?;
    Ok(Json(json!({"accepted":true})))
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
