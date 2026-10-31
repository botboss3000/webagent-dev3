//! Process-wide caps on concurrent event-read connections, per credential, per user and in
//! total. A [`Connection`] holds one slot and releases it when dropped.
use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Most concurrent connections one credential may hold.
pub const MAX_PER_CREDENTIAL: usize = 4;
/// Most concurrent connections one user may hold.
pub const MAX_PER_USER: usize = 8;
/// Most concurrent connections in the process.
pub const MAX_TOTAL: usize = 64;

#[derive(Default)]
struct Counts {
    credentials: HashMap<String, usize>,
    users: HashMap<String, usize>,
    total: usize,
}

static COUNTS: OnceLock<Mutex<Counts>> = OnceLock::new();

fn counts() -> MutexGuard<'static, Counts> {
    COUNTS
        .get_or_init(Default::default)
        .lock()
        .expect("connection counts are never held across a panic")
}

/// Why a connection slot was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcquireError {
    /// The caller presented no credential.
    MissingCredential,
    /// A per-credential, per-user or total cap is reached.
    LimitReached,
}

impl fmt::Display for AcquireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MissingCredential => "Missing event credential",
            Self::LimitReached => "Event connection limit reached",
        })
    }
}

impl std::error::Error for AcquireError {}

/// One held event-read connection slot.
#[derive(Debug)]
pub struct Connection {
    credential: String,
    user: String,
}

impl Connection {
    /// Take a slot for `credential` owned by `user`.
    pub fn acquire(credential: &str, user: &str) -> Result<Self, AcquireError> {
        if credential.is_empty() {
            return Err(AcquireError::MissingCredential);
        }
        let mut counts = counts();
        if counts.credentials.get(credential).copied().unwrap_or(0) >= MAX_PER_CREDENTIAL
            || counts.users.get(user).copied().unwrap_or(0) >= MAX_PER_USER
            || counts.total >= MAX_TOTAL
        {
            return Err(AcquireError::LimitReached);
        }
        *counts.credentials.entry(credential.to_owned()).or_default() += 1;
        *counts.users.entry(user.to_owned()).or_default() += 1;
        counts.total += 1;
        Ok(Self {
            credential: credential.to_owned(),
            user: user.to_owned(),
        })
    }
}

/// Decrement `key`'s count in `map`, dropping it at zero.
fn release(map: &mut HashMap<String, usize>, key: &str) {
    if let Some(count) = map.get_mut(key) {
        *count -= 1;
        if *count == 0 {
            map.remove(key);
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let mut counts = counts();
        release(&mut counts.credentials, &self.credential);
        release(&mut counts.users, &self.user);
        counts.total -= 1;
    }
}

#[cfg(test)]
mod tests {
    // The workspace has no clippy.toml with `allow-unwrap-in-tests` yet.
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn fresh() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    #[test]
    fn credential_cap_releases_on_drop() {
        let (credential, user) = (fresh(), fresh());
        let mut held: Vec<Connection> = (0..MAX_PER_CREDENTIAL)
            .map(|_| Connection::acquire(&credential, &user).unwrap())
            .collect();
        assert_eq!(
            Connection::acquire(&credential, &user).unwrap_err(),
            AcquireError::LimitReached
        );
        held.pop();
        assert!(Connection::acquire(&credential, &user).is_ok());
        drop(held);
        // Every slot came back, so a full set is available again.
        let again: Vec<Connection> = (0..MAX_PER_CREDENTIAL)
            .map(|_| Connection::acquire(&credential, &user).unwrap())
            .collect();
        assert_eq!(again.len(), MAX_PER_CREDENTIAL);
    }

    #[test]
    fn user_cap_spans_credentials_and_missing_credential_is_rejected() {
        let user = fresh();
        let mut held = Vec::new();
        for _ in 0..MAX_PER_USER / MAX_PER_CREDENTIAL {
            let credential = fresh();
            for _ in 0..MAX_PER_CREDENTIAL {
                held.push(Connection::acquire(&credential, &user).unwrap());
            }
        }
        let error = Connection::acquire(&fresh(), &user).unwrap_err();
        assert_eq!(error, AcquireError::LimitReached);
        assert_eq!(error.to_string(), "Event connection limit reached");
        let missing = Connection::acquire("", &user).unwrap_err();
        assert_eq!(missing.to_string(), "Missing event credential");
        drop(held);
        assert!(Connection::acquire(&fresh(), &user).is_ok());
    }
}
