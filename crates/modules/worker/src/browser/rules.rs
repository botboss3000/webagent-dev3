//! Validation of the trusted authority the worker supplies and of the model's arguments,
//! before any browser effect: ported rule for rule (and message for message) from the
//! Python Browser Control adapter.
use super::url::{parse_qsl, urlencode, urlsplit, urlunsplit, Invalid};
use super::{now, Fault, KEYS, MAX_READ_OFFSET, MAX_SCROLL, MAX_SITE_COOKIES, MAX_STEPS};
use super::{COLOR_SCHEMES, REDUCED_MOTION, VIEWPORTS};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::Path;

/// URL parameters the app itself writes. Invite and embed authentication parameters stay
/// excluded.
const APP_PARAMS: [&str; 12] = [
    "agent",
    "agentScope",
    "agentView",
    "appView",
    "article",
    "job_id",
    "path",
    "session_id",
    "tab",
    "theme",
    "ui_sample",
    "window",
];
pub(crate) const READ_ACTIONS: [&str; 12] = [
    "animations",
    "close",
    "emulate",
    "measure",
    "navigate",
    "performance",
    "pixels",
    "read",
    "screenshot",
    "scroll",
    "viewport",
    "wait_for",
];
const INPUT_ACTIONS: [&str; 5] = ["click", "type", "press", "touch", "save_login"];
const WAIT_CONDITIONS: [&str; 6] = [
    "animations_done",
    "hidden",
    "network_idle",
    "quiet",
    "text",
    "visible",
];
const MAX_PIXELS: usize = 20;

impl From<Invalid> for Fault {
    fn from(_: Invalid) -> Self {
        Self::backend("ValueError")
    }
}

/// Whether `value` fully matches `[chars]{low,high}`, counted in characters.
fn shaped(value: &str, low: usize, high: usize, allowed: impl Fn(char) -> bool) -> bool {
    let count = value.chars().count();
    (low..=high).contains(&count) && value.chars().all(allowed)
}

fn lower_id(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'
}

/// A vault entry name: `[A-Za-z0-9_-]{1,64}`.
#[must_use]
pub fn vault_name(value: &str) -> bool {
    shaped(value, 1, 64, |c| {
        c.is_ascii_alphanumeric() || c == '_' || c == '-'
    })
}

/// Canonical (lower-case, hyphenated) UUID spelling, as private ownership requires.
#[must_use]
pub fn canonical_id(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| id.hyphenated().to_string() == value)
}

/// `scheme://host[:port]` of an http(s) URL (default port left out), or `""` otherwise.
#[must_use]
pub fn origin_of(url: &str) -> String {
    let Ok(parsed) = urlsplit(url) else {
        return String::new();
    };
    let Ok(port) = parsed.port() else {
        return String::new();
    };
    let Some(host) = parsed.hostname() else {
        return String::new();
    };
    if !matches!(parsed.scheme.as_str(), "http" | "https") {
        return String::new();
    }
    let default = if parsed.scheme == "https" { 443 } else { 80 };
    match port {
        Some(port) if port != default => format!("{}://{host}:{port}", parsed.scheme),
        _ => format!("{}://{host}", parsed.scheme),
    }
}

/// A plain https site origin, as the vault stores it.
#[must_use]
pub fn valid_origin(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|value| value.starts_with("https://") && origin_of(value) == value)
}

/// A pattern covers its host and every subdomain.
#[must_use]
pub fn host_matches(host: &str, patterns: &[String]) -> bool {
    patterns
        .iter()
        .any(|pattern| host == pattern || host.ends_with(&format!(".{pattern}")))
}

fn in_v4(address: u32, network: [u8; 4], prefix: u32) -> bool {
    let network = u32::from_be_bytes(network);
    let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
    address & mask == network & mask
}

fn in_v6(address: u128, network: [u16; 8], prefix: u32) -> bool {
    let network = network
        .iter()
        .fold(0u128, |acc, part| acc << 16 | u128::from(*part));
    let mask = u128::MAX.checked_shl(128 - prefix).unwrap_or(0);
    address & mask == network & mask
}

