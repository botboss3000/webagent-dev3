//! Existing account/profile SQL over a borrowed application pool.
use crate::{RecordCodec, Repository};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use sqlx::Row;
const ADMIN_ID: &str = "admin";
async fn audit(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    store: &Repository<'_, impl RecordCodec>,
    kind: &str,
    payload: &Value,
) -> Result<()> {
    sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES('',?,?)")
        .bind(kind)
        .bind(store.seal(payload)?)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
impl<C: RecordCodec> Repository<'_, C> {
    pub async fn has_admin_account(&self) -> Result<bool> {
        let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM user_accounts WHERE user_id=?")
            .bind(ADMIN_ID)
            .fetch_optional(self.app_pool)
            .await?;
        Ok(found.is_some())
    }

    pub(crate) async fn account_by_id(&self, id: &str) -> Result<Option<Value>> {
        let row = sqlx::query("SELECT value FROM user_accounts WHERE user_id=?")
            .bind(id)
            .fetch_optional(self.app_pool)
            .await?;
        row.map(|r| self.unseal(&r.get::<Vec<u8>, _>("value")))
            .transpose()
    }

    pub(crate) async fn account_by_username(&self, username: &str) -> Result<Option<Value>> {
        let row = sqlx::query("SELECT value FROM user_accounts WHERE username=?")
            .bind(username)
            .fetch_optional(self.app_pool)
            .await?;
        row.map(|r| self.unseal(&r.get::<Vec<u8>, _>("value")))
            .transpose()
    }

    pub async fn user_profile(&self, id: &str) -> Result<Option<Value>> {
        let row = sqlx::query("SELECT value FROM user_profiles WHERE user_id=?")
            .bind(id)
            .fetch_optional(self.app_pool)
            .await?;
        row.map(|r| self.unseal(&r.get::<Vec<u8>, _>("value")))
            .transpose()
    }

    pub async fn record_user_login(&self, id: &str, timestamp: u64) -> Result<()> {
        let mut tx = self.app_pool.begin().await?;
        let row = sqlx::query("SELECT value FROM user_profiles WHERE user_id=?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let mut profile = self.unseal(&row.get::<Vec<u8>, _>("value"))?;
        profile["last_login_at"] = json!(timestamp);
        sqlx::query("UPDATE user_profiles SET value=? WHERE user_id=?")
            .bind(self.seal(&profile)?)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn update_user_profile(
        &self,
        user_id: &str,
        username: &str,
        display_name: &str,
    ) -> Result<Value> {
        if !crate::valid_id(user_id)
            || username.is_empty()
            || username.len() > 128
            || username.trim() != username
            || username.chars().any(char::is_control)
            || display_name.is_empty()
            || display_name.len() > 128
            || display_name.trim() != display_name
            || display_name.chars().any(char::is_control)
        {
            bail!("Invalid profile input");
        }
        let mut tx = self.app_pool.begin().await?;
        let row = sqlx::query("SELECT value FROM user_accounts WHERE user_id=?")
            .bind(user_id)
            .fetch_one(&mut *tx)
            .await?;
        let mut account = self.unseal(&row.get::<Vec<u8>, _>("value"))?;
        account["username"] = json!(username);
        sqlx::query("UPDATE user_accounts SET username=?,value=? WHERE user_id=?")
            .bind(username)
            .bind(self.seal(&account)?)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        let row = sqlx::query("SELECT value FROM user_profiles WHERE user_id=?")
            .bind(user_id)
            .fetch_one(&mut *tx)
            .await?;
        let mut profile = self.unseal(&row.get::<Vec<u8>, _>("value"))?;
        profile["display_name"] = json!(display_name);
        sqlx::query("UPDATE user_profiles SET value=? WHERE user_id=?")
            .bind(self.seal(&profile)?)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        if user_id == ADMIN_ID {
            let legacy = json!({"username":username,"password_hash":account["password_hash"],
                "generation":account["generation"]});
            sqlx::query(
                "INSERT INTO docs(key,value) VALUES('admin',?)
                ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            )
            .bind(self.seal(&legacy)?)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES('','profile_updated',?)")
            .bind(self.seal(&json!({"user_id":user_id}))?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(
            json!({"user":username,"user_id":user_id,"display_name":display_name,
            "is_admin":account["status"] == "active" && account["role"] == "administrator"}),
        )
    }

    pub async fn session_days(&self, id: &str) -> Result<u64> {
        let profile = self.user_profile(id).await?;
        Ok(profile
            .as_ref()
            .and_then(|v| v["session_days"].as_u64())
            .unwrap_or(1))
    }

    pub async fn set_session_days(&self, id: &str, days: u64) -> Result<()> {
        anyhow::ensure!([1, 7, 30, 90].contains(&days), "Invalid sign-in duration");
        let account = self
            .account_by_id(id)
            .await?
            .ok_or_else(|| anyhow!("Account not found"))?;
        anyhow::ensure!(
            account["role"] != "guest",
            "Guest sign-in duration is fixed"
        );
        let mut tx = self.app_pool.begin().await?;
        let row = sqlx::query("SELECT value FROM user_profiles WHERE user_id=?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let mut profile = self.unseal(&row.get::<Vec<u8>, _>("value"))?;
        profile["session_days"] = json!(days);
        sqlx::query("UPDATE user_profiles SET value=? WHERE user_id=?")
            .bind(self.seal(&profile)?)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn auth_sessions(
        &self,
        user_id: &str,
        generation: &str,
        current_time: u64,
    ) -> Result<Vec<Value>> {
        let rows = sqlx::query("SELECT key,value FROM docs WHERE key LIKE 'auth:%' ORDER BY key")
            .fetch_all(self.app_pool)
            .await?;
        let mut sessions = Vec::new();
        for row in rows {
            let key: String = row.get("key");
            let value = self.unseal(&row.get::<Vec<u8>, _>("value"))?;
            if value["expires"].as_u64().unwrap_or(0) <= current_time
                || value["generation"] != generation
                || value["user_id"].as_str().unwrap_or("admin") != user_id
            {
                continue;
            }
            sessions.push(json!({"id":value["id"].as_str().unwrap_or(&key[5..]),
                "key":key,"user_agent":value["user_agent"],"created_at":value["created_at"],
                "expires_at":value["expires"]}));
        }
        Ok(sessions)
    }

    pub async fn replace_user_password(
        &self,
        user_id: &str,
        expected_generation: &str,
        password_hash: &str,
        generation: &str,
    ) -> Result<()> {
        if !crate::valid_id(user_id)
            || expected_generation.is_empty()
            || password_hash.len() < 20
            || generation.is_empty()
        {
            bail!("Invalid password update");
        }
        let mut tx = self.app_pool.begin().await?;
        let row = sqlx::query("SELECT value FROM user_accounts WHERE user_id=?")
            .bind(user_id)
            .fetch_one(&mut *tx)
            .await?;
        let mut account = self.unseal(&row.get::<Vec<u8>, _>("value"))?;
        if account["generation"] != expected_generation {
            bail!("Account changed; sign in again");
        }
        account["password_hash"] = json!(password_hash);
        account["generation"] = json!(generation);
        sqlx::query("UPDATE user_accounts SET value=? WHERE user_id=?")
            .bind(self.seal(&account)?)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        if user_id == ADMIN_ID {
            let legacy = json!({"username":account["username"],
                "password_hash":password_hash,"generation":generation});
            sqlx::query(
                "INSERT INTO docs(key,value) VALUES('admin',?)
                ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            )
            .bind(self.seal(&legacy)?)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES('','password_changed',?)")
            .bind(self.seal(&json!({"user_id":user_id}))?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Guests are durable independent identities, never password accounts.
    pub async fn create_guest_account(&self, initial: crate::InitialAccess<'_>) -> Result<Value> {
        initial.validate()?;
        let id = format!("guest_{}", uuid::Uuid::new_v4().simple());
        let account = json!({"user_id":id,"username":id,"password_hash":null,
            "generation":uuid::Uuid::new_v4().to_string(),"is_approved":true,
            "status":"active","role":initial.role,"tier_id":initial.tier_id,
            "access_revision":1});
        let profile = json!({"user_id":id,"display_name":"Guest","is_admin":initial.profile_is_admin,
            "default_agent_id":null,"last_login_at":null,"source_channel":"web_public"});
        let mut tx = self.app_pool.begin().await?;
        sqlx::query("INSERT INTO user_accounts(user_id,username,value) VALUES(?,?,?)")
            .bind(&id)
            .bind(&id)
            .bind(self.seal(&account)?)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO user_profiles(user_id,value) VALUES(?,?)")
            .bind(&id)
            .bind(self.seal(&profile)?)
            .execute(&mut *tx)
            .await?;
        audit(
            &mut tx,
            self,
            "account_created",
            &json!({"user_id":id,"role":initial.role}),
        )
        .await?;
        tx.commit().await?;
        Ok(crate::access_records::state(&account))
    }
    pub async fn create_user_account(
        &self,
        username: &str,
        password_hash: &str,
        initial: crate::InitialAccess<'_>,
    ) -> Result<Value> {
        initial.validate()?;
        if !crate::username_ok(username) || password_hash.len() < 20 || password_hash.len() > 1024 {
            bail!("Invalid account input");
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let account = json!({"user_id":id,"username":username,"password_hash":password_hash,
            "generation":uuid::Uuid::new_v4().to_string(),"is_approved":false,
            "status":"pending","role":initial.role,"tier_id":initial.tier_id,
            "access_revision":1});
        let profile = json!({"user_id":id,"display_name":username,"is_admin":initial.profile_is_admin,
            "default_agent_id":null,"last_login_at":null});
        let mut tx = self.app_pool.begin().await?;
        sqlx::query("INSERT INTO user_accounts(user_id,username,value) VALUES(?,?,?)")
            .bind(&id)
            .bind(username)
            .bind(self.seal(&account)?)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO user_profiles(user_id,value) VALUES(?,?)")
            .bind(&id)
            .bind(self.seal(&profile)?)
            .execute(&mut *tx)
            .await?;
        audit(
            &mut tx,
            self,
            "account_created",
            &json!({"user_id":id,"status":"pending"}),
        )
        .await?;
        tx.commit().await?;
        Ok(
            json!({"user_id":account["user_id"],"username":account["username"],"status":account["status"],"role":account["role"],"tier_id":account["tier_id"],"access_revision":account["access_revision"]}),
        )
    }
}
