//! Send feedback: the switch, relay address and submission rules for reports the app
//! forwards to the WebAgent feedback relay, which files each one as an issue in the
//! project's private feedback repository. The relay never sees the caller's account.
use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};

/// The upstream WebAgent relay (a Cloudflare Worker) that every installation uses by default.
pub const DEFAULT_RELAY_URL: &str = "https://webagent-feedback-relay.botboss.workers.dev/submit";
const MAX_BODY_CHARS: usize = 8000;
const MAX_EMAIL_CHARS: usize = 254;

/// One report from the account menu's feedback form.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Submission {
    #[serde(rename = "type")]
    pub kind: String,
    pub body: String,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Debug, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    pub relay_url: String,
    /// Outbound attempts one account may make in any hour.
    pub per_account_per_hour: usize,
    /// Outbound attempts the whole installation may make in any day.
    pub per_installation_per_day: usize,
}

const DEFAULT_PER_ACCOUNT_PER_HOUR: usize = 5;
const DEFAULT_PER_INSTALLATION_PER_DAY: usize = 50;

/// Read one limit: a whole number from 1 to `max`, or `default` when missing.
fn limit(section: &Value, key: &str, default: usize, max: usize) -> Result<usize> {
    match section.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .filter(|n| (1..=max as u64).contains(n))
            .map(|n| n as usize)
            .ok_or_else(|| anyhow!("Feedback {key} must be a whole number from 1 to {max}")),
    }
}

/// Feedback switch and relay address; a missing section means on, with the upstream relay.
pub fn settings(app: &Value) -> Result<Settings> {
    let Some(section) = app.get("feedback") else {
        return Ok(Settings {
            enabled: true,
            relay_url: DEFAULT_RELAY_URL.into(),
            per_account_per_hour: DEFAULT_PER_ACCOUNT_PER_HOUR,
            per_installation_per_day: DEFAULT_PER_INSTALLATION_PER_DAY,
        });
    };
    if !section.is_object() {
        return Err(anyhow!("Invalid feedback settings"));
    }
    let enabled = match section.get("enabled") {
        None => true,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| anyhow!("Feedback enabled must be a boolean"))?,
    };
    let relay_url = match section.get("relay_url") {
        None => DEFAULT_RELAY_URL.to_owned(),
        Some(value) => value
            .as_str()
            .filter(|url| url.starts_with("https://") && url.len() <= 2048 && !url.contains('@'))
            .ok_or_else(|| anyhow!("Feedback relay_url must be an https URL without credentials"))?
            .to_owned(),
    };
    Ok(Settings {
        enabled,
        relay_url,
        per_account_per_hour: limit(
            section,
            "per_account_per_hour",
            DEFAULT_PER_ACCOUNT_PER_HOUR,
            100,
        )?,
        per_installation_per_day: limit(
            section,
            "per_installation_per_day",
            DEFAULT_PER_INSTALLATION_PER_DAY,
            10_000,
        )?,
    })
}

const HOUR: u64 = 3_600;
const DAY: u64 = 86_400;
/// Most accounts the limiter remembers at once; beyond it the longest-idle account is forgotten.
const MAX_ACCOUNTS: usize = 10_000;

/// Counts outbound relay attempts per account (per hour) and for the installation (per day).
/// It lives in memory, so counts start again when the process restarts.
#[derive(Default)]
pub struct Limiter {
    accounts: HashMap<String, VecDeque<u64>>,
    installation: VecDeque<u64>,
}

impl Limiter {
    /// Charge one outbound attempt for `account` at `now` (Unix seconds), or refuse when
    /// either limit is already reached. A refusal charges nothing.
    pub fn try_charge(&mut self, account: &str, now: u64, settings: &Settings) -> bool {
        let expire = |times: &mut VecDeque<u64>, window: u64| {
            while times
                .front()
                .is_some_and(|t| now.saturating_sub(*t) >= window)
            {
                times.pop_front();
            }
        };
        expire(&mut self.installation, DAY);
        let used = self.accounts.get_mut(account).map_or(0, |times| {
            expire(times, HOUR);
            times.len()
        });
        if used == 0 {
            self.accounts.remove(account);
        }
        if used >= settings.per_account_per_hour
            || self.installation.len() >= settings.per_installation_per_day
        {
            return false;
        }
        if used == 0 && self.accounts.len() >= MAX_ACCOUNTS {
            self.make_room(now);
        }
        self.accounts
            .entry(account.to_owned())
            .or_default()
            .push_back(now);
        self.installation.push_back(now);
        true
    }

    /// Forget accounts with no attempt in the last hour; if none, forget the longest-idle one.
    fn make_room(&mut self, now: u64) {
        self.accounts
            .retain(|_, times| times.back().is_some_and(|t| now.saturating_sub(*t) < HOUR));
        if self.accounts.len() >= MAX_ACCOUNTS {
            if let Some(idle) = self
                .accounts
                .iter()
                .min_by_key(|(_, times)| times.back().copied())
                .map(|(account, _)| account.clone())
            {
                self.accounts.remove(&idle);
            }
        }
    }
}