/// Python's `ipaddress` `is_global` (3.13 tables): not private, reserved, shared or
/// documentation space; IPv4-mapped IPv6 addresses follow their IPv4 address.
#[must_use]
pub fn is_global(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let address = u32::from(v4);
            let private = [
                ([0, 0, 0, 0], 8),
                ([10, 0, 0, 0], 8),
                ([127, 0, 0, 0], 8),
                ([169, 254, 0, 0], 16),
                ([172, 16, 0, 0], 12),
                ([192, 0, 0, 0], 24),
                ([192, 0, 0, 170], 31),
                ([192, 0, 2, 0], 24),
                ([192, 168, 0, 0], 16),
                ([198, 18, 0, 0], 15),
                ([198, 51, 100, 0], 24),
                ([203, 0, 113, 0], 24),
                ([240, 0, 0, 0], 4),
                ([255, 255, 255, 255], 32),
            ]
            .iter()
            .any(|(network, prefix)| in_v4(address, *network, *prefix))
                && ![[192, 0, 0, 9], [192, 0, 0, 10]]
                    .iter()
                    .any(|network| in_v4(address, *network, 32));
            !in_v4(address, [100, 64, 0, 0], 10) && !private
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_global(IpAddr::V4(mapped));
            }
            let address = u128::from(v6);
            let private = [
                ([0, 0, 0, 0, 0, 0, 0, 1], 128),
                ([0, 0, 0, 0, 0, 0, 0, 0], 128),
                ([0x64, 0xff9b, 1, 0, 0, 0, 0, 0], 48),
                ([0x100, 0, 0, 0, 0, 0, 0, 0], 64),
                ([0x2001, 0, 0, 0, 0, 0, 0, 0], 23),
                ([0x2001, 0xdb8, 0, 0, 0, 0, 0, 0], 32),
                ([0x2002, 0, 0, 0, 0, 0, 0, 0], 16),
                ([0x3fff, 0, 0, 0, 0, 0, 0, 0], 20),
                ([0xfc00, 0, 0, 0, 0, 0, 0, 0], 7),
                ([0xfe80, 0, 0, 0, 0, 0, 0, 0], 10),
            ]
            .iter()
            .any(|(network, prefix)| in_v6(address, *network, *prefix))
                && ![
                    ([0x2001, 1, 0, 0, 0, 0, 0, 1], 128),
                    ([0x2001, 1, 0, 0, 0, 0, 0, 2], 128),
                    ([0x2001, 3, 0, 0, 0, 0, 0, 0], 32),
                    ([0x2001, 4, 0x112, 0, 0, 0, 0, 0], 48),
                    ([0x2001, 0x20, 0, 0, 0, 0, 0, 0], 28),
                    ([0x2001, 0x30, 0, 0, 0, 0, 0, 0], 28),
                ]
                .iter()
                .any(|(network, prefix)| in_v6(address, *network, *prefix));
            !private
        }
    }
}

/// The agent's site lists for the signed-out browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sites {
    pub allow: Vec<String>,
    pub block: Vec<String>,
}

/// A vault value Rust resolved for this call, bound to its own https origin.
#[derive(Debug, Clone)]
pub struct Secret {
    pub value: String,
    pub origin: String,
}

/// A saved site sign-in: its cookies, recorded steps and sign-in page.
#[derive(Debug, Clone)]
pub struct Site {
    pub origin: String,
    pub cookies: Vec<Value>,
    pub steps: Vec<Value>,
    pub login_url: String,
}

/// The worker-supplied current-caller authority, validated.
#[derive(Debug, Clone)]
pub struct Trusted {
    pub session_id: String,
    pub user_id: String,
    pub agent_id: String,
    /// The app session cookie when signed in; `None` signed out.
    pub cookie: Option<String>,
    /// The signed-out site lists; `None` signed in.
    pub sites: Option<Sites>,
    /// `trusted["sites"]` as sent, compared when a browser is reused.
    pub sites_raw: Value,
    pub generation: String,
    pub expires_at: f64,
    pub secrets: BTreeMap<String, Secret>,
    pub site: Option<Site>,
}

impl Trusted {
    /// The owner tuple a session process and the worker pool pin to.
    #[must_use]
    pub fn identity(&self) -> (String, String, String, String, Option<String>) {
        (
            self.session_id.clone(),
            self.user_id.clone(),
            self.agent_id.clone(),
            self.generation.clone(),
            self.cookie.clone(),
        )
    }
}

fn host_list(value: &Value) -> Option<Vec<String>> {
    let list = value.as_array().filter(|list| list.len() <= 200)?;
    list.iter()
        .map(|host| {
            host.as_str()
                .filter(|host| {
                    shaped(host, 1, 253, |c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-'
                    })
                })
                .map(str::to_owned)
        })
        .collect()
}

fn number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|_| value.is_number())
}

