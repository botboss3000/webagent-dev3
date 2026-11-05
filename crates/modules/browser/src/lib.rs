//! Browsers the agent drives: the user's own Chrome through the WebAgent extension
//! (Browser Extension), and a private server browser (Browser Control).
//!
//! The extension links to one WebAgent account and long-polls for queued actions; the
//! agent's `browser_extension_action` tool queues one action and waits for its result. This crate
//! holds the pure rules: which sites an agent may control, action admission, link codes
//! and the queued call's lifecycle. Browser Control runs in the worker's browser package;
//! this crate holds its site-policy parsing. The root adapter (`host/browser.rs`) owns
//! storage, routes, authority and tool dispatch.
use anyhow::{bail, Result};
use serde_json::Value;

/// Tools served by this module: the extension's, then Browser Control's.
pub const TOOLS: &[&str] = &[
    "browser_extension_status",
    "browser_extension_action",
    "browser_read",
    "browser_input",
    "browser_record",
];
/// Browser Control's tools, run in the worker's private browser.
pub const SERVER_TOOLS: &[&str] = &["browser_read", "browser_input", "browser_record"];
/// The ability whose per-agent settings hold the extension's site allowlist.
pub const ABILITY: &str = "browser_extension";
/// The ability whose per-agent settings hold Browser Control's allow and block lists.
pub const SERVER_ABILITY: &str = "browser_control";
/// Most hosts in one Browser Control site list.
pub const SERVER_SITES_MAX: usize = 200;

/// A queued action nobody fetches within this window fails as not started.
pub const OFFLINE_SECS: u64 = 30;
/// A device that polled within this window is online.
pub const RECENT_SECS: u64 = 60;
/// A fetched action is offered this long; unacknowledged, it returns to the queue.
pub const LEASE_SECS: u64 = 30;
/// An acknowledged action waits this long for its result before it is uncertain.
pub const ANSWER_SECS: u64 = 120;
/// Extension long-poll window.
pub const POLL_SECS: u64 = 25;
/// A link code is valid for this long, once.
pub const LINK_SECS: u64 = 120;

/// The longest allowlist setting accepted, in characters.
pub const SITES_MAX_CHARS: usize = 2000;

/// Hosts from the agent's `sites` setting: separated by commas, spaces or new lines, with
/// any pasted scheme, path or port dropped. `*.` prefixes are accepted and mean the same
/// as the bare host, which already covers its subdomains.
pub fn sites(setting: &str) -> Vec<String> {
    setting
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter_map(|entry| {
            let entry = entry.trim().to_ascii_lowercase();
            let entry = entry
                .split_once("://")
                .map_or(entry.as_str(), |(_, rest)| rest);
            let host = entry.split(['/', ':', '?', '#']).next().unwrap_or("");
            let host = host.trim_start_matches("*.").trim_matches('.');
            ((!host.is_empty() && host.contains('.')) || host == "localhost")
                .then(|| host.to_owned())
        })
        .collect()
}

/// Hosts from a Browser Control `allow_sites` or `block_sites` setting, limited to the
/// characters and count the browser package accepts; anything else is dropped.
pub fn server_sites(setting: &str) -> Vec<String> {
    sites(setting)
        .into_iter()
        .filter(|host| {
            host.len() <= 253
                && host
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
        })
        .take(SERVER_SITES_MAX)
        .collect()
}

/// Whether `url` is an http(s) address on one of `sites` or their subdomains.
pub fn site_allowed(url: &str, sites: &[String]) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = parsed.host_str().map(str::to_ascii_lowercase) else {
        return false;
    };
    sites
        .iter()
        .any(|site| host == *site || host.ends_with(&format!(".{site}")))
}

/// Admit one `browser_extension_action` call: required fields per action and, for actions that
/// load a page, an allowlisted address. The extension checks the tab's current site again
/// before acting, since a page can redirect anywhere.
pub fn check_action(args: &Value, sites: &[String]) -> Result<()> {
    let action = args["action"].as_str().unwrap_or("");
    let has = |field: &str| !args[field].is_null();
    match action {
        "open" | "navigate" => {
            let url = args["url"].as_str().unwrap_or("");
            if !site_allowed(url, sites) {
                bail!("{url} is not on this agent's browser site list; an operator adds sites in the Browser Extension ability settings");
            }
        }
        "click" if !has("ref") => bail!("click needs ref from your last read"),
        "type" if !has("ref") || !args["text"].is_string() => {
            bail!("type needs ref from your last read and text")
        }
        "press" if !args["key"].is_string() => bail!("press needs key"),
        "read" | "click" | "type" | "press" | "scroll" | "tabs" | "close" => {}
        _ => bail!("Unknown browser action {action}"),
    }
    Ok(())
}

