//! Browser Control (`browser_read`, `browser_input`, `browser_record`), ported from the
//! Python `browser_control` package. Each chat session gets one private Chromium, driven
//! through chromedriver's WebDriver API by its own long-lived session process
//! (`webagent-v3 --browser-session`, [`session`]), so recorders and observations survive
//! between calls. The worker's [`Browsers`] pool owns the session processes: within the live
//! per-app and per-user limits, each pinned to its caller's authority and reaped on lease
//! expiry or idle time. On the server they run in the browser container, reached through
//! its [`host`] service, with Chromium's own sandbox and each one locked to its folder. Signed-out browsing goes through the [`egress`] guard; the API decides
//! the authority and approvals, and the browser enforces it again here.
mod base64;
pub mod egress;
pub mod host;
pub mod png;
mod pool;
pub mod rules;
pub mod scripts;
pub mod service;
pub mod session;
pub mod totp;
mod url;
pub mod webdriver;

pub use pool::Browsers;

use std::path::PathBuf;

/// The calls this module answers.
pub const TOOLS: [&str; 5] = [
    "browser_read",
    "browser_input",
    "browser_record",
    "administrator_artifact_get",
    "browser_protocol",
];
/// Reported on every browser result as `implementation_version`.
pub const IMPLEMENTATION_VERSION: &str = "3.0.0";

/// Whether this module answers `name`.
#[must_use]
pub fn handles(name: &str) -> bool {
    TOOLS.contains(&name)
}

pub(crate) const MAX_PNG: usize = 1024 * 1024;
pub(crate) const MAX_READ_OFFSET: i64 = 1_000_000;
pub(crate) const MAX_SCROLL: i64 = 20000;
/// Saved site sign-ins: one site's cookies at a time, and the steps that sign in again.
pub(crate) const MAX_SITE_COOKIES: usize = 150;
pub(crate) const MAX_STEPS: usize = 20;
pub(crate) const PAGE_TIMEOUT_MS: u64 = 10000;
pub(crate) const SCRIPT_TIMEOUT_MS: u64 = 5000;
/// Presets with a touch screen also emulate mobile media queries.
pub(crate) const VIEWPORTS: [(&str, u32, u32); 5] = [
    ("phone", 390, 844),
    ("phone_small", 360, 640),
    ("tablet", 768, 1024),
    ("desktop", 1280, 900),
    ("desktop_wide", 1440, 900),
];
/// WebDriver key codes; only navigation and activation keys, never text or shortcuts.
pub(crate) const KEYS: [(&str, &str); 13] = [
    ("Tab", "\u{e004}"),
    ("Shift+Tab", "\u{e008}\u{e004}"),
    ("Enter", "\u{e007}"),
    ("Escape", "\u{e00c}"),
    ("Space", " "),
    ("ArrowUp", "\u{e013}"),
    ("ArrowDown", "\u{e015}"),
    ("ArrowLeft", "\u{e012}"),
    ("ArrowRight", "\u{e014}"),
    ("Home", "\u{e011}"),
    ("End", "\u{e010}"),
    ("PageUp", "\u{e00e}"),
    ("PageDown", "\u{e00f}"),
];
pub(crate) const COLOR_SCHEMES: [(&str, &str); 3] =
    [("light", "light"), ("dark", "dark"), ("default", "")];
pub(crate) const REDUCED_MOTION: [(&str, &str); 2] = [("reduce", "reduce"), ("default", "")];

/// Why a browser call did not succeed. Messages are fixed strings, never page or
/// credential text.
#[derive(Debug)]
pub enum Fault {
    /// The request broke a rule; reported as `browser_invalid_request`.
    Rejected(String),
    /// An effect may have happened; the browser is closed and nothing is replayed.
    Uncertain(String),
    /// Chromium could not start.
    Startup,
    /// The signed-in browser could not prove the caller's identity.
    Authentication,
    /// Too many private browsers are open.
    Capacity,
    /// The app or a saved sign-in stopped at `kind`.
    NotReady {
        kind: &'static str,
        message: String,
        errors: Option<Vec<String>>,
    },
    /// WebDriver or the system failed; only the failure's type name is kept, and whether
    /// the tab crashed.
    Backend { name: String, crashed: bool },
}

impl Fault {
    pub(crate) fn rejected(message: impl Into<String>) -> Self {
        Self::Rejected(message.into())
    }

    pub(crate) fn backend(name: &str) -> Self {
        Self::Backend {
            name: name.to_owned(),
            crashed: false,
        }
    }
}

/// Where private browser state lives and which app origin a signed-in browser may use.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Private profiles and screenshots, one folder per session, outside the workspaces.
    pub root: PathBuf,
    /// The fixed app origin, without a trailing slash.
    pub origin: String,
    /// The browser container's socket (`V3_BROWSER_HOST`): each chat's browser then runs
    /// there, and the private folders are reached only through it.
    pub host: Option<PathBuf>,
    /// Chromium's own sandbox (`V3_BROWSER_SANDBOX=1`), set only where the container
    /// allows the user namespaces it needs.
    pub sandbox: bool,
}

impl Settings {
    /// `worker/browser` under the runtime root, and the origin from
    /// `V3_PUBLIC_ORIGIN` as the API uses it.
    #[must_use]
    pub fn container() -> Self {
        Self {
            root: webagent_configuration::paths::runtime().join("worker/browser"),
            origin: std::env::var("V3_PUBLIC_ORIGIN")
                .unwrap_or_else(|_| "https://dev.webagent.live".into())
                .trim_end_matches('/')
                .to_owned(),
            host: std::env::var_os("V3_BROWSER_HOST").map(PathBuf::from),
            sandbox: std::env::var_os("V3_BROWSER_SANDBOX").is_some_and(|value| value == "1"),
        }
    }

    /// What the worker passed a session process: `V3_BROWSER_ROOT` and
    /// `V3_PUBLIC_ORIGIN`, defaulting to [`Settings::container`].
    #[must_use]
    pub fn from_env() -> Self {
        let mut settings = Self::container();
        if let Ok(root) = std::env::var("V3_BROWSER_ROOT") {
            settings.root = PathBuf::from(root);
        }
        settings
    }
}

/// Unix time in seconds.
pub(crate) fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

#[cfg(test)]
mod tests;
