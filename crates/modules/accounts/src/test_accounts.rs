//! Test accounts: one per account tier, for an administrator's agent to use the app as a
//! person of that tier would (Diagnostics `test_login`). They are flagged in the sealed record
//! and in the plain `is_test_account` column, have no password and keep the tier of their
//! definition. The rules that keep them apart from real accounts live with each writer
//! (`apply_access_in`, memberships, shared chats); this file creates and repairs them.
use crate::{AccountStatus, RecordCodec, Repository};
use anyhow::{bail, ensure, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

/// The live limits file, relative to the checkout.
pub const LIVE_TEST_ACCOUNTS: &str = "config/accounts/test-accounts.json";
/// Sign-in record `kind` of a test-persona sign-in (Diagnostics `test_login`).
pub const TEST_PERSONA_KIND: &str = "test_persona";

/// `config/accounts/test-accounts.json`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TestAccountLimits {
    schema_version: u32,
    /// Runs one test account may have going at once.
    pub max_active_runs: u32,
    /// The longest a test sign-in lasts.
    pub sign_in_minutes: u64,
    /// Test accounts' chats older than this are deleted.
    pub session_retention_days: u64,
}

impl Default for TestAccountLimits {
    fn default() -> Self {
        Self {
            schema_version: 1,
            max_active_runs: 2,
            sign_in_minutes: 120,
            session_retention_days: 7,
        }
    }
}

impl TestAccountLimits {
    /// Parses and bounds the file.
    ///
    /// # Errors
    /// Invalid JSON, an unknown key or a value out of range.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let mut value: Value = serde_json::from_slice(bytes)?;
        if let Some(object) = value.as_object_mut() {
            object.remove("_instructions");
        }
        let limits: Self = serde_json::from_value(value)?;
        ensure!(limits.schema_version == 1, "schema_version must be 1");
        ensure!(
            (1..=20).contains(&limits.max_active_runs),
            "max_active_runs must be 1..=20"
        );
        ensure!(
            (5..=1440).contains(&limits.sign_in_minutes),
            "sign_in_minutes must be 5..=1440"
        );
        ensure!(
            (1..=365).contains(&limits.session_retention_days),
            "session_retention_days must be 1..=365"
        );
        Ok(limits)
    }

    /// The live limits; the defaults (the shipped values) when the file is missing or invalid.
    #[must_use]
    pub fn live() -> Self {
        webagent_configuration::live::parsed(LIVE_TEST_ACCOUNTS, 16 * 1024, Self::parse)
            .unwrap_or_default()
    }
}

/// Diagnostics `test_login`'s arguments, decoded by the app (checked against its tool.json).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestLoginArguments {
    /// The account tier id, for example `free` or `anonymous`.
    pub tier: String,
}

/// What a test-persona sign-in is bound to: the administrator, session and run that made it.
pub struct TestPersonaBinding<'a> {
    pub creator_user_id: &'a str,
    pub creator_session_id: &'a str,
    pub creator_run_id: &'a str,
}

/// The fixed account id of tier `tier_id`'s test account.
#[must_use]
pub fn test_account_id(tier_id: &str) -> String {
    format!("test_{tier_id}")
}

/// The reserved username of tier `tier_id`'s test account.
#[must_use]
pub fn test_username(tier_id: &str) -> String {
    format!(
        "{}{}",
        crate::TEST_USERNAME_PREFIX,
        tier_id.replace('_', "-")
    )
}