/// Validates the current-caller authority and its bounded lease for the session folder
/// `base`.
///
/// # Errors
/// A rejection naming the first rule broken.
pub fn validate_trusted(base: &Path, trusted: &Value) -> Result<Trusted, Fault> {
    let Value::Object(map) = trusted else {
        return Err(Fault::rejected("Trusted browser authorization required"));
    };
    let text = |key: &str| map.get(key).and_then(Value::as_str);
    let sid = text("session_id")
        .filter(|sid| canonical_id(sid))
        .filter(|sid| base.file_name().and_then(|name| name.to_str()) == Some(*sid))
        .ok_or_else(|| Fault::rejected("Browser session ownership mismatch"))?;
    let user = text("user_id")
        .filter(|user| shaped(user, 1, 64, lower_id))
        .ok_or_else(|| Fault::rejected("Invalid trusted caller"))?;
    let agent = text("agent_id")
        .filter(|agent| shaped(agent, 1, 64, lower_id))
        .ok_or_else(|| Fault::rejected("Invalid trusted agent"))?;
    let cookie = match map.get("cookie") {
        None | Some(Value::Null) => None,
        Some(Value::String(cookie)) if canonical_id(cookie) => Some(cookie.clone()),
        Some(_) => return Err(Fault::rejected("Invalid trusted authentication")),
    };
    let sites_raw = map.get("sites").cloned().unwrap_or(Value::Null);
    let sites = if cookie.is_none() {
        let parsed = sites_raw.as_object().and_then(|sites| {
            if sites.len() != 2 {
                return None;
            }
            Some(Sites {
                allow: host_list(sites.get("allow")?)?,
                block: host_list(sites.get("block")?)?,
            })
        });
        Some(parsed.ok_or_else(|| Fault::rejected("Invalid trusted site policy"))?)
    } else {
        None
    };
    let generation = text("generation")
        .filter(|generation| (1..=128).contains(&generation.chars().count()))
        .ok_or_else(|| Fault::rejected("Invalid trusted generation"))?;
    let current = now();
    let expires_at = map
        .get("expires_at")
        .and_then(number)
        .filter(|expires| current < *expires && *expires <= now() + 601.0)
        .ok_or_else(|| Fault::rejected("Browser authorization expired or exceeds lease"))?;
    if map.get("allowed_session_ids") != Some(&Value::Array(vec![Value::from(sid)])) {
        return Err(Fault::rejected(
            "Browser routes require the current session",
        ));
    }
    let secrets = validate_secrets(map.get("secrets"))
        .ok_or_else(|| Fault::rejected("Invalid trusted vault values"))?;
    let site = match map.get("site") {
        None | Some(Value::Null) => None,
        Some(site) => Some(
            validate_site(site)
                .filter(|_| cookie.is_none())
                .ok_or_else(|| Fault::rejected("Invalid trusted site sign-in"))?,
        ),
    };
    Ok(Trusted {
        session_id: sid.to_owned(),
        user_id: user.to_owned(),
        agent_id: agent.to_owned(),
        cookie,
        sites,
        sites_raw,
        generation: generation.to_owned(),
        expires_at,
        secrets,
        site,
    })
}

fn validate_secrets(value: Option<&Value>) -> Option<BTreeMap<String, Secret>> {
    let Some(value) = value else {
        return Some(BTreeMap::new());
    };
    let entries = value
        .as_object()
        .filter(|entries| entries.len() <= MAX_STEPS)?;
    entries
        .iter()
        .map(|(name, entry)| {
            let entry = entry.as_object().filter(|entry| {
                entry.len() == 2 && entry.contains_key("value") && entry.contains_key("origin")
            })?;
            let secret = entry["value"]
                .as_str()
                .filter(|secret| (1..=8192).contains(&secret.chars().count()))?;
            (vault_name(name) && valid_origin(&entry["origin"])).then(|| {
                (
                    name.clone(),
                    Secret {
                        value: secret.to_owned(),
                        origin: entry["origin"].as_str().unwrap_or_default().to_owned(),
                    },
                )
            })
        })
        .collect()
}

fn validate_site(value: &Value) -> Option<Site> {
    let site = value.as_object()?;
    let origin = site.get("origin").filter(|origin| valid_origin(origin))?;
    let cookies = site.get("cookies")?.as_array().filter(|cookies| {
        cookies.len() <= MAX_SITE_COOKIES && cookies.iter().all(Value::is_object)
    })?;
    let steps = site
        .get("steps")?
        .as_array()
        .filter(|steps| steps.len() <= MAX_STEPS && steps.iter().all(Value::is_object))?;
    let login_url = site
        .get("login_url")?
        .as_str()
        .filter(|url| Some(origin_of(url).as_str()) == origin.as_str())?;
    Some(Site {
        origin: origin.as_str().unwrap_or_default().to_owned(),
        cookies: cookies.clone(),
        steps: steps.clone(),
        login_url: login_url.to_owned(),
    })
}

