//! The outgoing billing connection's rules that need no socket (plus tier spec 3.9, protocol
//! `work/plus-tier/billing-protocol.md` v1): its live configuration, the notice kinds the
//! billing service may name and the app's own sentence for each, and field checks. The
//! connection itself is Unix-only host code.
use anyhow::{bail, Result};
use serde::Deserialize;

/// Live billing connection settings, relative to the checkout.
pub const LIVE: &str = "config/access/billing.json";
/// Longest message line, newline included.
pub const MAX_LINE: usize = 65_536;
/// The protocol version this app speaks.
pub const PROTOCOL: u64 = 1;

/// Where the billing service's socket is and which system account must own it. No path: the
/// app runs without billing and every account keeps what it has.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Absolute path of the service's socket file; null turns the connection off.
    #[serde(default)]
    pub socket_path: Option<String>,
    /// The service's system account id as the app sees it (checked on every connection).
    #[serde(default)]
    pub service_uid: Option<u32>,
    /// Seconds a request waits for its reply.
    #[serde(default = "default_timeout")]
    pub request_timeout_seconds: u64,
}

fn default_timeout() -> u64 {
    10
}

impl Config {
    /// Parse and check the live file's bytes.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let config: Self = serde_json::from_slice(bytes)?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        if !(1..=60).contains(&self.request_timeout_seconds) {
            bail!("request_timeout_seconds must be 1..=60");
        }
        if let Some(path) = &self.socket_path {
            // A Unix socket path holds at most 107 bytes.
            if !path.starts_with('/') || path.len() > 107 || path.contains('\0') {
                bail!("socket_path must be an absolute path of at most 107 bytes");
            }
            if self.service_uid.is_none() {
                bail!("service_uid is required with a socket_path");
            }
        }
        Ok(())
    }
}

/// The app's own sentence for each notice kind the service may name (no text crosses the
/// connection).
#[must_use]
pub fn notice_text(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "allowance_80" => "You have used 80% of this period's model allowance.",
        "allowance_100" => "You have used this period's model allowance; catalog models pause until it resets or you add credit.",
        "daily_cap_80" => "You have used 80% of today's spending cap.",
        "daily_cap_100" => "You reached today's spending cap; catalog models resume after midnight UTC.",
        "credit_low" => "Your model credit is running low.",
        "credit_empty" => "Your model credit is used up.",
        "renewal_failed" => "A payment for your plan failed. Your plan stays active for a short grace period; update your billing details.",
        "plus_ended" => "Your Plus plan has ended; your account is on Free. Your settings are kept.",
        "payment_refunded" => "A payment was refunded; what it bought has been removed.",
        _ => return None,
    })
}

/// A service reference or message id: 1 to `max` characters of letters, digits and `._:-`.
#[must_use]
pub fn valid_token(value: &str, max: usize) -> bool {
    (1..=max).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

/// A one-time hosted-page link the app may hand to its user: https, at most 2,048 bytes.
#[must_use]
pub fn valid_link(url: &str) -> bool {
    url.len() <= 2_048
        && url.starts_with("https://")
        && url.len() > "https://".len()
        && !url
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b' ')
}

/// The plan summary fields kept (numbers, a status word and a flag); anything else is dropped.
#[must_use]
pub fn summary_fields(summary: &serde_json::Value) -> Option<serde_json::Value> {
    let object = summary.as_object()?;
    let mut kept = serde_json::Map::new();
    for field in [
        "allowance_cents",
        "allowance_used_cents",
        "credit_cents",
        "daily_cap_cents",
        "daily_used_cents",
        "next_reset",
        "plan_ends",
    ] {
        match object.get(field) {
            None => {}
            Some(value) if value.is_null() || value.as_u64().is_some() => {
                kept.insert(field.into(), value.clone());
            }
            Some(_) => return None,
        }
    }
    if let Some(source) = object.get("source") {
        if !matches!(source.as_str(), Some("paid" | "grant" | "oneoff" | "none")) {
            return None;
        }
        kept.insert("source".into(), source.clone());
    }
    if let Some(flag) = object.get("cancel_at_period_end") {
        kept.insert(
            "cancel_at_period_end".into(),
            serde_json::json!(flag.as_bool()?),
        );
    }
    Some(serde_json::Value::Object(kept))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn config_needs_an_absolute_path_and_the_services_account() {
        assert_eq!(
            Config::parse(b"{}").unwrap(),
            Config {
                request_timeout_seconds: 10,
                ..Config::default()
            }
        );
        Config::parse(br#"{"socket_path":"/run/webagent-billing/billing.sock","service_uid":998}"#)
            .unwrap();
        for bad in [
            &br#"{"socket_path":"/run/b.sock"}"#[..],
            br#"{"socket_path":"run/b.sock","service_uid":1}"#,
            br#"{"request_timeout_seconds":0}"#,
            br#"{"other":1}"#,
        ] {
            assert!(Config::parse(bad).is_err());
        }
    }

    #[test]
    fn only_fixed_kinds_tokens_links_and_numbers_cross() {
        assert!(notice_text("renewal_failed").is_some());
        assert!(notice_text("anything else").is_none());
        assert!(valid_token("stripe:evt_1.a-b", 128));
        assert!(
            !valid_token("", 128)
                && !valid_token("a b", 128)
                && !valid_token(&"a".repeat(129), 128)
        );
        assert!(valid_link("https://checkout.example/c/pay"));
        for bad in [
            "http://x.example",
            "https://",
            "javascript:alert(1)",
            "https://a b",
        ] {
            assert!(!valid_link(bad), "{bad}");
        }
        assert_eq!(
            summary_fields(
                &json!({"allowance_cents":2000,"plan_ends":null,"source":"paid","note":"x"})
            ),
            Some(json!({"allowance_cents":2000,"plan_ends":null,"source":"paid"}))
        );
        assert!(summary_fields(&json!({"allowance_cents":-1})).is_none());
        assert!(summary_fields(&json!({"source":"gift"})).is_none());
    }
}
