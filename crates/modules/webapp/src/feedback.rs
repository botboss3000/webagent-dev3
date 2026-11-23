//! Send feedback: the switch, relay address and submission rules for reports the app
//! forwards to the WebAgent feedback relay, which files each one as an issue in the
//! project's private feedback repository. The relay never sees the caller's account.
use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};

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
}

/// Feedback switch and relay address; a missing section means on, with the upstream relay.
pub fn settings(app: &Value) -> Result<Settings> {
    let Some(section) = app.get("feedback") else {
        return Ok(Settings {
            enabled: true,
            relay_url: DEFAULT_RELAY_URL.into(),
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
    Ok(Settings { enabled, relay_url })
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
                relay_url: DEFAULT_RELAY_URL.into()
            }
        );
        let off = settings(&json!({"feedback":{"enabled":false}})).unwrap();
        assert!(!off.enabled);
        for bad in [
            json!({"feedback":true}),
            json!({"feedback":{"enabled":"no"}}),
            json!({"feedback":{"relay_url":"http://relay.example/submit"}}),
            json!({"feedback":{"relay_url":concat!("https://", "fixture", ":", "value", "@relay.example/submit")}}),
            json!({"feedback":{"relay_url":7}}),
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
}
