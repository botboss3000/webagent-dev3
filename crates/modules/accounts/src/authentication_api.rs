//! Password sign-in and setup controllers; the host supplies live Access admission and composition.
use super::self_service::required;
use crate::{
    api::{Api, Error, Host},
    LoginAttempts, RecordCodec, Repository,
};
use axum::{
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
fn bad(message: &str) -> Error {
    Error::Public(StatusCode::BAD_REQUEST, message.into())
}
fn unauthorized() -> Error {
    Error::Public(StatusCode::UNAUTHORIZED, "Invalid login".into())
}
pub(crate) fn origin(headers: &HeaderMap, secure: bool) -> Result<(), Error> {
    if !crate::valid_origin(
        headers.get("origin").and_then(|v| v.to_str().ok()),
        headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        secure,
    ) {
        return Err(bad("Invalid request origin"));
    }
    Ok(())
}
pub async fn login<C: RecordCodec, H: Host>(
    repo: &Repository<'_, C>,
    host: &H,
    attempts: &LoginAttempts,
    headers: &HeaderMap,
    body: &Value,
    secure: bool,
    request: crate::AdmissionContext,
) -> Result<Response, Error> {
    let crate::AdmissionContext { now, client } = request;
    origin(headers, secure)?;
    let username = required(body, "username", 128)?;
    required(body, "password", 1024)?;
    if !attempts
        .admit(crate::Admission::Login, client, username, now)
        .await
    {
        return Err(Error::Public(
            StatusCode::TOO_MANY_REQUESTS,
            "Try signing in again in one minute".into(),
        ));
    }
    login_admitted(repo, host, attempts, headers, body, secure, request).await
}
// Registration has already passed its independent admission window.
pub(crate) async fn login_admitted<C: RecordCodec, H: Host>(
    repo: &Repository<'_, C>,
    host: &H,
    attempts: &LoginAttempts,
    headers: &HeaderMap,
    body: &Value,
    secure: bool,
    request: crate::AdmissionContext,
) -> Result<Response, Error> {
    let crate::AdmissionContext { now, client } = request;
    let username = required(body, "username", 128)?;
    let password = required(body, "password", 1024)?;
    let proof = repo
        .verify_credentials(username, password.into())
        .await
        .map_err(|error| {
            if error.is::<tokio::sync::TryAcquireError>() {
                Error::Public(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Password service busy; try again shortly".into(),
                )
            } else if error.is::<tokio::task::JoinError>() {
                bad("Sign in unavailable")
            } else {
                Error::Storage(error)
            }
        })?
        .ok_or_else(unauthorized)?;
    // Access validates its grant fields; the credential proof remains bound to the inspected generation.
    if host.account_view(proof.account_state()).is_err() {
        return Err(unauthorized());
    }
    repo.record_user_login(&proof.identity().user_id, now)
        .await?;
    attempts.successful_login(client, username).await;
    let user = proof.identity().username.clone();
    let user_agent = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("Unknown browser");
    let session = repo.issue_verified_session(proof, now, user_agent).await?;
    let mut response = Json(json!({"csrf_token":session.csrf,"user":user})).into_response();
    response.headers_mut().insert(
        "set-cookie",
        HeaderValue::from_str(&crate::session_cookie(
            &session.token,
            session.expires - now,
            secure,
        ))
        .map_err(|_| bad("Sign in unavailable"))?,
    );
    Ok(response)
}
pub async fn setup_status<C: RecordCodec>(repo: &Repository<'_, C>) -> Api {
    Ok(Json(
        json!({"initialized":repo.has_admin_account().await?||repo.legacy_admin_exists().await?}),
    ))
}
pub async fn setup_admin<C: RecordCodec, H: Host>(
    repo: &Repository<'_, C>,
    host: &H,
    attempts: &LoginAttempts,
    headers: &HeaderMap,
    body: &Value,
    secure: bool,
    request: crate::AdmissionContext,
) -> Result<Response, Error> {
    origin(headers, secure)?;
    let username = body["username"].as_str().unwrap_or("admin").trim();
    let username = if username.is_empty() {
        "admin"
    } else {
        username
    };
    if username.len() > 128 {
        return Err(bad("Invalid username"));
    }
    let password = required(body, "password", 1024)?;
    if password.len() < 6 {
        return Err(bad("Password must be at least 6 characters"));
    }
    let _guard = host.gate().lock().await;
    if repo.has_admin_account().await? || repo.legacy_admin_exists().await? {
        return Err(Error::Public(
            StatusCode::CONFLICT,
            "Admin already exists".into(),
        ));
    }
    let hash = crate::hash_password(password.into())
        .await
        .map_err(|error| crate::api::password_error(error, "Password hashing failed"))?;
    host.create_administrator(username, &hash, &uuid::Uuid::new_v4().to_string())
        .await?;
    login(
        repo,
        host,
        attempts,
        headers,
        &json!({"username":username,"password":password}),
        secure,
        request,
    )
    .await
}
pub async fn me<C: RecordCodec, H: Host>(repo: &Repository<'_, C>, host: &H, doc: &Value) -> Api {
    let user = doc["_user_id"].as_str().unwrap_or("admin");
    let profile = repo.user_profile(user).await?;
    let account = repo
        .account_state(user)
        .await?
        .ok_or_else(|| Error::Public(StatusCode::UNAUTHORIZED, "Sign in required".into()))?;
    let administrator = host
        .administrator_account(&account)
        .map_err(|_| bad("Account unavailable"))?;
    Ok(Json(
        json!({"user":account["username"],"user_id":user,"display_name":profile.as_ref().map(|p|&p["display_name"]),"is_admin":administrator,"role":account["role"],"status":account["status"],"tier_id":account["tier_id"],"csrf_token":doc["csrf"]}),
    ))
}
pub async fn logout<C: RecordCodec>(
    repo: &Repository<'_, C>,
    headers: &HeaderMap,
) -> Result<Response, Error> {
    let token = crate::session_token(headers);
    repo.logout(token).await?;
    let mut response = Json(json!({"ok":true})).into_response();
    response.headers_mut().insert(
        "set-cookie",
        HeaderValue::from_static("v3_session=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict"),
    );
    Ok(response)
}
