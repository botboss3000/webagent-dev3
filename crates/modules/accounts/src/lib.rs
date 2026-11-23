//! Account identity, encrypted persistence and authentication lifecycle.
//! The host supplies its existing application pool and Storage codec.
mod account_rows;
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
// Repositories hold their codec by value so a host crate can pass a borrowed
// handle wrapper (the orphan rule keeps it from implementing this trait on a
// foreign storage type); plain references keep working through this impl.
impl<T: RecordCodec + ?Sized> RecordCodec for &T {
    fn seal(&self, value: &Value) -> Result<Vec<u8>> {
        (**self).seal(value)
    }
    fn unseal(&self, value: &[u8]) -> Result<Value> {
        (**self).unseal(value)
    }
}
pub struct Repository<'a, C: RecordCodec> {
    app_pool: &'a SqlitePool,
    codec: C,
}
impl<'a, C: RecordCodec> Repository<'a, C> {
    pub fn new(app_pool: &'a SqlitePool, codec: C) -> Self {
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
        account_rows::ensure_auth_epoch_column(self.app_pool).await
    }
    /// The account's plain `auth_epoch`, raised by every change to its row. `None` (no row, or
    /// no value) means the caller must treat any remembered identity as changed.
    pub async fn auth_epoch(&self, id: &str) -> Result<Option<i64>> {
        account_rows::auth_epoch(self.app_pool, id).await
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
/// Usernames of test accounts start with this; people cannot register or take one.
pub const TEST_USERNAME_PREFIX: &str = "test-";
/// Refusal when a change would alter a test account's role or tier.
pub const TEST_ACCOUNT_FIXED: &str =
    "A test account keeps the tier of its definition and is never an administrator";
/// Refusal for any password or password sign-in on a test account.
pub const TEST_ACCOUNT_NO_PASSWORD: &str =
    "Test accounts have no password; only the Diagnostics test_login tool signs in as one";
/// Whether `username` is reserved for test accounts (case-insensitive `test-` prefix).
#[must_use]
pub fn reserved_username(value: &str) -> bool {
    value
        .get(..TEST_USERNAME_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(TEST_USERNAME_PREFIX))
}
/// Whether an account record (or its projected state) is a test account.
#[must_use]
pub fn is_test_account(account: &Value) -> bool {
    account["is_test_account"] == true
}
mod test_accounts;
pub use test_accounts::{
    test_account_id, test_username, TestAccountLimits, TestLoginArguments, TestPersonaBinding,
    LIVE_TEST_ACCOUNTS, TEST_PERSONA_KIND,
};
fn username_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && !value.chars().any(char::is_control)
}
pub use authentication::{
    hash_password, message_api_authorized, message_api_token_hash, verify_password,
};
pub use sessions::{session_key, AgentBrowserBinding, IssuedSession};
pub use sessions::{AuthenticatedSession, RegistrationMode};
mod protection;
pub use protection::{session_cookie, session_token, valid_csrf, valid_origin};

mod bootstrap;

mod access_records;
pub use access_records::{AccessMutation, InitialAccess, PaidGrant, TierSources};

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
/// The `user_agent` of sign-ins the retired diagnostics browser-login bridge issued (route removed
/// 2026-11-23). Kept so administrator-only pages keep refusing any such record.
pub const DIAGNOSTICS_BROWSER_AGENT: &str = "Diagnostics v3 development browser";
/// Sign-in record `kind` of a sign-in issued to an agent's own browser (Browser Control).
pub const AGENT_BROWSER_KIND: &str = "agent_browser";
pub mod registration_api;
pub mod self_service;
pub use sessions::SessionCandidate;

mod proxy_trust;
pub use proxy_trust::{trust_settings, LIVE_AUTHENTICATION, PROOF_HASH};
mod throttling;
pub use throttling::{
    client_group, client_ip, AccountSlowdown, AddressBackoff, AddressLimits, Admission,
    AdmissionContext, LoginAttempts, LoginGate, LoginProtection, Recent, RequestLimits, UserLimits,
};

/// Apply one minimum to every password-creation path; the byte cap bounds hashing input.
pub fn password_length_ok(value: &str) -> bool {
    value.chars().count() >= 8 && value.len() <= 1024
}
