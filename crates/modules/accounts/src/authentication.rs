use crate::{AccountStatus, Identity, RecordCodec, Repository};
use anyhow::{anyhow, Result};
use argon2::{password_hash::SaltString, Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::Mutex;

/// The host shares one budget across password login, public registration and guests.
#[derive(Clone, Default)]
pub struct LoginAttempts(Arc<Mutex<Vec<u64>>>);
impl LoginAttempts {
    pub async fn admit(&self, now: u64) -> bool {
        let mut attempts = self.0.lock().await;
        attempts.retain(|t| now.saturating_sub(*t) < 60);
        if attempts.len() >= 5 {
            return false;
        }
        attempts.push(now);
        true
    }
}
pub async fn hash_password(password: String) -> Result<String> {
    tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
            .map(|v| v.to_string())
            .map_err(|_| anyhow!("Password hashing failed"))
    })
    .await?
}
pub async fn verify_password(hash: String, password: String) -> Result<bool> {
    Ok(tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash).ok().is_some_and(|h| {
            Argon2::default()
                .verify_password(password.as_bytes(), &h)
                .is_ok()
        })
    })
    .await?)
}
pub fn message_api_token_hash(token: &str) -> Result<Option<[u8; 32]>> {
    if token.is_empty() {
        return Ok(None);
    }
    anyhow::ensure!(
        (32..=256).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_graphic()),
        "V3_MESSAGE_API_TOKEN must be 32-256 printable non-space ASCII characters"
    );
    Ok(Some(Sha256::digest(token.as_bytes()).into()))
}
pub fn message_api_authorized(expected: Option<&[u8; 32]>, authorization: Option<&str>) -> bool {
    let Some(expected) = expected else {
        return false;
    };
    let Some(token) = authorization.and_then(|h| h.strip_prefix("Bearer ")) else {
        return false;
    };
    if token.len() > 256 {
        return false;
    }
    let actual: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    expected
        .iter()
        .zip(actual.iter())
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}
impl<C: RecordCodec> Repository<'_, C> {
    /// Verifies local password credentials only; resource authorization belongs to Access.
    pub async fn verify_credentials(
        &self,
        username: &str,
        password: String,
    ) -> Result<Option<VerifiedCredentials>> {
        let Some(account) = self.account_by_username(username).await? else {
            return Ok(None);
        };
        if account["role"] == "guest" {
            return Ok(None);
        }
        let valid = verify_password(
            account["password_hash"].as_str().unwrap_or("").into(),
            password,
        )
        .await?;
        let Ok(identity) = crate::identity(&account) else {
            return Ok(None);
        };
        if identity.username != username
            || account["is_approved"] == false
            || identity.status != AccountStatus::Active
            || !valid
        {
            return Ok(None);
        }
        let Some(generation) = account["generation"]
            .as_str()
            .filter(|value| !value.is_empty())
        else {
            return Ok(None);
        };
        Ok(Some(VerifiedCredentials {
            identity,
            generation: generation.into(),
            state: crate::access_records::state(&account),
        }))
    }
}

/// Opaque verified identity/version; never serialize a password proof into durable state.
pub struct VerifiedCredentials {
    identity: Identity,
    generation: String,
    state: serde_json::Value,
}
impl VerifiedCredentials {
    pub fn identity(&self) -> &Identity {
        &self.identity
    }
    pub fn account_state(&self) -> &serde_json::Value {
        &self.state
    }
    pub(crate) fn generation(&self) -> &str {
        &self.generation
    }
}
