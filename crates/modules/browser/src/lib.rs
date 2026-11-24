//! Browsers the agent drives: the user's own Chrome through the WebAgent extension
//! (Browser Extension), and a private server browser (Browser Control).
//!
//! The extension links to one WebAgent account and long-polls for queued actions; the
//! agent's `browser_extension_action` tool queues one action and waits for its result. This crate
//! holds the pure rules: which sites an agent may control, action admission, link codes
//! and the queued call's lifecycle. Browser Control runs in the worker's browser package;
//! this crate holds its site-policy parsing. The root adapter (`host/browser.rs`) owns
//! storage, routes, authority and tool dispatch.
pub mod limits;
pub mod pages;
pub mod queue;

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
/// Browser Control's setting for signing in to this app on navigation; on unless set to false.
pub const SIGN_IN_SETTING: &str = "sign_in_app";
/// How long a signed-in browser session lasts. Each call still gets at most a ten-minute lease.
pub const SIGN_IN_SECS: u64 = 1800;
/// A signed-in session this close to its end is replaced by a new one on the next call.
pub const RENEW_SECS: u64 = 120;
/// Saved site sign-ins, in the user's own store, one per site under its origin's digest.
pub const SITE_PREFIX: &str = "administrator_browser:site:";
/// The most one saved sign-in keeps, serialized: its cookies, steps and dates.
pub const SITE_MAX_BYTES: usize = 256 * 1024;

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

/// Whether a Browser Control call opens this app signed in: a `navigate` to a URL on the app's
/// own `origin`, unless the agent passed `signed_in: false` or the `setting` is off.
pub fn signs_in(args: &Value, origin: &str, setting: &Value) -> bool {
    args["action"] == "navigate"
        && args["signed_in"] != false
        && *setting != false
        && args["url"]
            .as_str()
            .is_some_and(|url| url == origin || url.starts_with(&format!("{origin}/")))
}

/// The https origin a saved site sign-in or a vault entry belongs to (`https://host` plus any
/// non-default port, as the vault stores origins); `None` for any other address.
pub fn site_origin(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    (parsed.scheme() == "https"
        && parsed.host_str().is_some()
        && parsed.username().is_empty()
        && parsed.password().is_none())
    .then(|| parsed.origin().ascii_serialization())
}

/// The vault entries a saved sign-in's steps type, in order, each once.
pub fn step_vaults(steps: &Value) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for name in steps
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|step| step["vault"].as_str())
    {
        if !names.iter().any(|seen| seen == name) {
            names.push(name.to_owned());
        }
    }
    names
}

/// Name suffixes that only resolve inside a private network: multicast DNS, common
/// intranet names and Tailscale's MagicDNS.
const PRIVATE_SUFFIXES: &[&str] = &[
    "localhost",
    "local",
    "localdomain",
    "internal",
    "intranet",
    "lan",
    "home.arpa",
    "ts.net",
    "tailscale.net",
];

/// Hosts the Browser Extension never acts on, listed or not: the app's own host, taken from
/// its public `origin`. The queued call carries them to the extension, which checks them too.
pub fn blocked_hosts(origin: &str) -> Vec<String> {
    url::Url::parse(origin)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_ascii_lowercase))
        .into_iter()
        .collect()
}

/// Why the extension may never act on `host`, or `None` when only the site list decides:
/// the app's own host (also through a listed parent or subdomain of it), loopback, IP
/// literals of any kind, and private-network or Tailscale names (BROWSER-5).
pub fn forbidden_host(host: &str, blocked: &[String]) -> Option<&'static str> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let related = |a: &str, b: &str| a == b || a.ends_with(&format!(".{b}"));
    if blocked
        .iter()
        .any(|app| related(&host, app) || related(app, &host))
    {
        return Some("this app's own address");
    }
    let last = host.rsplit('.').next().unwrap_or("");
    let ip = matches!(
        url::Url::parse(&format!("http://{host}/"))
            .ok()
            .and_then(|u| u.host().map(|h| h.to_owned())),
        Some(url::Host::Ipv4(_) | url::Host::Ipv6(_))
    );
    // A name whose last label is a number is an address in a browser's eyes (`0x7f.1`).
    if ip || host.contains([':', '[']) || last.starts_with(|c: char| c.is_ascii_digit()) {
        return Some("an IP address");
    }
    if PRIVATE_SUFFIXES.iter().any(|suffix| related(&host, suffix)) {
        return Some("a private-network address");
    }
    None
}