/// Check one submission and build the relay's request body, with no account identity in it.
pub fn relay_payload(
    submission: &Submission,
    installation_id: &str,
    app_version: &str,
) -> Result<Value, &'static str> {
    if !matches!(submission.kind.as_str(), "bug" | "feature" | "message") {
        return Err("Feedback type must be bug, feature or message");
    }
    let body = submission.body.trim();
    if body.is_empty() {
        return Err("Describe your feedback before sending");
    }
    if body.chars().count() > MAX_BODY_CHARS {
        return Err("Feedback is limited to 8000 characters");
    }
    let email = submission
        .email
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty());
    if let Some(email) = email {
        let valid = email.chars().count() <= MAX_EMAIL_CHARS
            && email
                .rsplit_once('@')
                .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'));
        if !valid {
            return Err("Enter a valid email address or leave it empty");
        }
    }
    Ok(json!({
        "type": submission.kind,
        "body": body,
        "email": email,
        "installation_id": installation_id,
        "app_version": app_version,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn submission(kind: &str, body: &str, email: Option<&str>) -> Submission {
        Submission {
            kind: kind.into(),
            body: body.into(),
            email: email.map(Into::into),
        }
    }

    #[test]
    fn settings_default_on_with_upstream_relay_and_reject_bad_values() {
        assert_eq!(
            settings(&json!({})).unwrap(),
            Settings {
                enabled: true,
                relay_url: DEFAULT_RELAY_URL.into(),
                per_account_per_hour: 5,
                per_installation_per_day: 50,
            }
        );
        let custom = settings(
            &json!({"feedback":{"per_account_per_hour":100,"per_installation_per_day":10000}}),
        )
        .unwrap();
        assert_eq!(
            (custom.per_account_per_hour, custom.per_installation_per_day),
            (100, 10_000)
        );
        let off = settings(&json!({"feedback":{"enabled":false}})).unwrap();
        assert!(!off.enabled);
        for bad in [
            json!({"feedback":true}),
            json!({"feedback":{"enabled":"no"}}),
            json!({"feedback":{"relay_url":"http://relay.example/submit"}}),
            json!({"feedback":{"relay_url":concat!("https://", "fixture", ":", "value", "@relay.example/submit")}}),
            json!({"feedback":{"relay_url":7}}),
            json!({"feedback":{"per_account_per_hour":0}}),
            json!({"feedback":{"per_account_per_hour":101}}),
            json!({"feedback":{"per_account_per_hour":-1}}),
            json!({"feedback":{"per_account_per_hour":2.5}}),
            json!({"feedback":{"per_installation_per_day":0}}),
            json!({"feedback":{"per_installation_per_day":10001}}),
            json!({"feedback":{"per_installation_per_day":"50"}}),
        ] {
            assert!(settings(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn relay_payload_validates_and_carries_no_account() {
        let payload = relay_payload(
            &submission("bug", "  broken  ", Some(" a@b.co ")),
            "inst",
            "0.1.0",
        )
        .unwrap();
        assert_eq!(
            payload,
            json!({"type":"bug","body":"broken","email":"a@b.co","installation_id":"inst","app_version":"0.1.0"})
        );
        assert_eq!(
            relay_payload(&submission("message", "hi", Some("")), "i", "v").unwrap()["email"],
            Value::Null
        );
        for bad in [
            submission("praise", "hi", None),
            submission("bug", "   ", None),
            submission("bug", &"x".repeat(MAX_BODY_CHARS + 1), None),
            submission("bug", "hi", Some("nobody")),
            submission("bug", "hi", Some("@b.co")),
            submission("bug", "hi", Some("a@localhost")),
        ] {
            assert!(relay_payload(&bad, "i", "v").is_err(), "{bad:?}");
        }
        assert!(relay_payload(
            &submission("feature", &"é".repeat(MAX_BODY_CHARS), None),
            "i",
            "v"
        )
        .is_ok());
    }

    #[test]
    fn limiter_counts_per_account_and_installation_and_windows_expire() {
        let settings = settings(&json!({})).unwrap();
        let mut limiter = Limiter::default();
        for _ in 0..5 {
            assert!(limiter.try_charge("a", 1_000, &settings));
        }
        assert!(!limiter.try_charge("a", 1_000, &settings), "6th in an hour");
        assert!(
            limiter.try_charge("b", 1_000, &settings),
            "other accounts still send"
        );
        assert!(
            limiter.try_charge("a", 1_000 + HOUR, &settings),
            "the hour has passed"
        );
        // 7 charged so far today; fill the installation's day.
        for n in 0..43 {
            assert!(limiter.try_charge(&format!("u{n}"), 2_000, &settings));
        }
        assert!(
            !limiter.try_charge("fresh", 2_000, &settings),
            "51st in a day"
        );
        assert!(
            !limiter.accounts.contains_key("fresh"),
            "a refusal charges nothing"
        );
        assert!(
            limiter.try_charge("fresh", 1_000 + DAY, &settings),
            "the day has passed"
        );
    }

    #[test]
    fn limiter_forgets_idle_accounts_and_stays_bounded() {
        let settings = Settings {
            per_installation_per_day: 10_000,
            ..settings(&json!({})).unwrap()
        };
        let mut limiter = Limiter::default();
        for n in 0..MAX_ACCOUNTS {
            limiter
                .accounts
                .insert(format!("old{n}"), VecDeque::from([n as u64]));
        }
        limiter.accounts.insert("idle".into(), VecDeque::new());
        limiter.try_charge("idle", 5 * HOUR, &settings);
        assert_eq!(
            limiter.accounts.len(),
            1,
            "stale and empty entries are dropped"
        );
        for n in 0..MAX_ACCOUNTS {
            limiter.accounts.insert(
                format!("busy{n}"),
                VecDeque::from([5 * HOUR + 1 + n as u64]),
            );
        }
        assert!(limiter.try_charge("new", 5 * HOUR + 2, &settings));
        assert!(limiter.accounts.len() <= MAX_ACCOUNTS + 1);
        assert!(
            !limiter.accounts.contains_key("idle"),
            "the longest-idle account goes first"
        );
    }
}