/// A queued call no device holds: never fetched, or offered and left unacknowledged.
pub fn open(call: &Value, at: u64) -> bool {
    match call["state"].as_str() {
        Some("pending") => true,
        Some("offered") => at > since(call, "offered") + LEASE_SECS,
        _ => false,
    }
}

/// A call the waiting tool has already given up on, or a finished one kept past the window
/// in which a retried tool call could still pick up its result.
pub fn stale(call: &Value, at: u64) -> bool {
    match call["state"].as_str() {
        Some("pending") => at > since(call, "created") + OFFLINE_SECS,
        Some("offered") => at > since(call, "offered") + LEASE_SECS + OFFLINE_SECS,
        Some("claimed") => at > since(call, "claimed") + ANSWER_SECS,
        _ => at > since(call, "created") + 10 * ANSWER_SECS,
    }
}

fn since(call: &Value, field: &str) -> u64 {
    call[field].as_u64().unwrap_or(0)
}

/// Whether a device last seen at `seen` counts as online at `at`.
pub fn recently_seen(seen: Option<u64>, at: u64) -> bool {
    seen.is_some_and(|seen| at <= seen + RECENT_SECS)
}

/// A one-time link code. The widget hands it to the extension programmatically, so it is
/// long rather than typeable.
pub fn link_code() -> String {
    format!(
        "blc_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// A device secret, returned once to the extension.
pub fn device_secret() -> String {
    format!(
        "bds_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn server_sites_keep_package_safe_hosts_only() {
        assert_eq!(
            super::server_sites("https://Example.com/path, *.docs.rs\nbad_host.test, ünïcode.test"),
            ["example.com", "docs.rs"]
        );
        assert_eq!(super::server_sites(&"a.test ".repeat(300)).len(), 200);
    }
    #[test]
    fn sites_accept_pasted_urls_and_wildcards() {
        assert_eq!(
            sites("https://crm.example.com/login, *.Example.org\nlocalhost:3000 nothost"),
            ["crm.example.com", "example.org", "localhost"]
        );
        assert!(sites("").is_empty());
    }

    #[test]
    fn sites_cover_subdomains_only() {
        let list = sites("example.com");
        assert!(site_allowed("https://example.com/a", &list));
        assert!(site_allowed("https://app.example.com", &list));
        assert!(!site_allowed("https://badexample.com", &list));
        assert!(!site_allowed("https://example.com.evil.test", &list));
        assert!(!site_allowed("javascript:alert(1)", &list));
        assert!(!site_allowed("file:///etc/passwd", &list));
        assert!(!site_allowed("https://example.com", &[]));
    }

    #[test]
    fn actions_need_their_fields_and_listed_sites() {
        let list = sites("example.com");
        let ok = |args: Value| check_action(&args, &list).is_ok();
        assert!(ok(json!({"action":"open","url":"https://example.com"})));
        assert!(!ok(json!({"action":"navigate","url":"https://other.test"})));
        assert!(!ok(json!({"action":"click"})));
        assert!(ok(json!({"action":"click","ref":4})));
        assert!(!ok(json!({"action":"type","ref":4})));
        assert!(ok(json!({"action":"type","ref":4,"text":"hi"})));
        assert!(ok(json!({"action":"read"})));
        assert!(!ok(json!({"action":"eval"})));
    }

    #[test]
    fn offered_calls_return_after_their_lease() {
        let offered = json!({"state":"offered","offered":1000,"created":990});
        assert!(!open(&offered, 1000 + LEASE_SECS));
        assert!(open(&offered, 1001 + LEASE_SECS));
        assert!(open(&json!({"state":"pending","created":1000}), 1000));
        assert!(!open(&json!({"state":"claimed","claimed":1000}), 99_999));
        assert!(stale(
            &json!({"state":"pending","created":1000}),
            1001 + OFFLINE_SECS
        ));
        assert!(!stale(
            &json!({"state":"replied","created":1000}),
            1000 + ANSWER_SECS
        ));
    }

    #[test]
    fn codes_and_secrets_are_distinct_and_long() {
        assert_ne!(link_code(), link_code());
        assert_eq!(device_secret().len(), 68);
    }
}
