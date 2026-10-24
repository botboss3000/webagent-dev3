//! First-administrator account writes join the host's existing cross-owner transaction.
//! Access installs its membership in that same transaction before the host commits.
use crate::{RecordCodec, Repository};
use anyhow::{anyhow, Result};
use serde_json::json;
const ADMIN_ID: &str = "admin";
impl<C: RecordCodec> Repository<'_, C> {
    pub async fn create_admin_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        username: &str,
        password_hash: &str,
        generation: &str,
        initial: crate::InitialAccess<'_>,
    ) -> Result<()> {
        initial.validate()?;
        let account = json!({
            "user_id": ADMIN_ID,
            "username": username,
            "password_hash": password_hash,
            "generation": generation,
            "is_approved": true,
            "status": "active",
            "role": initial.role,
            "tier_id": initial.tier_id,
            "access_revision": 1
        });
        let profile = json!({
            "user_id": ADMIN_ID,
            "display_name": username,
            "is_admin": initial.profile_is_admin,
            "default_agent_id": "shared_default",
            "last_login_at": null
        });
        let legacy = json!({
            "username": username,
            "password_hash": password_hash,
            "generation": generation
        });
        let existing: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM user_accounts WHERE user_id=?")
                .bind(ADMIN_ID)
                .fetch_optional(&mut **tx)
                .await?;
        if existing.is_some() {
            return Err(anyhow!("Admin account already exists"));
        }
        sqlx::query("INSERT INTO user_accounts(user_id,username,value) VALUES(?,?,?)")
            .bind(ADMIN_ID)
            .bind(username)
            .bind(self.seal(&account)?)
            .execute(&mut **tx)
            .await?;
        sqlx::query("INSERT INTO user_profiles(user_id,value) VALUES(?,?)")
            .bind(ADMIN_ID)
            .bind(self.seal(&profile)?)
            .execute(&mut **tx)
            .await?;
        sqlx::query("INSERT INTO docs(key,value) VALUES('admin',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(self.seal(&legacy)?)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }
}

impl<C: RecordCodec> Repository<'_, C> {
    async fn legacy_admin_record(&self) -> Result<Option<serde_json::Value>> {
        let blob: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key='admin'")
            .fetch_optional(self.app_pool)
            .await?;
        blob.as_ref().map(|blob| self.unseal(blob)).transpose()
    }
    pub async fn legacy_admin_exists(&self) -> Result<bool> {
        Ok(self.legacy_admin_record().await?.is_some())
    }
    /// Existing idempotent startup import; keep the encrypted legacy document as rollback material.
    pub async fn import_original_admin(&self) -> Result<()> {
        let Some(legacy) = self.legacy_admin_record().await? else {
            return Ok(());
        };
        if legacy["generation"].as_str().is_none() {
            return Ok(());
        }
        let username = legacy["username"]
            .as_str()
            .ok_or_else(|| anyhow!("Legacy admin has no username"))?;
        let account = json!({"user_id":"admin","username":username,"password_hash":legacy["password_hash"],"generation":legacy["generation"],"is_approved":true});
        let profile = json!({"user_id":"admin","display_name":username,"is_admin":true,"default_agent_id":"shared_default","last_login_at":null});
        let mut tx = self.app_pool.begin().await?;
        sqlx::query("INSERT OR IGNORE INTO user_accounts(user_id,username,value) VALUES(?,?,?)")
            .bind("admin")
            .bind(username)
            .bind(self.seal(&account)?)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT OR IGNORE INTO user_profiles(user_id,value) VALUES(?,?)")
            .bind("admin")
            .bind(self.seal(&profile)?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}