/// The Browser Extension's site list from the agent's `sites` setting, split into the hosts
/// it may use and the entries it refuses whatever the setting says ([`forbidden_host`]).
pub fn extension_sites(setting: &str, blocked: &[String]) -> (Vec<String>, Vec<String>) {
    sites(setting)
        .into_iter()
        .partition(|host| forbidden_host(host, blocked).is_none())
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
/// load a page, an allowlisted address that is not `blocked` or private. The extension checks
/// the tab's current site again at every action, since a page can redirect anywhere.
pub fn check_action(args: &Value, sites: &[String], blocked: &[String]) -> Result<()> {
    let action = args["action"].as_str().unwrap_or("");
    let has = |field: &str| !args[field].is_null();
    match action {
        "open" | "navigate" => {
            let url = args["url"].as_str().unwrap_or("");
            let host = url::Url::parse(url)
                .ok()
                .and_then(|parsed| parsed.host_str().map(str::to_owned));
            if let Some(reason) = host.as_deref().and_then(|h| forbidden_host(h, blocked)) {
                bail!("{url} is {reason}; the browser extension never acts there");
            }
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

/// The extension's `major.minor.patch` version, as it sends it with every poll.
pub fn extension_version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.trim().split('.').map(|part| {
        (!part.is_empty() && part.len() <= 6 && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse::<u32>().ok())
            .flatten()
    });
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// Whether an extension that sent `sent` may take actions: at least `minimum`. A missing or
/// unreadable version (every install older than 0.3.0 sends none) is refused.
pub fn extension_supported(sent: Option<&str>, minimum: (u32, u32, u32)) -> bool {
    sent.and_then(extension_version)
        .is_some_and(|version| version >= minimum)
}

/// The entry of `sites` that covers `url`'s host: a member's consent is given per entry.
pub fn site_entry<'a>(url: &str, sites: &'a [String]) -> Option<&'a str> {
    let host = url::Url::parse(url)
        .ok()?
        .host_str()?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    sites
        .iter()
        .find(|site| host == **site || host.ends_with(&format!(".{site}")))
        .map(String::as_str)
}

/// The member-consent check for one `browser_extension_action` (D3), given the entries of
/// `sites` the user agreed to for this agent: `Err(entry)` names the listed site an open or
/// navigate needs consent for; `Err("")` refuses any other page action while the user has
/// agreed to no site at all. `tabs` and `close` never need consent. The extension checks the
/// tab's site against the same `consented` entries at every action.
pub fn consent_refusal<'a>(
    args: &Value,
    sites: &'a [String],
    consented: &[String],
) -> Result<(), &'a str> {
    match args["action"].as_str().unwrap_or("") {
        "tabs" | "close" => Ok(()),
        "open" | "navigate" => match site_entry(args["url"].as_str().unwrap_or(""), sites) {
            Some(entry) if !consented.iter().any(|c| c == entry) => Err(entry),
            _ => Ok(()),
        },
        _ if consented.is_empty() => Err(""),
        _ => Ok(()),
    }
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
    #![allow(clippy::unwrap_used)]
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
    fn navigation_to_the_app_origin_signs_in_unless_turned_off() {
        let origin = "https://app.example";
        let navigate = |url: &str| json!({"action":"navigate","url":url});
        assert!(signs_in(
            &navigate("https://app.example/app?tab=builds"),
            origin,
            &Value::Null
        ));
        assert!(signs_in(
            &navigate("https://app.example"),
            origin,
            &json!(true)
        ));
        assert!(!signs_in(
            &navigate("https://app.example/app"),
            origin,
            &json!(false)
        ));
        for url in [
            "https://app.example.evil/app",
            "https://app.example@evil.test/",
            "http://app.example/app",
            "https://other.example/",
        ] {
            assert!(!signs_in(&navigate(url), origin, &Value::Null), "{url}");
        }
        let signed_out =
            json!({"action":"navigate","url":"https://app.example/app","signed_in":false});
        assert!(!signs_in(&signed_out, origin, &Value::Null));
        assert!(!signs_in(
            &json!({"action":"read","url":"https://app.example/app"}),
            origin,
            &Value::Null
        ));
    }
    #[test]
    fn saved_sign_ins_belong_to_plain_https_origins() {
        assert_eq!(
            site_origin("https://Example.com:443/login?next=/").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            site_origin("https://example.com:8443/").as_deref(),
            Some("https://example.com:8443")
        );
        for url in [
            "http://example.com/",
            concat!("https://", "user", ":", "pass", "@example.com/"),
            "javascript:alert(1)",
            "not a url",
        ] {
            assert_eq!(site_origin(url), None, "{url}");
        }
        let steps = json!([{"action":"type","vault":"pw"},{"action":"click"},{"vault":"code"},{"vault":"pw"}]);
        assert_eq!(step_vaults(&steps), ["pw", "code"]);
        assert!(step_vaults(&Value::Null).is_empty());
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
        let ok = |args: Value| check_action(&args, &list, &[]).is_ok();
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
    fn the_app_itself_is_never_opened_even_when_listed() {
        let blocked = blocked_hosts("https://dev.webagent.live");
        assert_eq!(blocked, ["dev.webagent.live"]);
        let open = json!({"action":"open","url":"https://dev.webagent.live/app"});
        // Listed by hand: the server still refuses it.
        let listed = sites("dev.webagent.live, example.com");
        let refused = check_action(&open, &listed, &blocked).unwrap_err();
        assert!(refused.to_string().contains("own address"), "{refused}");
        // And the agent's list never keeps it, nor a parent or subdomain of it.
        let (allowed, dropped) = extension_sites(
            "dev.webagent.live webagent.live api.dev.webagent.live example.com",
            &blocked,
        );
        assert_eq!(allowed, ["example.com"]);
        assert_eq!(dropped.len(), 3);
        // The listed site on the same list still opens.
        let other = json!({"action":"open","url":"https://www.example.com/a"});
        assert!(check_action(&other, &allowed, &blocked).is_ok());
        let navigate = json!({"action":"navigate","url":"https://dev.webagent.live/app?tab=admin"});
        assert!(check_action(&navigate, &listed, &blocked).is_err());
    }

    #[test]
    fn site_lists_refuse_loopback_private_and_tailscale_hosts() {
        let blocked = blocked_hosts("https://dev.webagent.live");
        let (allowed, dropped) = extension_sites(
            "localhost, app.localhost, 127.0.0.1, 10.1.2.3, 192.168.1.10, 172.16.0.5, \
             169.254.169.254, 100.87.9.65, 0x7f.0.0.1, 2130706433.1, nas.local, \
             db.corp.internal, intranet, printer.lan, router.home.arpa, ar-server.tail1234.ts.net, \
             crm.example.com, docs.rs",
            &blocked,
        );
        assert_eq!(allowed, ["crm.example.com", "docs.rs"]);
        assert_eq!(dropped.len(), 15, "{dropped:?}");
        for host in ["[::1]", "::1", "[fe80::1]"] {
            assert_eq!(
                forbidden_host(host, &blocked),
                Some("an IP address"),
                "{host}"
            );
        }
        assert_eq!(forbidden_host("crm.example.com", &blocked), None);
        // Even an entry that slipped onto the list is refused at open and navigate.
        let raw = sites("192.168.1.10 nas.local");
        for url in ["http://192.168.1.10/admin", "https://nas.local/"] {
            let open = json!({"action":"open","url":url});
            assert!(check_action(&open, &raw, &blocked).is_err(), "{url}");
        }
    }

    #[test]
    fn extensions_older_than_the_minimum_are_refused() {
        let minimum = (0, 2, 0);
        // Every 0.1.0 install sends no version at all.
        assert!(!extension_supported(None, minimum));
        for old in [
            "0.1.0",
            "0.1.9",
            "",
            "0.2",
            "0.2.0.1",
            "x.y.z",
            "0.-1.0",
            "0.2.0-beta",
        ] {
            assert!(!extension_supported(Some(old), minimum), "{old}");
        }
        for new in ["0.2.0", "0.3.0", "1.0.0", " 0.10.2 "] {
            assert!(extension_supported(Some(new), minimum), "{new}");
        }
        assert!(!extension_supported(Some("0.3.0"), (0, 3, 1)));
    }

    #[test]
    fn members_need_consent_per_listed_site() {
        let list = sites("example.com, crm.test");
        let open = |url: &str| json!({"action":"open","url":url});
        // Nothing agreed: open names the entry, page actions are refused, tabs/close are not.
        assert_eq!(
            consent_refusal(&open("https://www.example.com/a"), &list, &[]),
            Err("example.com")
        );
        assert_eq!(
            consent_refusal(&json!({"action":"read"}), &list, &[]),
            Err("")
        );
        assert_eq!(
            consent_refusal(&json!({"action":"click","ref":3}), &list, &[]),
            Err("")
        );
        assert!(consent_refusal(&json!({"action":"tabs"}), &list, &[]).is_ok());
        assert!(consent_refusal(&json!({"action":"close"}), &list, &[]).is_ok());
        // Agreed to example.com: it and its subdomains open, crm.test still needs consent.
        let agreed = vec!["example.com".to_owned()];
        assert!(consent_refusal(&open("https://app.example.com/"), &list, &agreed).is_ok());
        assert!(consent_refusal(&json!({"action":"read"}), &list, &agreed).is_ok());
        assert_eq!(
            consent_refusal(
                &json!({"action":"navigate","url":"https://crm.test/x"}),
                &list,
                &agreed
            ),
            Err("crm.test")
        );
        assert_eq!(
            site_entry("https://APP.example.com./", &list),
            Some("example.com")
        );
        assert_eq!(site_entry("https://badexample.com/", &list), None);
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