/// Where a URL may lead: the agent's public sites signed out, or the current session's
/// development routes signed in.
pub enum Scope<'a> {
    SignedIn {
        session_id: &'a str,
        agent_id: &'a str,
    },
    SignedOut {
        sites: &'a Sites,
    },
}

impl Trusted {
    /// The URL scope this authority grants.
    #[must_use]
    pub fn scope(&self) -> Scope<'_> {
        match &self.sites {
            Some(sites) if self.cookie.is_none() => Scope::SignedOut { sites },
            _ => Scope::SignedIn {
                session_id: &self.session_id,
                agent_id: &self.agent_id,
            },
        }
    }
}

/// Signed out: a public http(s) URL the agent's site policy allows, normalized. Signed in:
/// a development route (`origin` + route accepted and stripped).
///
/// # Errors
/// A rejection naming the rule; a malformed bracketed host is a `ValueError` failure.
pub fn validate_url(url: &Value, scope: &Scope<'_>, origin: &str) -> Result<String, Fault> {
    let sites = match scope {
        Scope::SignedIn {
            session_id,
            agent_id,
        } => {
            let route = match url.as_str() {
                Some(text) if text.starts_with(&format!("{origin}/")) => {
                    Value::from(&text[origin.len()..])
                }
                _ => url.clone(),
            };
            return validate_path(&route, session_id, agent_id);
        }
        Scope::SignedOut { sites } => sites,
    };
    let full = || Fault::rejected("Navigate needs a full http or https URL");
    let url = url
        .as_str()
        .filter(|url| (1..=2048).contains(&url.chars().count()))
        .filter(|url| !url.chars().any(|c| (c as u32) < 33) && !url.contains('\\'))
        .ok_or_else(full)?;
    let parsed = urlsplit(url)?;
    let host = parsed.hostname();
    if !matches!(parsed.scheme.as_str(), "http" | "https") || host.is_none() {
        return Err(full());
    }
    if parsed.has_userinfo() {
        return Err(Fault::rejected(
            "URLs with a user name or password are not allowed",
        ));
    }
    let port = parsed
        .port()
        .map_err(|_| Fault::rejected("Invalid URL port"))?;
    if !matches!(port, None | Some(80 | 443)) {
        return Err(Fault::rejected("Only ports 80 and 443 are allowed"));
    }
    let host = host.unwrap_or_default();
    let host = host.trim_end_matches('.');
    let internal = || Fault::rejected("Private and internal addresses are not allowed");
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !is_global(ip) {
            return Err(internal());
        }
    }
    // The guard checks resolved addresses too; obvious internal names fail early with a
    // clear reason.
    let local: Vec<String> = ["localhost", "local", "internal", "lan", "home.arpa"]
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    if !host.contains('.') || host_matches(host, &local) {
        return Err(internal());
    }
    if host_matches(host, &sites.block) {
        return Err(Fault::rejected(format!(
            "This agent's site policy blocks {host}"
        )));
    }
    if !sites.allow.is_empty() && !host_matches(host, &sites.allow) {
        return Err(Fault::rejected(format!(
            "This agent may only open: {}",
            sites.allow.join(", ")
        )));
    }
    let path = if parsed.path.is_empty() {
        "/"
    } else {
        &parsed.path
    };
    Ok(urlunsplit(
        &parsed.scheme,
        &parsed.netloc.to_lowercase(),
        path,
        &parsed.query,
        "",
    ))
}

