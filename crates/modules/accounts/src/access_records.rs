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
    /// When the tier ends (Unix seconds) and the account returns to Free; `None` keeps it
    /// until changed. The `account_due` row that brings the change is the caller's.
    pub tier_until: Option<u64>,
    /// The tier's other sources and the catalog switch; `None` keeps the stored ones.
    pub sources: Option<TierSources<'a>>,
    pub profile_is_admin: bool,
}

/// Where an account's tier comes from beside the administrator's choice, and its catalog
/// switch (plus tier spec 3.4, 3.8), supplied by Access, never evaluated by Accounts.
pub struct TierSources<'a> {
    /// The administrator's own tier choice while a paid grant raises the account above it
    /// (`None`: `tier_id` is the administrator's choice).
    pub granted_tier: Option<&'a str>,
    /// The billing service's paid grant.
    pub paid: Option<PaidGrant<'a>>,
    /// Whether the account's agents may run catalog models (`None`: its tier's default).
    pub catalog_models: Option<bool>,
}

/// A paid grant: a tier until a time (Unix seconds), applied once per provider reference.
pub struct PaidGrant<'a> {
    pub tier: &'a str,
    pub until: u64,
    pub reference: &'a str,
}
pub(crate) fn state(value: &Value) -> Value {
    let mut result = serde_json::Map::new();
    for field in [
        "user_id",
        "username",
        "status",
        "role",
        "tier_id",
        "tier_until",
        "granted_tier",
        "paid",
        "catalog_models",
        "access_revision",
        "generation",
        "is_approved",
        "is_test_account",
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
    /// Whether the account exists: a primary-key lookup without decrypting its record.
    pub async fn account_exists(&self, id: &str) -> Result<bool> {
        Ok(
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM user_accounts WHERE user_id=?)")
                .bind(id)
                .fetch_one(self.app_pool)
                .await?,
        )
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
        crate::account_rows::lock(tx, id).await
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
        ensure!(crate::valid_id(update.role), "Invalid account role");
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
        // A test account keeps the role and tier of its definition: never administrator.
        if crate::is_test_account(&account) {
            ensure!(
                update.role != "administrator"
                    && account["role"] == update.role
                    && account["tier_id"] == update.tier_id,
                "{}",
                crate::TEST_ACCOUNT_FIXED
            );
        }
        account["status"] = json!(update.status);
        account["role"] = json!(update.role);
        account["tier_id"] = json!(update.tier_id);
        match update.tier_until {
            Some(until) => account["tier_until"] = json!(until),
            None => {
                if let Some(record) = account.as_object_mut() {
                    record.remove("tier_until");
                }
            }
        }
        if let Some(sources) = &update.sources {
            ensure!(
                sources.granted_tier.is_none_or(crate::valid_id)
                    && sources
                        .paid
                        .as_ref()
                        .is_none_or(|paid| crate::valid_id(paid.tier) && !paid.reference.is_empty()),
                "Invalid account update"
            );
            let record = account
                .as_object_mut()
                .ok_or_else(|| anyhow!("Invalid account record"))?;
            match sources.granted_tier {
                Some(tier) => record.insert("granted_tier".into(), json!(tier)),
                None => record.remove("granted_tier"),
            };
            match &sources.paid {
                Some(paid) => record.insert(
                    "paid".into(),
                    json!({"tier":paid.tier,"until":paid.until,"reference":paid.reference}),
                ),
                None => record.remove("paid"),
            };
            match sources.catalog_models {
                Some(allowed) => record.insert("catalog_models".into(), json!(allowed)),
                None => record.remove("catalog_models"),
            };
        }
        account["access_revision"] = json!(revision
            .checked_add(1)
            .ok_or_else(|| anyhow!("Account revision overflow"))?);
        // Role and tier apply from the next request through the access revision; only a
        // deactivation ends the account's sign-ins.
        if update.status != AccountStatus::Active {
            account["generation"] = json!(uuid::Uuid::new_v4().to_string());
        }
        account["is_approved"] = json!(update.status == AccountStatus::Active);
        crate::account_rows::update(tx, id, None, &self.seal(&account)?).await?;
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
        // A tier edit applies from the next request; it does not sign the tier's accounts out.
        account["access_revision"] = json!(expected
            .checked_add(1)
            .ok_or_else(|| anyhow!("Account revision overflow"))?);
        crate::account_rows::update(tx, id, None, &self.seal(&account)?).await?;
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
            crate::account_rows::update(tx, "admin", None, &self.seal(&account)?).await?;
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
            crate::valid_id(self.role) && crate::valid_id(self.tier_id),
            "Invalid initial account assignment"
        );
        Ok(())
    }
}

impl<C: RecordCodec> Repository<'_, C> {
    pub async fn account_state_by_username(&self, username: &str) -> Result<Option<Value>> {
        Ok(self
            .account_by_username(username)
            .await?
            .as_ref()
            .map(state))
    }
}
