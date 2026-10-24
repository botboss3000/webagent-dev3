//! Registration and persistent guest sign-in use the existing shared policy document.
use super::{
    authentication_api::{login, origin},
    self_service::required,
};
use crate::{
    api::{Api, Error, Host},
    LoginAttempts, RecordCodec, RegistrationMode, Repository,
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
fn conflict(message: &str) -> Error {
    Error::Public(StatusCode::CONFLICT, message.into())
}
fn policy_mode<H: Host>(host: &H) -> Result<RegistrationMode, Error> {
    Ok(RegistrationMode::from_policy(&host.registration_policy()?)?)
}
pub async fn access_mode<H: Host>(host: &H) -> Api {
    let policy = host.registration_policy()?;
    let open = RegistrationMode::from_policy(&policy)? == RegistrationMode::Open;
    Ok(Json(
        json!({"access_mode":if open{"public_registered"}else{"admin_approval"},"guest_access":open,"revision":policy["revision"].as_u64().unwrap_or(0)}),
    ))
}
pub async fn set_mode<H: Host>(host: &H, identity: &Value, body: &Value) -> Api {
    host.administrator(identity).await?;
    let mode = required(body, "access_mode", 32)?;
    if !matches!(mode, "admin_approval" | "public_registered") {
        return Err(bad("Invalid access mode"));
    }
    let _guard = host.gate().lock().await;
    let expected = body["expected_revision"]
        .as_u64()
        .ok_or_else(|| bad("Expected policy revision required"))?;
    host.set_registration_policy(
        if mode == "public_registered" {
            "open"
        } else {
            "restricted"
        },
        Some(expected),
    )
    .map_err(|_| conflict("Policy save rejected; reload the current policy"))?;
    Ok(Json(
        json!({"access_mode":mode,"guest_access":mode=="public_registered"}),
    ))
}
pub async fn register<C: RecordCodec, H: Host>(
    repo: &Repository<'_, C>,
    host: &H,
    attempts: &LoginAttempts,
    headers: &HeaderMap,
    body: &Value,
    secure: bool,
    now: u64,
) -> Result<Response, Error> {
    origin(headers, secure)?;
    if !repo.has_admin_account().await? {
        return Err(conflict("Complete administrator setup first"));
    }
    let username = required(body, "username", 128)?.to_owned();
    let password = required(body, "password", 1024)?.to_owned();
    let display = body["display_name"]
        .as_str()
        .unwrap_or(&username)
        .to_owned();
    if password.len() < 8
        || username.trim() != username
        || username.chars().any(char::is_control)
        || display.trim() != display
        || display.is_empty()
        || display.len() > 128
        || display.chars().any(char::is_control)
    {
        return Err(bad(
            "Invalid registration input; password must be at least 8 characters",
        ));
    }
    if !attempts.admit(now).await {
        return Err(Error::Public(
            StatusCode::TOO_MANY_REQUESTS,
            "Try again in one minute".into(),
        ));
    }
    let hash = crate::hash_password(password.clone())
        .await
        .map_err(|error| {
            if error.is::<tokio::task::JoinError>() {
                bad("Registration unavailable")
            } else {
                bad("Password hashing failed")
            }
        })?;
    let guard = host.gate().lock().await;
    let public = policy_mode(host)? == RegistrationMode::Open;
    let created = repo
        .create_user_account(&username, &hash, host.initial_user_access())
        .await
        .map_err(|_| conflict("Account creation rejected; username may already be registered"))?;
    let id = created["user_id"]
        .as_str()
        .ok_or_else(|| bad("Account unavailable"))?;
    repo.update_user_profile(id, &username, &display).await?;
    if !public {
        return Ok(Json(json!({"user_id":id,"username":username,"display_name":display,"pending_approval":true})).into_response());
    }
    host.activate_registered_user(id).await?;
    drop(guard);
    let mut response = login(
        repo,
        host,
        attempts,
        headers,
        &json!({"username":username,"password":password}),
        secure,
        now,
    )
    .await?;
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    Ok(response)
}
pub async fn guest_login<C: RecordCodec, H: Host>(
    repo: &Repository<'_, C>,
    host: &H,
    attempts: &LoginAttempts,
    headers: &HeaderMap,
    secure: bool,
    now: u64,
) -> Result<Response, Error> {
    origin(headers, secure)?;
    let _guard = host.gate().lock().await;
    if policy_mode(host)? != RegistrationMode::Open {
        return Err(Error::Public(
            StatusCode::FORBIDDEN,
            "Guest access is not enabled".into(),
        ));
    }
    if !repo.has_admin_account().await? {
        return Err(conflict("Complete administrator setup first"));
    }
    let old_token = headers
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .find_map(|v| v.trim().strip_prefix("v3_session="))
        .unwrap_or("");
    let mut recovered = None;
    if old_token.len() == 36 {
        if let Some(doc) = repo.auth_document(old_token).await? {
            if doc["expires"].as_u64().unwrap_or(0) > now {
                if let Some(id) = doc["user_id"].as_str() {
                    if let Some(account) = repo.account_by_id(id).await? {
                        if account["role"] != "guest" {
                            return Err(conflict("Sign out before using guest access"));
                        }
                        if account["user_id"] == id
                            && account["generation"] == doc["generation"]
                            && account["status"] == "active"
                            && account["is_approved"] == true
                        {
                            recovered = Some(account);
                        }
                    }
                }
            }
        }
    }
    let account = if let Some(account) = recovered {
        account
    } else {
        if !attempts.admit(now).await {
            return Err(Error::Public(
                StatusCode::TOO_MANY_REQUESTS,
                "Try again in one minute".into(),
            ));
        }
        repo.create_guest_account(host.initial_guest_access())
            .await?
    };
    let identity = crate::identity(&account)?;
    let generation = account["generation"]
        .as_str()
        .ok_or_else(|| bad("Account unavailable"))?;
    let duration = 30 * 86400;
    let session = repo
        .issue_versioned_session(&identity, generation, now, "Guest browser", duration)
        .await?;
    if old_token.len() == 36 {
        repo.logout(old_token).await?;
    }
    let mut response =
        Json(json!({"csrf_token":session.csrf,"user_id":identity.user_id,"is_guest":true}))
            .into_response();
    response.headers_mut().insert(
        "set-cookie",
        HeaderValue::from_str(&crate::session_cookie(&session.token, duration, secure))
            .map_err(|_| bad("Guest session unavailable"))?,
    );
    Ok(response)
}