/// Confines navigation to the fixed development origin and the current session's
/// implemented routes; returns the normalized route.
///
/// # Errors
/// A rejection naming the rule broken.
pub fn validate_path(path: &Value, session_id: &str, agent_id: &str) -> Result<String, Fault> {
    let invalid = || Fault::rejected("Invalid development route");
    let path = path
        .as_str()
        .filter(|path| (1..=2048).contains(&path.chars().count()))
        .filter(|path| !path.chars().any(|c| (c as u32) < 32) && !path.contains('\\'))
        .ok_or_else(invalid)?;
    let mut parsed = urlsplit(path)?;
    // The site root serves the app, so open it as the app route instead of refusing.
    if parsed.path == "/" && parsed.query.is_empty() {
        parsed.path = "/app".to_owned();
    }
    if !parsed.scheme.is_empty()
        || !parsed.netloc.is_empty()
        || !parsed.fragment.is_empty()
        || !parsed.path.starts_with('/')
        || parsed.path.contains('%')
    {
        return Err(Fault::rejected(
            "Only exact relative development routes are supported",
        ));
    }
    if parsed.path.contains("//")
        || parsed
            .path
            .split('/')
            .any(|part| matches!(part, "." | ".."))
    {
        return Err(invalid());
    }
    let pairs = parse_qsl(&parsed.query);
    let mut query: Vec<(String, String)> = Vec::new();
    for (name, value) in pairs {
        if query.iter().any(|(seen, _)| *seen == name) {
            return Err(Fault::rejected("Duplicate route parameters"));
        }
        query.push((name, value));
    }
    let get = |name: &str| {
        query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    if parsed.path == "/app" {
        if query
            .iter()
            .any(|(name, _)| !APP_PARAMS.contains(&name.as_str()))
        {
            return Err(Fault::rejected(format!(
                "Unsupported app route parameters; allowed: {}",
                APP_PARAMS.join(", ")
            )));
        }
        if query
            .iter()
            .any(|(_, value)| !(1..=256).contains(&value.chars().count()))
        {
            return Err(Fault::rejected("Invalid app route parameter"));
        }
        let kebab = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
        if !shaped(get("tab").unwrap_or("chat"), 1, 40, kebab) {
            return Err(Fault::rejected("Unsupported development page"));
        }
        // The browser signs in with the caller's own app cookie, so the app shows only that
        // user's sessions: another user's id reads as missing there, never as their chat.
        let hex_id = |c: char| c.is_ascii_hexdigit() || c == '-';
        if get("session_id").is_some_and(|sid| !shaped(sid, 36, 36, hex_id)) {
            return Err(Fault::rejected("Invalid chat session id"));
        }
        if get("agent").is_some_and(|agent| agent != agent_id) {
            return Err(Fault::rejected("Another agent is unavailable"));
        }
        if !shaped(get("agentView").unwrap_or("config"), 1, 40, lower_id) {
            return Err(Fault::rejected("Unsupported agent view"));
        }
        // Avoid an unconstrained restored conversation on a bare app route.
        if get("session_id").is_none() {
            query.push(("session_id".into(), session_id.to_owned()));
        }
        return Ok(urlunsplit("", "", &parsed.path, &urlencode(&query), ""));
    }
    if !query.is_empty() {
        return Err(Fault::rejected(
            "API navigation does not accept query parameters",
        ));
    }
    if parsed.path == "/api/v3/auth/me" {
        return Ok(parsed.path);
    }
    let prefix = format!("/api/v3/sessions/{session_id}");
    if [
        prefix.clone(),
        format!("{prefix}/abilities"),
        format!("{prefix}/mode"),
    ]
    .contains(&parsed.path)
    {
        return Ok(parsed.path);
    }
    if let Some(run) = parsed
        .path
        .strip_prefix(&format!("{prefix}/runs/"))
        .and_then(|rest| rest.strip_suffix("/diagnostics"))
    {
        if shaped(run, 36, 36, |c| {
            c.is_ascii_digit() || ('a'..='f').contains(&c) || c == '-'
        }) && canonical_id(run)
        {
            return Ok(parsed.path);
        }
    }
    // A UI draft preview of this session's own workspace; the app checks ownership again.
    if let Some(variant) = parsed.path.strip_prefix(&format!("{prefix}/ui-preview/")) {
        if shaped(variant, 1, 32, |c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'
        }) {
            return Ok(parsed.path);
        }
    }
    if !parsed.path.starts_with("/api/") {
        return Err(Fault::rejected(
            "The browser opens app pages (/app?...) only; read source files such as CSS or JS with file_read",
        ));
    }
    // Name the mismatch: a URL for another session is the most common cause of this refusal,
    // and an agent that can see both ids can fix it in one step instead of retrying blindly.
    if let Some(rest) = parsed.path.strip_prefix("/api/v3/sessions/") {
        if let Some((other, _)) = rest.split_once('/') {
            if other != session_id && canonical_id(other) {
                return Err(Fault::rejected(format!(
                    "This browser reads only its own session: the URL names {other} but the current session is {session_id}; use {session_id}",
                )));
            }
        }
    }
    Err(Fault::rejected("Unsupported current-session read route"))
}

fn is_int(value: &Value) -> bool {
    value.is_i64() || value.is_u64()
}

fn int_in(value: &Value, low: i64, high: i64) -> bool {
    value
        .as_i64()
        .is_some_and(|value| (low..=high).contains(&value))
}

