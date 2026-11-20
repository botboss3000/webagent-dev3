//! Opt-in diagnostics browser sign-in; the host evaluates current Access authority.
use crate::{
    api::{Error, Host},
    BrowserLoginBridge, LoginAttempts, RecordCodec, Repository,
};
use axum::{
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    pub caller_id: String,
}
/// Wrong bridge tokens from every caller together share one per-minute cap.
const FAILURES: &str = "browser-login:failures";

#[allow(clippy::too_many_arguments)]
pub async fn login<C: RecordCodec, H: Host>(
    repo: &Repository<'_, C>,
    host: &H,
    attempts: &LoginAttempts,
    bridge: Option<&BrowserLoginBridge>,
    headers: &HeaderMap,
    body: LoginRequest,
    secure: bool,
    now: u64,
) -> Result<Response, Error> {
    let denied = || {
        Error::Public(
            StatusCode::UNAUTHORIZED,
            "Diagnostics browser login unavailable or unauthorized".into(),
        )
    };
    let bridge = bridge.ok_or_else(denied)?;
    let cap = crate::RequestLimits::from_policy(&host.registration_policy()?)?
        .browser_login_failures_per_minute;
    if attempts.retry_after(FAILURES, cap, now).await > 0 {
        return Err(Error::Public(
            StatusCode::TOO_MANY_REQUESTS,
            "Try again in one minute".into(),
        ));
    }
    // The bearer is compared as a SHA-256 digest in constant time.
    if !bridge.authorized(
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        &body.caller_id,
        headers.contains_key("origin"),
    ) {
        attempts.hit(FAILURES.into(), cap, 60, now).await;
        return Err(denied());
    }
    let _guard = host.gate().lock().await;
    let account = repo.account_state("admin").await?.ok_or_else(denied)?;
    if !host.administrator_account(&account).unwrap_or(false) {
        return Err(denied());
    }
    let generation = account["generation"].as_str().ok_or_else(denied)?;
    let identity = crate::identity(&account).map_err(|_| denied())?;
    if identity.user_id != "admin" {
        return Err(denied());
    }
    let duration = 600;
    let session = repo
        .issue_versioned_session(
            &identity,
            generation,
            now,
            "Diagnostics v3 development browser",
            duration,
        )
        .await?;
    let mut response =
        Json(json!({"user_id":"admin","user":identity.username,"expires_in":duration}))
            .into_response();
    response.headers_mut().insert(
        "set-cookie",
        HeaderValue::from_str(&crate::session_cookie(&session.token, duration, secure))
            .map_err(|_| denied())?,
    );
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    Ok(response)
}