impl<C: RecordCodec> Repository<'_, C> {
    /// Creates tier `tier_id`'s test account, or repairs its role, tier and flag; idempotent.
    /// `None` when a person already holds the reserved username (left untouched).
    ///
    /// # Errors
    /// The administrator or anonymous tier (never test accounts), an invalid tier id, or a
    /// store failure.
    pub async fn ensure_test_account(&self, tier_id: &str, role: &str) -> Result<Option<Value>> {
        if tier_id == "administrator" || role == "administrator" {
            bail!("{}", crate::TEST_ACCOUNT_FIXED);
        }
        if tier_id == "anonymous" || role == "guest" {
            bail!("The anonymous tier's test visitor is a fresh guest per sign-in");
        }
        let id = test_account_id(tier_id);
        if !crate::valid_id(tier_id) || !crate::valid_id(&id) || !crate::valid_id(role) {
            bail!("Invalid test account tier");
        }
        let username = test_username(tier_id);
        let mut tx = self.app_pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query("SELECT value FROM user_accounts WHERE user_id=?")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await?;
        if let Some(row) = row {
            let mut account = self.unseal(&row.get::<Vec<u8>, _>("value"))?;
            let wanted = [
                ("role", json!(role)),
                ("tier_id", json!(tier_id)),
                ("is_test_account", json!(true)),
                ("password_hash", Value::Null),
            ];
            if wanted.iter().any(|(key, value)| &account[*key] != value) {
                for (key, value) in wanted {
                    account[key] = value;
                }
                let revision = account["access_revision"].as_u64().unwrap_or(0);
                account["access_revision"] = json!(revision.saturating_add(1));
                crate::account_rows::update(&mut tx, &id, None, &self.seal(&account)?).await?;
            }
            tx.commit().await?;
            return Ok(Some(crate::access_records::state(&account)));
        }
        let taken: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM user_accounts WHERE username=?)")
                .bind(&username)
                .fetch_one(&mut *tx)
                .await?;
        if taken {
            return Ok(None);
        }
        let account = json!({"user_id":id,"username":username,"password_hash":null,
            "generation":uuid::Uuid::new_v4().to_string(),"is_approved":true,
            "status":"active","role":role,"tier_id":tier_id,"access_revision":1,
            "is_test_account":true});
        let profile = json!({"user_id":id,"display_name":format!("Test ({tier_id})"),
            "is_admin":false,"default_agent_id":null,"last_login_at":null,
            "source_channel":"test_account"});
        crate::account_rows::insert_test(&mut tx, &id, &username, &self.seal(&account)?).await?;
        sqlx::query("INSERT INTO user_profiles(user_id,value) VALUES(?,?)")
            .bind(&id)
            .bind(self.seal(&profile)?)
            .execute(&mut *tx)
            .await?;
        crate::repository::audit(
            &mut tx,
            self,
            "account_created",
            &json!({"user_id":id,"role":role,"test_account":true}),
        )
        .await?;
        tx.commit().await?;
        Ok(Some(crate::access_records::state(&account)))
    }

    /// Every test account's id (plain column, no decryption).
    pub async fn test_account_ids(&self) -> Result<Vec<String>> {
        crate::account_rows::test_account_ids(self.app_pool).await
    }

    /// Whether `user_id` is a test account, from the plain column (no decryption); a missing
    /// account reads as not a test account.
    pub async fn is_test_account_id(&self, user_id: &str) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, i64>(
            "SELECT is_test_account FROM user_accounts WHERE user_id=?",
        )
        .bind(user_id)
        .fetch_optional(self.app_pool)
        .await?
            == Some(1))
    }

    /// A sign-in as test account `user` for the run that asked for it. The record names its
    /// kind and creator; it lasts `duration` seconds at most, and the host ends it sooner when
    /// the creating run ends. The token is the cookie value: callers install it in the run's
    /// own browser and never return, store elsewhere or log it.
    ///
    /// # Errors
    /// The account is not an active test account, or a binding field is malformed.
    pub async fn issue_test_persona_session(
        &self,
        user: &str,
        now: u64,
        duration: u64,
        binding: TestPersonaBinding<'_>,
    ) -> Result<crate::IssuedSession> {
        let account = self
            .account_by_id(user)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Test account unavailable"))?;
        let identity = crate::identity(&account)?;
        ensure!(
            crate::is_test_account(&account)
                && identity.user_id == user
                && identity.status == AccountStatus::Active
                && account["is_approved"] == true,
            "Test account unavailable"
        );
        ensure!(
            crate::valid_id(binding.creator_user_id)
                && [binding.creator_session_id, binding.creator_run_id]
                    .iter()
                    .all(|id| (1..=64).contains(&id.len())
                        && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')),
            "Invalid test sign-in binding"
        );
        let generation = account["generation"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        self.issue_record(
            &identity,
            &generation,
            now,
            "Diagnostics test sign-in",
            duration,
            json!({"kind": TEST_PERSONA_KIND,
                "creator_user_id": binding.creator_user_id,
                "creator_session_id": binding.creator_session_id,
                "creator_run_id": binding.creator_run_id}),
        )
        .await
    }
}