fn blank(value: Option<&Value>) -> bool {
    matches!(value, None | Some(Value::Null)) || value == Some(&Value::from(""))
}

/// Python truthiness of an optional JSON value.
pub(crate) fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(items)) => !items.is_empty(),
        Some(Value::Number(number)) => number.as_f64() != Some(0.0),
        Some(Value::Bool(true)) => true,
    }
}

/// The arguments each action takes besides `action`.
fn permitted(action: &str) -> &'static [&'static str] {
    match action {
        "navigate" => &["url", "signed_in"],
        "read" => &["selector", "offset"],
        "click" => &["selector"],
        "type" => &["selector", "text", "vault", "totp"],
        "save_login" => &["origin"],
        "viewport" => &["preset"],
        "measure" => &["selector", "offset", "styles"],
        "pixels" => &["points", "selector"],
        "wait_for" => &["condition", "selector", "text", "timeout_ms", "quiet_ms"],
        "animations" => &["selector"],
        "scroll" => &["selector", "dx", "dy"],
        "press" => &["key", "selector"],
        "touch" => &["selector", "dx", "dy"],
        "emulate" => &["color_scheme", "reduced_motion"],
        "screenshot" => &["label"],
        _ => &[],
    }
}

/// Rejects unknown or action-inapplicable fields before browser effects, dropping empty
/// optional fields a schema may emit. Returns the action.
///
/// # Errors
/// A rejection naming the first rule broken.
pub fn validate_args(name: &str, args: &mut Map<String, Value>) -> Result<&'static str, Fault> {
    let action = args.get("action").and_then(Value::as_str).unwrap_or("");
    let (allowed, action): (&[&str], &'static str) = match name {
        "browser_read" => {
            let Some(found) = READ_ACTIONS.iter().find(|known| **known == action) else {
                return Err(Fault::rejected(format!(
                    "browser_read actions: {}; click, type, press and touch use browser_input",
                    READ_ACTIONS.join(", ")
                )));
            };
            (
                &[
                    "action",
                    "url",
                    "signed_in",
                    "selector",
                    "offset",
                    "condition",
                    "text",
                    "timeout_ms",
                    "quiet_ms",
                    "dx",
                    "dy",
                    "preset",
                    "color_scheme",
                    "reduced_motion",
                    "styles",
                    "points",
                    "expected_sequence",
                    "label",
                ],
                found,
            )
        }
        "browser_input" => {
            let Some(found) = INPUT_ACTIONS.iter().find(|known| **known == action) else {
                return Err(Fault::rejected(
                    "browser_input actions: click, type, press, touch, save_login; everything else uses browser_read",
                ));
            };
            (
                &[
                    "action",
                    "selector",
                    "text",
                    "key",
                    "vault",
                    "totp",
                    "origin",
                    "dx",
                    "dy",
                    "expected_sequence",
                ],
                found,
            )
        }
        "browser_record" => {
            let Some(found) = ["start", "stop", "status"]
                .iter()
                .find(|known| **known == action)
            else {
                return Err(Fault::rejected("Unsupported recorder action"));
            };
            (&["action"], found)
        }
        _ => return Err(Fault::rejected("Unknown browser tool")),
    };
    if args.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(Fault::rejected("Unknown browser argument"));
    }
    let keys: Vec<String> = args.keys().cloned().collect();
    for key in keys {
        if key == "action" || permitted(action).contains(&key.as_str()) {
            continue;
        }
        let value = &args[&key];
        let zero_sequence =
            key == "expected_sequence" && is_int(value) && value.as_i64() == Some(0);
        if !zero_sequence && !blank(Some(value)) {
            return Err(Fault::rejected("Arguments do not apply to this action"));
        }
        args.remove(&key);
    }
    if [
        "read",
        "animations",
        "scroll",
        "press",
        "wait_for",
        "pixels",
    ]
    .contains(&action)
        && blank(args.get("selector"))
    {
        args.remove("selector");
    }
    // signed_in is Rust's to apply when it chooses the authority; the browser only checks
    // its type.
    if let Some(signed_in) = args.remove("signed_in") {
        if !signed_in.is_boolean() {
            return Err(Fault::rejected("signed_in must be true or false"));
        }
    }
    if action == "navigate" && !truthy(args.get("url")) {
        return Err(Fault::rejected("Navigate needs a url"));
    }
    if let Some(selector) = args.get("selector") {
        if !selector.as_str().is_some_and(|selector| {
            (1..=256).contains(&selector.chars().count()) && !selector.contains('\0')
        }) {
            return Err(Fault::rejected("Invalid bounded CSS selector"));
        }
    }
    if let Some(text) = args.get("text") {
        if !text
            .as_str()
            .is_some_and(|text| text.len() <= 4096 && !text.contains('\0'))
        {
            return Err(Fault::rejected("Literal text exceeds limit"));
        }
    }
    if matches!(action, "click" | "type") && !truthy(args.get("selector")) {
        return Err(Fault::rejected("Action requires a selector"));
    }
    if action == "type" && args.contains_key("text") == args.contains_key("vault") {
        return Err(Fault::rejected(
            "Type needs either literal text or a vault entry name",
        ));
    }
    if let Some(vault) = args.get("vault") {
        if !vault.as_str().is_some_and(vault_name) {
            return Err(Fault::rejected("Invalid vault entry name"));
        }
    }
    if let Some(totp) = args.get("totp") {
        if !totp.is_boolean() || !args.contains_key("vault") {
            return Err(Fault::rejected(
                "totp is true or false and goes with a vault entry",
            ));
        }
    }
    if action == "save_login" && !args.get("origin").is_some_and(valid_origin) {
        return Err(Fault::rejected(
            "save_login needs the site's https origin, for example https://example.com",
        ));
    }
    if action == "viewport"
        && !args
            .get("preset")
            .and_then(Value::as_str)
            .is_some_and(|preset| VIEWPORTS.iter().any(|(name, _, _)| *name == preset))
    {
        let names: Vec<&str> = VIEWPORTS.iter().map(|(name, _, _)| *name).collect();
        return Err(Fault::rejected(format!(
            "Viewport preset must be one of {}",
            names.join(", ")
        )));
    }
    if let Some(offset) = args.get("offset") {
        if !(is_int(offset) && int_in(offset, 0, MAX_READ_OFFSET)) {
            return Err(Fault::rejected(
                "Read offset must be a whole number of characters",
            ));
        }
    }
    if action == "measure" {
        if !truthy(args.get("selector")) {
            return Err(Fault::rejected("Measure requires a selector"));
        }
        if args
            .get("styles")
            .is_some_and(|styles| !styles.is_boolean())
        {
            return Err(Fault::rejected("styles must be true or false"));
        }
    }
    if action == "pixels" {
        let points = args.get("points");
        let none = matches!(points, None | Some(Value::Null))
            || points.and_then(Value::as_array).is_some_and(Vec::is_empty);
        if none && !truthy(args.get("selector")) {
            return Err(Fault::rejected("Pixels needs points or a selector"));
        }
        if !none && !points.is_some_and(valid_points) {
            return Err(Fault::rejected(
                "points must be up to 20 objects with whole-pixel x and y inside the viewport",
            ));
        }
    }
    if action == "wait_for" {
        let condition = args.get("condition").and_then(Value::as_str);
        if !condition.is_some_and(|condition| WAIT_CONDITIONS.contains(&condition)) {
            return Err(Fault::rejected(format!(
                "Wait condition must be one of {}",
                WAIT_CONDITIONS.join(", ")
            )));
        }
        if matches!(condition, Some("visible" | "hidden")) && !truthy(args.get("selector")) {
            return Err(Fault::rejected("This wait condition requires a selector"));
        }
        if condition == Some("text") && !truthy(args.get("text")) {
            return Err(Fault::rejected("A text wait requires the text to wait for"));
        }
        for (key, low, high) in [("timeout_ms", 100, 10000), ("quiet_ms", 100, 3000)] {
            if let Some(value) = args.get(key) {
                if !(is_int(value) && int_in(value, low, high)) {
                    return Err(Fault::rejected(format!(
                        "{key} must be a whole number from {low} to {high}"
                    )));
                }
            }
        }
    }
    if action == "scroll" {
        for key in ["dx", "dy"] {
            if let Some(value) = args.get(key) {
                if !(is_int(value) && int_in(value, -MAX_SCROLL, MAX_SCROLL)) {
                    return Err(Fault::rejected(
                        "Scroll distances must be whole pixels within 20000",
                    ));
                }
            }
        }
        if !truthy(args.get("selector")) && !truthy(args.get("dx")) && !truthy(args.get("dy")) {
            return Err(Fault::rejected(
                "Scroll needs a selector to bring into view or a distance",
            ));
        }
    }
    if action == "touch" {
        if blank(args.get("selector")) {
            return Err(Fault::rejected("touch needs a selector"));
        }
        for key in ["dx", "dy"] {
            if let Some(value) = args.get(key) {
                if !is_int(value) || value.as_i64().is_none_or(|v| v.abs() > 2000) {
                    return Err(Fault::rejected(
                        "Touch distances must be whole pixels within 2000",
                    ));
                }
            }
        }
    }
    if action == "press"
        && !args
            .get("key")
            .and_then(Value::as_str)
            .is_some_and(|key| KEYS.iter().any(|(name, _)| *name == key))
    {
        let names: Vec<&str> = KEYS.iter().map(|(name, _)| *name).collect();
        return Err(Fault::rejected(format!(
            "Key must be one of {}",
            names.join(", ")
        )));
    }
    if action == "emulate" {
        if !args.contains_key("color_scheme") && !args.contains_key("reduced_motion") {
            return Err(Fault::rejected(
                "Emulate needs color_scheme or reduced_motion",
            ));
        }
        let known = |key: &str, table: &[(&str, &str)]| {
            args.get(key).is_none_or(|value| {
                value
                    .as_str()
                    .is_some_and(|value| table.iter().any(|(name, _)| *name == value))
            })
        };
        if !known("color_scheme", &COLOR_SCHEMES) {
            return Err(Fault::rejected(
                "color_scheme must be light, dark or default",
            ));
        }
        if !known("reduced_motion", &REDUCED_MOTION) {
            return Err(Fault::rejected("reduced_motion must be reduce or default"));
        }
    }
    Ok(action)
}

