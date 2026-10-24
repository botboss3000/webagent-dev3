//! Accounts endpoints use borrowed records and explicit host authorization/storage adapters.
use crate::{RecordCodec, Repository};
use axum::{http::StatusCode, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use std::future::Future;
use tokio::sync::Mutex;
#[derive(Debug)]
pub enum Error {
    Public(StatusCode, String),
    Storage(anyhow::Error),
}
impl From<anyhow::Error> for Error {
    fn from(error: anyhow::Error) -> Self {
        Self::Storage(error)
    }
}
pub type Result<T> = std::result::Result<T, Error>;
pub type Api = Result<Json<Value>>;
fn invalid(message: &str) -> Error {
    Error::Public(StatusCode::BAD_REQUEST, message.into())
}
fn stale(message: &str) -> Error {
    Error::Public(StatusCode::CONFLICT, message.into())
}
/// Access authorizes; Storage supplies already scoped tenant counts. No Accounts SQL is delegated.
pub trait Host: Sync {
    fn initial_user_access(&self) -> crate::InitialAccess<'_>;
    fn administrator<'a>(
        &'a self,
        identity: &'a Value,
    ) -> impl Future<Output = Result<()>> + Send + 'a;
    fn account_view(&self, record: &Value) -> Result<Value>;
    fn user_counts<'a>(
        &'a self,
        user: &'a str,
    ) -> impl Future<Output = Result<(i64, i64)>> + Send + 'a;

    fn initial_guest_access(&self) -> crate::InitialAccess<'_>;
    fn administrator_account(&self, record: &Value) -> Result<bool>;
    fn registration_policy(&self) -> Result<Value>;
    fn set_registration_policy(&self, registration: &str, expected: Option<u64>) -> Result<()>;
    fn create_administrator<'a>(
        &'a self,
        username: &'a str,
        hash: &'a str,
        generation: &'a str,
    ) -> impl Future<Output = Result<()>> + Send + 'a;
    fn activate_registered_user<'a>(
        &'a self,
        id: &'a str,
    ) -> impl Future<Output = Result<()>> + Send + 'a;
    fn gate(&self) -> &Mutex<()>;
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewAccount {
    pub username: String,
    pub password: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileChange {
    pub username: String,
    pub display_name: String,
}
pub async fn accounts<C: RecordCodec, H: Host>(
    repo: &Repository<'_, C>,
    host: &H,
    identity: &Value,
) -> Api {
    host.administrator(identity).await?;
    let mut accounts = Vec::new();
    for record in repo.list_account_states().await? {
        let mut account = host.account_view(&record)?;
        let id = record["user_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Account has no stable identity"))?;
        account["profile"] = repo.user_profile(id).await?.unwrap_or_else(|| json!({}));
        let (sessions, messages) = host.user_counts(id).await?;
        account["session_count"] = json!(sessions);
        account["message_count"] = json!(messages);
        accounts.push(account);
    }
    Ok(Json(json!({"accounts":accounts})))
}
pub async fn create_account<C: RecordCodec, H: Host>(
    repo: &Repository<'_, C>,
    host: &H,
    identity: &Value,
    body: NewAccount,
) -> Api {
    host.administrator(identity).await?;
    if !crate::password_length_ok(&body.password) {
        return Err(invalid("Password must be 8-1024 characters"));
    }
    let hash = crate::hash_password(body.password).await.map_err(|error| {
        if error.is::<tokio::sync::TryAcquireError>() {
            Error::Public(
                StatusCode::SERVICE_UNAVAILABLE,
                "Password service busy; try again shortly".into(),
            )
        } else if error.is::<tokio::task::JoinError>() {
            invalid("Password hashing unavailable")
        } else {
            invalid("Password hashing failed")
        }
    })?;
    let account = repo
        .create_user_account(&body.username, &hash, host.initial_user_access())
        .await
        .map_err(|_| stale("Account creation rejected; check username and existing accounts"))?;
    Ok(Json(json!({"account":account})))
}
pub async fn change_profile<C: RecordCodec, H: Host>(
    repo: &Repository<'_, C>,
    host: &H,
    identity: &Value,
    id: &str,
    body: ProfileChange,
) -> Api {
    host.administrator(identity).await?;
    let _guard = host.gate().lock().await;
    let profile = repo
        .update_user_profile(id, &body.username, &body.display_name)
        .await
        .map_err(|_| invalid("Profile update rejected"))?;
    Ok(Json(profile))
}
pub async fn account_sessions<C: RecordCodec, H: Host>(
    repo: &Repository<'_, C>,
    host: &H,
    identity: &Value,
    id: &str,
    now: u64,
) -> Api {
    host.administrator(identity).await?;
    let account = repo
        .account_state(id)
        .await?
        .ok_or_else(|| invalid("Account not found"))?;
    let sessions = repo
        .auth_sessions(id, account["generation"].as_str().unwrap_or(""), now)
        .await?;
    let public:Vec<Value>=sessions.into_iter().map(|s|json!({"id":s["id"],"user_agent":s["user_agent"],"created_at":s["created_at"],"expires_at":s["expires_at"]})).collect();
    Ok(Json(json!({"sessions":public})))
}
pub async fn revoke_account_session<C: RecordCodec, H: Host>(
    repo: &Repository<'_, C>,
    host: &H,
    identity: &Value,
    id: &str,
    session_id: &str,
    now: u64,
) -> Api {
    host.administrator(identity).await?;
    let _guard = host.gate().lock().await;
    let account = repo
        .account_state(id)
        .await?
        .ok_or_else(|| invalid("Account not found"))?;
    let sessions = repo
        .auth_sessions(id, account["generation"].as_str().unwrap_or(""), now)
        .await?;
    let session = sessions
        .iter()
        .find(|s| s["id"] == session_id)
        .ok_or_else(|| invalid("Sign-in session not found"))?;
    repo.delete_auth_key(
        session["key"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid session key"))?,
    )
    .await?;
    Ok(Json(json!({"revoked":true})))
}

pub(crate) fn password_error(error: anyhow::Error, message: &str) -> Error {
    if error.is::<tokio::sync::TryAcquireError>() {
        Error::Public(
            StatusCode::SERVICE_UNAVAILABLE,
            "Password service busy; try again shortly".into(),
        )
    } else {
        Error::Public(StatusCode::BAD_REQUEST, message.into())
    }
}
