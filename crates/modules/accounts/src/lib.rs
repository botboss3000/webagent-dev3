//! Account identity, encrypted persistence and authentication lifecycle.
//! The host supplies its existing application pool and Storage codec.
mod authentication;
mod repository;
mod sessions;
#[cfg(test)]
mod tests;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    Pending,
    Active,
    Suspended,
    Disabled,
}

/// Public identity contains no password hash, session token or authorization grants.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Identity {
    pub user_id: String,
    pub username: String,
    pub status: AccountStatus,
}

pub trait RecordCodec: Sync {
    fn seal(&self, value: &Value) -> Result<Vec<u8>>;
    fn unseal(&self, value: &[u8]) -> Result<Value>;
}
pub struct Repository<'a, C: RecordCodec> {
    app_pool: &'a SqlitePool,
    codec: &'a C,
}
impl<'a, C: RecordCodec> Repository<'a, C> {
    pub fn new(app_pool: &'a SqlitePool, codec: &'a C) -> Self {
        Self { app_pool, codec }
    }
    fn seal(&self, value: &Value) -> Result<Vec<u8>> {
        self.codec.seal(value)
    }
    fn unseal(&self, value: &[u8]) -> Result<Value> {
        self.codec.unseal(value)
    }
    pub async fn identity(&self, id: &str) -> Result<Option<Identity>> {
        self.account_by_id(id)
            .await?
            .map(|v| identity(&v))
            .transpose()
    }
    /// Only Accounts tables; Access and Storage retain their own schema initialization.
    pub async fn initialize(&self) -> Result<()> {
        sqlx::raw_sql("CREATE TABLE IF NOT EXISTS user_accounts(user_id TEXT PRIMARY KEY,username TEXT NOT NULL UNIQUE,value BLOB NOT NULL);
   CREATE TABLE IF NOT EXISTS user_profiles(user_id TEXT PRIMARY KEY REFERENCES user_accounts(user_id),value BLOB NOT NULL);")
   .execute(self.app_pool).await?;
        Ok(())
    }
}
fn identity(v: &Value) -> Result<Identity> {
    let id = v["user_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Account has no stable identity"))?;
    anyhow::ensure!(valid_id(id), "Invalid account identity");
    let status = account_status(v)?;
    Ok(Identity {
        user_id: id.into(),
        username: v["username"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Account has no username"))?
            .into(),
        status,
    })
}
pub fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
fn username_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && !value.chars().any(char::is_control)
}
pub use authentication::{
    hash_password, message_api_authorized, message_api_token_hash, verify_password, LoginAttempts,
};
pub use sessions::IssuedSession;
pub use sessions::{AuthenticatedSession, RegistrationMode};
mod protection;
pub use protection::{session_cookie, session_token, valid_csrf, valid_origin, BrowserLoginBridge};

mod bootstrap;

mod access_records;
pub use access_records::{AccessMutation, InitialAccess};

pub mod api;

pub fn account_status(account: &Value) -> Result<AccountStatus> {
    let id = account["user_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Account has no stable identity"))?;
    anyhow::ensure!(valid_id(id), "Invalid account identity");
    match account["status"].as_str() {
        Some(value) => Ok(serde_json::from_value(Value::String(value.into()))?),
        None if id == "admin" => Ok(AccountStatus::Active),
        _ => anyhow::bail!("Account has no status"),
    }
}

pub use authentication::VerifiedCredentials;

pub mod authentication_api;
pub mod browser_login_api;
pub mod registration_api;
pub mod self_service;
pub use sessions::SessionCandidate;