fn valid_points(points: &Value) -> bool {
    points.as_array().is_some_and(|points| {
        points.len() <= MAX_PIXELS
            && points.iter().all(|point| {
                point.as_object().is_some_and(|point| {
                    point.len() == 2
                        && point
                            .get("x")
                            .is_some_and(|x| is_int(x) && int_in(x, 0, 1599))
                        && point
                            .get("y")
                            .is_some_and(|y| is_int(y) && int_in(y, 0, 1199))
                })
            })
    })
}

#[cfg(test)]
mod preview_route_tests {
    use super::validate_path;
    use serde_json::Value;

    #[test]
    fn only_this_sessions_ui_preview_route_is_allowed() {
        let sid = "083cd9bb-7c27-4022-a9de-09e7110a9e92";
        let other = "21e5ab5f-89d9-46ee-a417-96b1de77c1a8";
        let path = |p: String| validate_path(&Value::from(p), sid, "shared_default");
        assert!(path(format!("/api/v3/sessions/{sid}/ui-preview/option-a")).is_ok());
        assert!(path(format!("/api/v3/sessions/{sid}/ui-preview/off")).is_ok());
        assert!(path(format!("/api/v3/sessions/{other}/ui-preview/option-a")).is_err());
        assert!(path(format!("/api/v3/sessions/{sid}/ui-preview/../x")).is_err());
        assert!(path(format!("/api/v3/sessions/{sid}/ui-preview/A")).is_err());
    }

