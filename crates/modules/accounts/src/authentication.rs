use crate::{AccountStatus, Identity, RecordCodec, Repository};
use anyhow::{anyhow, Result};
use argon2::{password_hash::SaltString, Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use std::sync::Arc;

static PASSWORD_WORK: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
static PASSWORD_PENDING: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> =
    std::sync::OnceLock::new();
async fn password_work<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    let pending = PASSWORD_PENDING
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1024)))
        .clone()
        .try_acquire_owned()?;
    let permit = PASSWORD_WORK
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(8)))
        .clone()
        .acquire_owned()
        .await?;
    // The blocking closure owns its permit even if the HTTP request is cancelled.
    Ok(tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _pending = pending;
        work()
    })
    .await?)
}
pub async fn hash_password(password: String) -> Result<String> {
    password_work(move || {
        Argon2::default()
            .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
            .map(|v| v.to_string())
            .map_err(|_| anyhow!("Password hashing failed"))
    })
    .await?
}
pub async fn verify_password(hash: String, password: String) -> Result<bool> {
    password_work(move || {
        PasswordHash::new(&hash).ok().is_some_and(|h| {
            Argon2::default()
                .verify_password(password.as_bytes(), &h)
                .is_ok()
        })
    })
    .await
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

#[cfg(test)]
mod password_work_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[tokio::test]
    async fn blocking_password_work_is_bounded() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut jobs = Vec::new();
        for _ in 0..24 {
            let active = active.clone();
            let peak = peak.clone();
            jobs.push(tokio::spawn(async move {
                password_work(move || {
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(count, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    active.fetch_sub(1, Ordering::SeqCst);
                })
                .await
                .unwrap();
            }));
        }
        for job in jobs {
            job.await.unwrap();
        }
        assert!(peak.load(Ordering::SeqCst) <= 8);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }
}
