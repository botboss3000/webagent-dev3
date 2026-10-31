//! Event-reader credentials: hashed, sealed, revocable bearer tokens limited to the event
//! read routes. They are deliberately separate from the message-submission bearer API.
use serde::Deserialize;
use serde_json::json;

/// Document-key prefix of a stored reader credential.
pub const KEY_PREFIX: &str = "notification_reader:";
/// Most reader credentials an installation may hold.
pub const MAX_READERS: i64 = 64;
/// Shortest reader lifetime, in seconds.
pub const MIN_EXPIRES_IN: u64 = 60;
/// Longest reader lifetime, in seconds (90 days).
pub const MAX_EXPIRES_IN: u64 = 90 * 24 * 60 * 60;
/// The only scope a reader credential grants.
pub const SCOPE: &str = "events:read";

/// Request to issue a reader credential.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReaderBody {
    /// Lifetime in seconds, from [`MIN_EXPIRES_IN`] to [`MAX_EXPIRES_IN`].
    pub expires_in: u64,
}

/// Whether `expires_in` is an allowed reader lifetime.
#[must_use]
pub fn valid_lifetime(expires_in: u64) -> bool {
    (MIN_EXPIRES_IN..=MAX_EXPIRES_IN).contains(&expires_in)
}

/// Whether `path` is one of the event read routes a reader credential may call.
#[must_use]
pub fn reader_route(path: &str) -> bool {
    matches!(
        path,
        "/api/v3/admin/events"
            | "/api/v3/admin/events/stream"
            | "/admin/events"
            | "/admin/events/stream"
    )
}

/// Whether `value` has the shape of a reader token or reader ID: 64 hexadecimal digits.
#[must_use]
pub fn hex64(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// A fresh reader token: 64 random hexadecimal digits.
#[must_use]
pub fn new_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// The reader ID of `token`; only this digest is stored.
#[must_use]
pub fn reader_id(token: &str) -> String {
    webagent_configuration::digest(&json!(token))
}

/// The document key of reader `id`.
#[must_use]
pub fn key(id: &str) -> String {
    format!("{KEY_PREFIX}{id}")
}

#[cfg(test)]
mod tests {
    // The workspace has no clippy.toml with `allow-unwrap-in-tests` yet.
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn lifetimes_routes_and_token_shape() {
        assert!(!valid_lifetime(59));
        assert!(valid_lifetime(60));
        assert!(valid_lifetime(90 * 24 * 60 * 60));
        assert!(!valid_lifetime(90 * 24 * 60 * 60 + 1));
        for path in ["/api/v3/admin/events", "/admin/events/stream"] {
            assert!(reader_route(path));
        }
        for path in [
            "/api/v3/sessions",
            "/api/v3/admin/events/",
            "/admin/events/readers",
        ] {
            assert!(!reader_route(path));
        }
        let token = new_token();
        assert!(hex64(&token));
        assert_ne!(token, new_token());
        assert!(!hex64(&token[1..]));
        assert!(!hex64(&format!("{}g", &token[1..])));
        let id = reader_id(&token);
        assert!(hex64(&id));
        assert_eq!(id, reader_id(&token));
        assert_eq!(key(&id), format!("notification_reader:{id}"));
    }

    #[test]
    fn reader_body_rejects_unknown_fields() {
        let body: ReaderBody = serde_json::from_str(r#"{"expires_in":3600}"#).unwrap();
        assert_eq!(body.expires_in, 3600);
        assert!(serde_json::from_str::<ReaderBody>(r#"{"expires_in":60,"scope":"all"}"#).is_err());
    }
}
