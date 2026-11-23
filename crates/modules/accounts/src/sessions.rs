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
/// The sign-in record key of a browser cookie value; `None` for a malformed token.
pub fn session_key(token: &str) -> Option<String> {
    (token.len() == 36).then(|| auth_key(token))
}
impl<C: RecordCodec> Repository<'_, C> {
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

/// Session snapshot pending the host's live Access admission check.
pub struct SessionCandidate {
    identity: Identity,
    document: Value,
    key: String,
    state: Value,
    generation_matches: bool,
    auth_epoch: Option<i64>,
    record: Vec<u8>,
}
impl SessionCandidate {
    /// The sign-in record's sealed bytes as read, for later unchanged-record checks.
    pub fn record(&self) -> &[u8] {
        &self.record
    }
    /// The account's `auth_epoch` read together with the record this candidate came from.
    pub fn auth_epoch(&self) -> Option<i64> {
        self.auth_epoch
    }
    pub fn account_state(&self) -> &Value {
        &self.state
    }
    pub fn guest(&self) -> bool {
        self.state["role"] == "guest"
    }
    pub fn finish(self) -> Option<AuthenticatedSession> {
        self.generation_matches.then_some(AuthenticatedSession {
            identity: self.identity,
            document: self.document,
            key: self.key,
        })
    }
}
impl<C: RecordCodec> Repository<'_, C> {
    /// The account's `auth_epoch` while sign-in record `key` still holds the sealed bytes
    /// `record` (one indexed read, no decryption); `None` means the remembered identity is
    /// stale or the sign-in ended.
    pub async fn sign_in_epoch(&self, user: &str, key: &str, record: &[u8]) -> Result<Option<i64>> {
        crate::account_rows::sign_in_epoch(self.app_pool, user, key, record).await
    }
    /// Access page authorization deliberately precedes generation validation in browser middleware.
    pub async fn inspect_session(&self, token: &str, now: u64) -> Result<Option<SessionCandidate>> {
        if token.len() != 36 {
            return Ok(None);
        }
        self.inspect_session_key(&auth_key(token), now).await
    }
    async fn inspect_session_key(&self, key: &str, now: u64) -> Result<Option<SessionCandidate>> {
        if !key.starts_with("auth:") {
            return Ok(None);
        }
        let blob: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(key)
            .fetch_optional(self.app_pool)
            .await?;
        let Some(record) = blob else {
            return Ok(None);
        };
        let document = self.unseal(&record)?;
        if document["expires"].as_u64().unwrap_or(0) <= now {
            return Ok(None);
        }
        let user = document["user_id"].as_str().unwrap_or("admin");
        let Some((blob, auth_epoch)) =
            crate::account_rows::record_with_epoch(self.app_pool, user).await?
        else {
            return Ok(None);
        };
        let account = self.unseal(&blob)?;
        let Ok(identity) = crate::identity(&account) else {
            return Ok(None);
        };
        if identity.user_id != user
            || identity.status != AccountStatus::Active
            || account["is_approved"] != true
        {
            return Ok(None);
        }
        let generation_matches = document["generation"] == account["generation"];
        Ok(Some(SessionCandidate {
            identity,
            document,
            key: key.to_owned(),
            state: crate::access_records::state(&account),
            generation_matches,
            auth_epoch,
            record,
        }))
    }

    /// Consumers retain resource authorization; Accounts owns browser-session liveness.
    pub async fn session_reference_live(
        &self,
        key: &str,
        user: &str,
        generation: &Value,
        now: u64,
    ) -> Result<bool> {
        let Some(candidate) = self.inspect_session_key(key, now).await? else {
            return Ok(false);
        };
        if candidate.identity.user_id != user || &candidate.document["generation"] != generation {
            return Ok(false);
        }
        Ok(candidate.finish().is_some())
    }
    pub async fn authenticate_session(
        &self,
        token: &str,
        now: u64,
        mode: RegistrationMode,
    ) -> Result<Option<AuthenticatedSession>> {
        let Some(candidate) = self.inspect_session(token, now).await? else {
            return Ok(None);
        };
        if candidate.guest() && mode != RegistrationMode::Open {
            return Ok(None);
        }
        Ok(candidate.finish())
    }
}

impl<C: RecordCodec> Repository<'_, C> {
    pub(crate) async fn auth_document(&self, token: &str) -> Result<Option<Value>> {
        if token.len() != 36 {
            return Ok(None);
        }
        let blob: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(auth_key(token))
            .fetch_optional(self.app_pool)
            .await?;
        blob.as_ref().map(|blob| self.unseal(blob)).transpose()
    }
}

impl RegistrationMode {
    pub const fn access_mode_name(self) -> &'static str {
        match self {
            Self::Open => "public_registered",
            Self::Restricted => "admin_approval",
        }
    }
}

impl<C: RecordCodec> Repository<'_, C> {
    /// The host supplies its inspected generation; never refresh a revoked browser authority.
    pub async fn issue_trusted_browser_session(
        &self,
        user: &str,
        expected_generation: &str,
        now: u64,
        user_agent: &str,
        duration: u64,
    ) -> Result<crate::IssuedSession> {
        let account = self
            .account_by_id(user)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Caller unavailable"))?;
        let identity = crate::identity(&account)?;
        anyhow::ensure!(
            identity.user_id == user
                && identity.status == AccountStatus::Active
                && account["is_approved"] == true
                && account["generation"] == expected_generation,
            "Caller unavailable"
        );
        self.issue_versioned_session(&identity, expected_generation, now, user_agent, duration)
            .await
    }
    /// Existing one-time grant repair: Access supplies the role and target tier; identity is unchanged.
    pub async fn initialize_tier_assignment_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        role: &str,
        tier: &str,
    ) -> Result<()> {
        anyhow::ensure!(
            crate::valid_id(role) && crate::valid_id(tier),
            "Invalid initial account assignment"
        );
        let rows: Vec<(String, Vec<u8>)> =
            sqlx::query_as("SELECT user_id,value FROM user_accounts")
                .fetch_all(&mut **tx)
                .await?;
        for (id, blob) in rows {
            let mut account = self.unseal(&blob)?;
            if account["role"] != role || account["tier_id"] == tier {
                continue;
            }
            account["tier_id"] = json!(tier);
            account["access_revision"] = json!(account["access_revision"]
                .as_u64()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("Account revision overflow"))?);
            crate::account_rows::update(tx, &id, None, &self.seal(&account)?).await?;
        }
        Ok(())
    }
}
