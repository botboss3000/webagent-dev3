//! Test accounts: one per account tier, for an administrator's agent to use the app as a
//! person of that tier would (Diagnostics `test_login`). They are flagged in the sealed record
//! and in the plain `is_test_account` column, have no password and keep the tier of their
//! definition. The rules that keep them apart from real accounts live with each writer
//! (`apply_access_in`, memberships, shared chats); this file creates and repairs them.
use crate::{RecordCodec, Repository};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use sqlx::Row;

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
}
