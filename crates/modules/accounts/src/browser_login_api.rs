//! Opt-in diagnostics browser sign-in; the host evaluates current Access authority.
use crate::{
    api::{Error, Host},
    BrowserLoginBridge, RecordCodec, Repository,
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
pub async fn login<C: RecordCodec, H: Host>(
    repo: &Repository<'_, C>,
    host: &H,
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
    if !bridge.authorized(
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        &body.caller_id,
        headers.contains_key("origin"),
    ) {
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
