use crate::{AccountStatus, Identity, RecordCodec, Repository};
use anyhow::Result;
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistrationMode {
    Restricted,
    Open,
}
impl RegistrationMode {
    /// Interpret the existing shared access-policy document; no alternate file or cache.
    pub fn from_policy(policy: &Value) -> Result<Self> {
        match policy["registration"].as_str() {
            Some("restricted") => Ok(Self::Restricted),
            Some("open") => Ok(Self::Open),
            _ => anyhow::bail!("Invalid registration policy"),
        }
    }
}
pub struct AuthenticatedSession {
    pub identity: Identity,
    pub document: Value,
    pub key: String,
}
// Preserve the existing digest(json!(token)) key representation.
pub(crate) fn auth_key(token: &str) -> String {
    format!("auth:{}", webagent_configuration::digest(&json!(token)))
}
impl<C: RecordCodec> Repository<'_, C> {
    pub async fn authenticate_session(
        &self,
        token: &str,
        now: u64,
        mode: RegistrationMode,
    ) -> Result<Option<AuthenticatedSession>> {
        if token.len() != 36 {
            return Ok(None);
        }
        let key = auth_key(token);
        let blob: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(&key)
            .fetch_optional(self.app_pool)
            .await?;
        let Some(blob) = blob else {
            return Ok(None);
        };
        let document = self.unseal(&blob)?;
        if document["expires"].as_u64().unwrap_or(0) <= now {
            return Ok(None);
        }
        let user = document["user_id"].as_str().unwrap_or("admin");
        let Some(account) = self.account_by_id(user).await? else {
            return Ok(None);
        };
        let Ok(identity) = crate::identity(&account) else {
            return Ok(None);
        };
        if identity.user_id != user
            || identity.status != AccountStatus::Active
            || account["is_approved"] != true
            || document["generation"] != account["generation"]
            || (account["role"] == "guest" && mode != RegistrationMode::Open)
        {
            return Ok(None);
        }
        Ok(Some(AuthenticatedSession {
            identity,
            document,
            key,
        }))
    }
    /// Revocation checks ownership in the same transaction as deletion.
    pub async fn revoke_session(&self, user: &str, id: &str) -> Result<bool> {
        let mut tx = self.app_pool.begin().await?;
        // Acquire the writer before checking ownership to fence concurrent replacement.
        sqlx::query("UPDATE docs SET value=value WHERE key LIKE 'auth:%'")
            .execute(&mut *tx)
            .await?;
        let rows: Vec<(String, Vec<u8>)> =
            sqlx::query_as("SELECT key,value FROM docs WHERE key LIKE 'auth:%'")
                .fetch_all(&mut *tx)
                .await?;
        for (key, blob) in rows {
            let doc = self.unseal(&blob)?;
            if doc["user_id"].as_str().unwrap_or("admin") == user && doc["id"] == id {
                sqlx::query("DELETE FROM docs WHERE key=?")
                    .bind(key)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                return Ok(true);
            }
        }
        tx.commit().await?;
        Ok(false)
    }
}

pub struct IssuedSession {
    pub token: String,
    pub csrf: String,
    pub expires: u64,
    pub id: String,
}
impl<C: RecordCodec> Repository<'_, C> {
    /// Consume the credential proof without refreshing its account generation.
    /// A concurrent password/status change therefore keeps the resulting cookie revoked.
    pub async fn issue_verified_session(
        &self,
        proof: crate::VerifiedCredentials,
        now: u64,
        user_agent: &str,
    ) -> Result<IssuedSession> {
        let duration = self
            .session_days(&proof.identity().user_id)
            .await?
            .checked_mul(86400)
            .ok_or_else(|| anyhow::anyhow!("Session duration overflow"))?;
        self.issue_versioned_session(
            proof.identity(),
            proof.generation(),
            now,
            user_agent,
            duration,
        )
        .await
    }
    pub(crate) async fn issue_versioned_session(
        &self,
        identity: &Identity,
        generation: &str,
        now: u64,
        user_agent: &str,
        duration: u64,
    ) -> Result<IssuedSession> {
        anyhow::ensure!(
            crate::valid_id(&identity.user_id)
                && identity.status == AccountStatus::Active
                && !generation.is_empty(),
            "Account is not active"
        );
        let token = uuid::Uuid::new_v4().to_string();
        let csrf = uuid::Uuid::new_v4().to_string();
        let id = uuid::Uuid::new_v4().to_string();
        let expires = now
            .checked_add(duration)
            .ok_or_else(|| anyhow::anyhow!("Session expiry overflow"))?;
        let doc = json!({"id":id,"user":identity.username,"user_id":identity.user_id,"csrf":csrf,"generation":generation,"created_at":now,"user_agent":user_agent.chars().take(256).collect::<String>(),"expires":expires});
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(auth_key(&token))
            .bind(self.seal(&doc)?)
            .execute(self.app_pool)
            .await?;
        Ok(IssuedSession {
            token,
            csrf,
            expires,
            id,
        })
    }
    pub async fn logout(&self, token: &str) -> Result<()> {
        if token.len() == 36 {
            sqlx::query("DELETE FROM docs WHERE key=?")
                .bind(auth_key(token))
                .execute(self.app_pool)
                .await?;
        }
        Ok(())
    }
}

impl<C: RecordCodec> Repository<'_, C> {
    pub(crate) async fn delete_auth_key(&self, key: &str) -> Result<()> {
        anyhow::ensure!(key.starts_with("auth:"), "Invalid authentication key");
        sqlx::query("DELETE FROM docs WHERE key=?")
            .bind(key)
            .execute(self.app_pool)
            .await?;
        Ok(())
    }
}

impl RegistrationMode {
    pub fn from_legacy(legacy: Option<&Value>) -> Self {
        if legacy.and_then(|v| v["access_mode"].as_str()) == Some("public_registered") {
            Self::Open
        } else {
            Self::Restricted
        }
    }
    pub const fn policy_name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Restricted => "restricted",
        }
    }
    pub fn write_policy(self, policy: &mut Value) -> Result<()> {
        anyhow::ensure!(policy.is_object(), "Invalid registration policy");
        policy["registration"] = json!(self.policy_name());
        Ok(())
    }
}
