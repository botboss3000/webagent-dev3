//! Current-account operations; caller supplies the authenticated identity and shared gate.
use crate::api::{Api, Error, Host};
use crate::{RecordCodec, Repository};
use axum::{http::StatusCode, Json};
use serde_json::{json, Value};
fn bad(message: &str) -> Error {
    Error::Public(StatusCode::BAD_REQUEST, message.into())
}
fn sign_in() -> Error {
    Error::Public(StatusCode::UNAUTHORIZED, "Sign in required".into())
}
pub(crate) fn required<'a>(body: &'a Value, name: &str, max: usize) -> Result<&'a str, Error> {
    let value = body[name]
        .as_str()
        .ok_or_else(|| bad(&format!("{name} is required")))?;
    if value.is_empty() || value.len() > max {
        return Err(bad(&format!("{name} has invalid length")));
    }
    Ok(value)
}
pub async fn update_profile<C: RecordCodec>(
    repo: &Repository<'_, C>,
    doc: &Value,
    body: &Value,
) -> Api {
    if doc["_role"] == "guest" {
        return Err(Error::Public(
            StatusCode::FORBIDDEN,
            "Register an account to edit a profile".into(),
        ));
    }
    let display = required(body, "display_name", 128)?.trim();
    let username = required(body, "username", 128)?.trim();
    if display.is_empty() || display.chars().any(char::is_control) {
        return Err(bad("Invalid display name"));
    }
    if username.is_empty() || username.chars().any(char::is_control) {
        return Err(bad("Invalid username"));
    }
    let id = doc["_user_id"]
        .as_str()
        .ok_or_else(|| bad("Account unavailable"))?;
    Ok(Json(repo.update_user_profile(id, username, display).await?))
}
pub async fn session_policy<C: RecordCodec>(repo: &Repository<'_, C>, doc: &Value) -> Api {
    Ok(Json(
        json!({"days":repo.session_days(doc["_user_id"].as_str().unwrap_or("admin")).await?}),
    ))
}
pub async fn update_session_policy<C: RecordCodec>(
    repo: &Repository<'_, C>,
    doc: &Value,
    body: &Value,
) -> Api {
    if doc["_role"] == "guest" {
        return Err(Error::Public(
            StatusCode::FORBIDDEN,
            "Guest sign-in duration is fixed".into(),
        ));
    }
    let days = body["days"]
        .as_u64()
        .filter(|days| [1, 7, 30, 90].contains(days))
        .ok_or_else(|| bad("Invalid sign-in duration"))?;
    repo.set_session_days(doc["_user_id"].as_str().unwrap_or("admin"), days)
        .await?;
    Ok(Json(json!({"days":days})))
}
pub async fn list_auth_sessions<C: RecordCodec>(
    repo: &Repository<'_, C>,
    doc: &Value,
    now: u64,
) -> Api {
    let id = doc["_user_id"].as_str().unwrap_or("admin");
    let account = repo.account_state(id).await?.ok_or_else(sign_in)?;
    let key = doc["_auth_key"].as_str().unwrap_or("");
    let sessions=repo.auth_sessions(id,account["generation"].as_str().unwrap_or(""),now).await?.into_iter().map(|s|json!({"id":s["id"],"user_agent":s["user_agent"],"created_at":s["created_at"],"expires_at":s["expires_at"],"current":s["key"]==key})).collect::<Vec<_>>();
    Ok(Json(json!({"sessions":sessions})))
}
pub async fn revoke_auth_session<C: RecordCodec>(
    repo: &Repository<'_, C>,
    doc: &Value,
    id: &str,
    now: u64,
) -> Api {
    let user = doc["_user_id"].as_str().unwrap_or("admin");
    let account = repo.account_state(user).await?.ok_or_else(sign_in)?;
    let session = repo
        .auth_sessions(user, account["generation"].as_str().unwrap_or(""), now)
        .await?
        .into_iter()
        .find(|s| s["id"] == id)
        .ok_or_else(|| Error::Public(StatusCode::NOT_FOUND, "Sign-in session not found".into()))?;
    let current = session["key"] == doc["_auth_key"];
    repo.delete_auth_key(session["key"].as_str().unwrap_or(""))
        .await?;
    Ok(Json(json!({"ok":true,"current":current})))
}
pub async fn change_password<C: RecordCodec, H: Host>(
    repo: &Repository<'_, C>,
    host: &H,
    doc: &Value,
    body: &Value,
) -> Api {
    let current = required(body, "current_password", 1024)?.to_owned();
    let next = required(body, "new_password", 1024)?;
    if next.len() < 6 {
        return Err(bad("New password must be at least 6 characters"));
    }
    let id = doc["_user_id"].as_str().unwrap_or("admin");
    let account = repo.account_by_id(id).await?.ok_or_else(sign_in)?;
    let valid = crate::verify_password(
        account["password_hash"].as_str().unwrap_or("").into(),
        current,
    )
    .await
    .map_err(|error| crate::api::password_error(error, "Password verification unavailable"))?;
    if !valid {
        return Err(Error::Public(
            StatusCode::FORBIDDEN,
            "Current password is incorrect".into(),
        ));
    }
    let generation = account["generation"]
        .as_str()
        .ok_or_else(|| bad("Account unavailable"))?;
    let next_hash = crate::hash_password(next.into())
        .await
        .map_err(|error| crate::api::password_error(error, "Password hashing failed"))?;
    let _guard = host.gate().lock().await;
    repo.replace_user_password(
        id,
        generation,
        &next_hash,
        &uuid::Uuid::new_v4().to_string(),
    )
    .await
    .map_err(|_| {
        Error::Public(
            StatusCode::CONFLICT,
            "Account changed; sign in again".into(),
        )
    })?;
    Ok(Json(json!({"ok":true,"sign_in_required":true})))
}