    #[test]
    fn source_asset_route_points_to_file_read() {
        let sid = "083cd9bb-7c27-4022-a9de-09e7110a9e92";
        let fault = validate_path(
            &Value::from("/v3-ui/chat/elements/activity-group/style.css"),
            sid,
            "shared_default",
        )
        .unwrap_err();
        assert!(format!("{fault:?}").contains("file_read"));
        let api = validate_path(&Value::from("/api/v3/other"), sid, "shared_default").unwrap_err();
        assert!(format!("{api:?}").contains("Unsupported current-session read route"));
    }

    #[test]
    fn the_app_route_opens_any_well_formed_session_of_the_caller() {
        let sid = "083cd9bb-7c27-4022-a9de-09e7110a9e92";
        let other = "21e5ab5f-89d9-46ee-a417-96b1de77c1a8";
        let path = |p: String| validate_path(&Value::from(p), sid, "shared_default");
        assert!(path(format!("/app?session_id={other}")).is_ok());
        assert!(path("/app?session_id=not-a-session".into()).is_err());
        assert_eq!(path("/".into()).unwrap(), format!("/app?session_id={sid}"));
    }

    /// A URL for another session names both ids, so the caller can fix it in one step.
    #[test]
    fn a_foreign_session_route_names_the_mismatch() {
        let sid = "083cd9bb-7c27-4022-a9de-09e7110a9e92";
        let other = "94eca013-8f08-49ef-8a95-d44274439666";
        let fault = validate_path(
            &Value::from(format!("/api/v3/sessions/{other}/ui-preview/step-rail")),
            sid,
            "shared_default",
        )
        .unwrap_err();
        let text = format!("{fault:?}");
        assert!(text.contains(other), "{text}");
        assert!(text.contains(sid), "{text}");
    }
}
