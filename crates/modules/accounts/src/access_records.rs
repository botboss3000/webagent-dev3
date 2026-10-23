//! Borrowed transaction operations for Accounts records; Access supplies validated grants.
use crate::{AccountStatus, RecordCodec, Repository};
use anyhow::{anyhow, ensure, Result};
use serde_json::{json, Value};
use sqlx::{Sqlite, Transaction};

/// Persisted authorization assignment supplied by Access, never evaluated by Accounts.
pub struct AccessMutation<'a> {
    pub expected_revision: u64,
    pub status: AccountStatus,
    pub role: &'a str,
    pub tier_id: &'a str,
    pub profile_is_admin: bool,
}
pub(crate) fn state(value: &Value) -> Value {
    let mut result = serde_json::Map::new();
    for field in [
        "user_id",
        "username",
        "status",
        "role",
        "tier_id",
        "access_revision",
        "generation",
        "is_approved",
    ] {
        if let Some(value) = value.get(field) {
            result.insert(field.into(), value.clone());
        }
    }
    Value::Object(result)
}
impl<C: RecordCodec> Repository<'_, C> {
    pub async fn account_state(&self, id: &str) -> Result<Option<Value>> {
        Ok(self.account_by_id(id).await?.as_ref().map(state))
    }
    pub async fn all_user_ids(&self) -> Result<Vec<String>> {
        Ok(
            sqlx::query_scalar("SELECT user_id FROM user_accounts ORDER BY user_id")
                .fetch_all(self.app_pool)
                .await?,
        )
    }
    pub async fn list_account_states(&self) -> Result<Vec<Value>> {
        let blobs: Vec<Vec<u8>> =
            sqlx::query_scalar("SELECT value FROM user_accounts ORDER BY username LIMIT 1000")
                .fetch_all(self.app_pool)
                .await?;
        blobs
            .iter()
            .map(|blob| Ok(state(&self.unseal(blob)?)))
            .collect()
    }
    pub async fn account_state_in(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: &str,
    ) -> Result<Option<Value>> {
        let blob: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT value FROM user_accounts WHERE user_id=?")
                .bind(id)
                .fetch_optional(&mut **tx)
                .await?;
        blob.as_ref()
            .map(|blob| Ok(state(&self.unseal(blob)?)))
            .transpose()
    }
    pub async fn lock_account_in(&self, tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<()> {
        sqlx::query("UPDATE user_accounts SET value=value WHERE user_id=?")
            .bind(id)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }
    pub async fn all_account_states_in(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
    ) -> Result<Vec<(String, Value)>> {
        let rows: Vec<(String, Vec<u8>)> =
            sqlx::query_as("SELECT user_id,value FROM user_accounts")
                .fetch_all(&mut **tx)
                .await?;
        rows.into_iter()
            .map(|(id, blob)| Ok((id, state(&self.unseal(&blob)?))))
            .collect()
    }
    /// Caller retains its transaction until the Access audit/assignment work also succeeds.
    pub async fn apply_access_in(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: &str,
        update: AccessMutation<'_>,
    ) -> Result<Value> {
        ensure!(
            crate::valid_id(id) && crate::valid_id(update.tier_id) && update.expected_revision > 0,
            "Invalid account update"
        );
        ensure!(
            matches!(update.role, "guest" | "user" | "administrator"),
            "Invalid account role"
        );
        self.lock_account_in(tx, id).await?;
        let blob: Vec<u8> = sqlx::query_scalar("SELECT value FROM user_accounts WHERE user_id=?")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| anyhow!("Account not found"))?;
        let mut account = self.unseal(&blob)?;
        let revision = account["access_revision"]
            .as_u64()
            .unwrap_or(if id == "admin" { 1 } else { 0 });
        ensure!(
            revision == update.expected_revision,
            "Account revision changed"
        );
        account["status"] = json!(update.status);
        account["role"] = json!(update.role);
        account["tier_id"] = json!(update.tier_id);
        account["access_revision"] = json!(revision
            .checked_add(1)
            .ok_or_else(|| anyhow!("Account revision overflow"))?);
        account["generation"] = json!(uuid::Uuid::new_v4().to_string());
        account["is_approved"] = json!(update.status == AccountStatus::Active);
        sqlx::query("UPDATE user_accounts SET value=? WHERE user_id=?")
            .bind(self.seal(&account)?)
            .bind(id)
            .execute(&mut **tx)
            .await?;
        let blob: Vec<u8> = sqlx::query_scalar("SELECT value FROM user_profiles WHERE user_id=?")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
        let mut profile = self.unseal(&blob)?;
        profile["is_admin"] = json!(update.profile_is_admin);
        sqlx::query("UPDATE user_profiles SET value=? WHERE user_id=?")
            .bind(self.seal(&profile)?)
            .bind(id)
            .execute(&mut **tx)
            .await?;
        Ok(state(&account))
    }
    pub async fn bump_access_version_in(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        id: &str,
        expected: u64,
    ) -> Result<()> {
        self.lock_account_in(tx, id).await?;
        let blob: Vec<u8> = sqlx::query_scalar("SELECT value FROM user_accounts WHERE user_id=?")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
        let mut account = self.unseal(&blob)?;
        ensure!(
            account["access_revision"]
                .as_u64()
                .unwrap_or(if id == "admin" { 1 } else { 0 })
                == expected,
            "Account revision changed"
        );
        account["access_revision"] = json!(expected
            .checked_add(1)
            .ok_or_else(|| anyhow!("Account revision overflow"))?);
        account["generation"] = json!(uuid::Uuid::new_v4().to_string());
        sqlx::query("UPDATE user_accounts SET value=? WHERE user_id=?")
            .bind(self.seal(&account)?)
            .bind(id)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }
    /// Existing startup import only: Access owns which initial grant is supplied.
    pub async fn initialize_admin_assignment_in(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        status: AccountStatus,
        role: &str,
        tier: &str,
    ) -> Result<bool> {
        let blob: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT value FROM user_accounts WHERE user_id='admin'")
                .fetch_optional(&mut **tx)
                .await?;
        let Some(blob) = blob else {
            return Ok(false);
        };
        let mut account = self.unseal(&blob)?;
        if ["status", "role", "tier_id", "access_revision"]
            .iter()
            .any(|key| account[*key].is_null())
        {
            account["status"] = json!(status);
            account["role"] = json!(role);
            account["tier_id"] = json!(tier);
            account["access_revision"] = json!(1);
            sqlx::query("UPDATE user_accounts SET value=? WHERE user_id='admin'")
                .bind(self.seal(&account)?)
                .execute(&mut **tx)
                .await?;
        }
        Ok(true)
    }
}

#[derive(Clone, Copy)]
pub struct InitialAccess<'a> {
    pub role: &'a str,
    pub tier_id: &'a str,
    pub profile_is_admin: bool,
}
impl InitialAccess<'_> {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            matches!(self.role, "guest" | "user" | "administrator")
                && crate::valid_id(self.tier_id),
            "Invalid initial account assignment"
        );
        Ok(())
    }
}
