//! The Browser Control engine: one private Chromium per chat session; Rust owns authority
//! and approval.
//!
//! Two modes. Signed out (no cookie in the trusted authority): public http(s) sites through
//! the egress guard, filtered by the agent's allow/block lists. Signed in (the API supplies
//! a cookie when the agent navigates to the WebAgent app): the fixed app origin only.
//!
//! Signed out, a site the user saved a sign-in for opens with its saved cookies, and signs
//! in again by replaying its recorded steps when the site asks. Vault values (the API
//! resolves them from the user's vault) are typed only into sign-in fields on their own
//! origin and scrubbed from every result; the model only ever names them.
//!
//! Trusted cookie metadata is a separate dispatcher field, never a model argument. There
//! is no caller-supplied JavaScript, origin, profile or filesystem path.
use super::egress::Guard;
use super::rules::{
    canonical_id, origin_of, truthy, validate_args, validate_path, validate_trusted, validate_url,
    Scope, Site, Sites, Trusted,
};
use super::url::{quote, urlsplit, urlunsplit};
use super::webdriver::{self as wd, Driver, Element, Factory, Launch};
use super::{now, png, scripts, totp, Fault, Settings};
use super::{COLOR_SCHEMES, KEYS, REDUCED_MOTION, VIEWPORTS};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify, OnceCell};

/// One read page; small enough that the result is never cut off in the agent's context.
const MAX_TEXT: usize = 12000;
const MAX_SESSIONS: usize = 2;
/// Steps in one `script` call.
const MAX_SCRIPT_STEPS: usize = 20;
const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
/// WebDriver input failures returned before any event is dispatched.
const NOT_DISPATCHED: [&str; 3] = [
    "StaleElementReferenceException",
    "ElementClickInterceptedException",
    "ElementNotInteractableException",
];
const STEP_WAIT: Duration = Duration::from_secs(5);
const IDLE_SECONDS: u64 = 180;
/// Agent turns between a read and the input often take over 30 s; the target is
/// re-checked at dispatch anyway.
const READ_FRESH: Duration = Duration::from_secs(120);
const CALL_TIMEOUT: Duration = Duration::from_secs(20);
const SITE_CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Cloudflare injects its analytics beacon at the edge; the app's own CSP blocks it. Not an
/// app error.
const IGNORED_PAGE_ERRORS: [&str; 1] = ["static.cloudflareinsights.com"];
const MAX_PAGE_ERRORS: usize = 20;
const MAX_ELEMENTS: usize = 50;
const MEASURE_PAGE: usize = 20;
const MAX_FRAMES: usize = 30;

/// Bounds the tests shrink; the defaults are the production values.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Largest screenshot stored or served.
    pub png: usize,
    /// Screenshots kept per session; the oldest make room.
    pub artifacts: usize,
    /// How long a signed-in navigation waits for the app to finish booting.
    pub ready_ms: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            png: super::MAX_PNG,
            artifacts: 40,
            ready_ms: 10000,
        }
    }
}

/// One session's private browser.
struct State {
    sid: String,
    user: String,
    agent: String,
    generation: String,
    /// The app cookie signed in; a random value signed out keeps the scrub scripts harmless.
    cookie: String,
    csrf: String,
    expires: f64,
    driver: Arc<dyn Driver>,
    directory: PathBuf,
    touched: Instant,
    observed: bool,
    observed_url: String,
    observed_selector: String,
    observed_at: Instant,
    /// The last read's selector matched nothing; input on it reports the missing target.
    observed_missing: bool,
    viewport: Option<&'static str>,
    media: BTreeMap<String, String>,
    last_action: String,
    last_action_at: Option<Instant>,
    signed_in: bool,
    sites: Option<Sites>,
    sites_raw: Value,
    /// Signed out: the saved site whose cookies are loaded, the steps since the last
    /// navigation (what save_login keeps) and every vault value typed, scrubbed from
    /// results.
    site: Option<String>,
    steps: Vec<Value>,
    login_url: String,
    secrets: BTreeSet<String>,
}

/// A recorder: bounded PNG frames every second until stop, quota, lease or failure.
struct Recording {
    user: String,
    generation: String,
    cookie: Option<String>,
    started_at: u64,
    progress: StdMutex<Progress>,
    stop: Notify,
    stopped: std::sync::atomic::AtomicBool,
    task: StdMutex<Option<tokio::task::JoinHandle<()>>>,
}

struct Progress {
    recording: bool,
    artifacts: Vec<Value>,
    completed_at: Option<u64>,
}

impl Recording {
    fn signal(&self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.stop.notify_waiters();
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn progress(&self) -> std::sync::MutexGuard<'_, Progress> {
        self.progress.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn finish(&self) {
        let mut progress = self.progress();
        progress.recording = false;
        progress.completed_at.get_or_insert_with(|| now() as u64);
    }

    /// Ends the loop at once (a cancellation): stops it, aborts it and waits for it.
    async fn cancel(&self) {
        self.signal();
        let task = self
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        self.finish();
    }
}

/// The persistent browser service of one session process.
pub struct Service {
    origin: String,
    root: PathBuf,
    /// Chromium's own sandbox for every launch ([`Settings::sandbox`]).
    sandbox: bool,
    factory: Factory,
    limits: Limits,
    egress: OnceCell<Arc<Guard>>,
    states: Mutex<HashMap<String, State>>,
    recordings: StdMutex<HashMap<String, Arc<Recording>>>,
    record_guards: StdMutex<HashMap<String, Arc<Mutex<()>>>>,
}

fn rounded_ms(duration: Duration) -> i64 {
    (duration.as_secs_f64() * 1000.0).round_ties_even() as i64
}

fn text_of(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

fn cut(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

fn io_fault(_: std::io::Error) -> Fault {
    Fault::backend("OSError")
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// Writes `value` as a new private JSON file; an existing file is an error.
fn create_json(path: &Path, value: &Value) -> Result<(), Fault> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(io_fault)?;
    file.write_all(value.to_string().as_bytes())
        .map_err(io_fault)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(io_fault)
}

fn read_small_json(path: &Path, limit: u64, invalid: &str) -> Result<Value, Fault> {
    let meta = std::fs::symlink_metadata(path).map_err(io_fault)?;
    if meta.file_type().is_symlink() || meta.len() > limit {
        return Err(Fault::rejected(invalid));
    }
    let text = std::fs::read(path).map_err(io_fault)?;
    serde_json::from_slice(&text).map_err(|_| Fault::backend("JSONDecodeError"))
}

fn make_private_dir(path: &Path) -> Result<(), Fault> {
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        result => result.map_err(io_fault),
    }
}

/// The owner-bound private session folder outside the coding workspace, created on first
/// use with its ownership record and artifact folder.
///
/// # Errors
/// A rejection when the folder, its record or its artifacts are unsafe or another
/// caller's. Folders are deleted with their chat session, so there is no folder cap.
pub fn directory(root: &Path, sid: &str, user: &str) -> Result<PathBuf, Fault> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root)
        .map_err(io_fault)?;
    if is_symlink(root) {
        return Err(Fault::rejected("Invalid private browser root"));
    }
    let directory = root.join(sid);
    make_private_dir(&directory)?;
    if is_symlink(&directory) || !directory.is_dir() {
        return Err(Fault::rejected("Invalid private browser session"));
    }
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
        .map_err(io_fault)?;
    let owner = directory.join("owner.json");
    if owner.exists() {
        let record = read_small_json(&owner, 1024, "Invalid browser ownership record")?;
        if record["user_id"] != user {
            return Err(Fault::rejected("Private browser belongs to another caller"));
        }
    } else {
        create_json(&owner, &json!({"user_id":user,"session_id":sid}))?;
    }
    let artifacts = directory.join("artifacts");
    make_private_dir(&artifacts)?;
    if is_symlink(&artifacts) {
        return Err(Fault::rejected("Invalid artifact directory"));
    }
    Ok(directory)
}

/// The process start time from `/proc`, so a reused PID is never signalled.
fn process_stamp(pid: i32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, after) = stat.rsplit_once(')')?;
    after.split_whitespace().nth(19).map(str::to_owned)
}

/// Reaps only a recorded matching driver process left by a crash; never restores prior
/// authenticated state.
async fn cleanup_prior(directory: &Path) -> Result<(), Fault> {
    let record = directory.join("driver.json");
    if !record.exists() {
        return Ok(());
    }
    let data = read_small_json(&record, 1024, "Invalid browser process receipt")?;
    let stamp = data.get("stamp").cloned().unwrap_or(Value::Null);
    let current = |pid: i32| process_stamp(pid).map_or(Value::Null, Value::from);
    if let Some(pid) = data["pid"]
        .as_i64()
        .and_then(|pid| i32::try_from(pid).ok())
        .filter(|pid| *pid > 1)
    {
        // SAFETY: getpgid takes a plain integer.
        let leader = unsafe { libc::getpgid(pid) } == pid;
        if current(pid) == stamp && leader {
            // SAFETY: killpg takes plain integers; the start-time stamp matched, so the
            // recorded process still leads its group.
            unsafe {
                libc::killpg(pid, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(1);
            while current(pid) == stamp && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            if current(pid) == stamp {
                // SAFETY: as above.
                unsafe {
                    libc::killpg(pid, libc::SIGKILL);
                }
            }
        }
    }
    std::fs::remove_file(&record).map_err(io_fault)
}

/// Deletes one stored screenshot and its record; returns the image bytes freed.
fn evict(manifest: &Path) -> u64 {
    let image = manifest.with_extension("png");
    let mut freed = 0;
    if let Ok(meta) = std::fs::metadata(&image) {
        freed = meta.len();
        let _ = std::fs::remove_file(&image);
    }
    let _ = std::fs::remove_file(manifest);
    freed
}

/// Authenticates private artifact ownership and verifies the stored bytes before
/// returning a PNG.
///
/// # Errors
/// A rejection for a foreign, malformed or altered artifact; a backend failure when the
/// files are missing.
pub fn read_artifact(
    root: &Path,
    limits: &Limits,
    base: &Path,
    trusted: &Value,
    artifact: &Value,
) -> Result<Vec<u8>, Fault> {
    let trusted = validate_trusted(base, trusted)?;
    let artifact = artifact
        .as_str()
        .filter(|artifact| canonical_id(artifact))
        .ok_or_else(|| Fault::rejected("Invalid artifact ID"))?;
    let folder = directory(root, &trusted.session_id, &trusted.user_id)?.join("artifacts");
    let record = read_small_json(
        &folder.join(format!("{artifact}.json")),
        2048,
        "Invalid artifact record",
    )?;
    if record["user_id"] != trusted.user_id.as_str()
        || record["session_id"] != trusted.session_id.as_str()
    {
        return Err(Fault::rejected("Artifact ownership mismatch"));
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(folder.join(format!("{artifact}.png")))
        .map_err(io_fault)?;
    let info = file.metadata().map_err(io_fault)?;
    if !info.file_type().is_file() || info.nlink() != 1 || info.len() > limits.png as u64 {
        return Err(Fault::rejected("Invalid private artifact"));
    }
    let mut data = Vec::new();
    Read::by_ref(&mut file)
        .take(limits.png as u64 + 1)
        .read_to_end(&mut data)
        .map_err(io_fault)?;
    let actual = png::metadata(&data, limits.png)?;
    let Value::Object(actual) = actual else {
        return Err(Fault::rejected("Artifact digest changed"));
    };
    if actual
        .iter()
        .any(|(key, value)| record.get(key) != Some(value))
    {
        return Err(Fault::rejected("Artifact digest changed"));
    }
    Ok(data)
}

/// Every string form a page might echo a vault value in.
fn secret_forms(secret: &str) -> BTreeSet<String> {
    let json = Value::from(secret).to_string();
    let ascii: String = secret
        .chars()
        .map(|c| {
            if c.is_ascii() {
                let one = Value::from(c.to_string()).to_string();
                one[1..one.len() - 1].to_owned()
            } else {
                let mut units = [0u16; 2];
                c.encode_utf16(&mut units)
                    .iter()
                    .map(|unit| format!("\\u{unit:04x}"))
                    .collect()
            }
        })
        .collect();
    [
        secret.to_owned(),
        quote(secret, false),
        quote(secret, true),
        json[1..json.len() - 1].to_owned(),
        ascii,
    ]
    .into_iter()
    .filter(|form| !form.is_empty())
    .collect()
}

/// The focused element as a result reports it; a credential field's label stays private.
fn focus_summary(state: &State, focus: &Value, with_id: bool) -> Value {
    let label = if truthy(focus.get("secret")) {
        String::new()
    } else {
        state.redact(&text_of(focus.get("label")))
    };
    let mut used = json!({"tag":text_of(focus.get("tag")),"label":label});
    if with_id {
        used["id"] = focus.get("id").cloned().unwrap_or(Value::Null);
    }
    used
}

impl State {
    /// Removes the browser's own credentials from page-derived text.
    fn redact(&self, text: &str) -> String {
        let mut text = text.replace(&self.cookie, "[redacted]");
        for secret in &self.secrets {
            text = text.replace(secret.as_str(), "[redacted]");
        }
        if self.csrf.is_empty() {
            text
        } else {
            text.replace(&self.csrf, "[redacted]")
        }
    }

    /// Removes every vault value typed in this browser from a result, in any encoding a
    /// page might echo.
    fn scrub(&self, result: Value) -> Value {
        if self.secrets.is_empty() {
            return result;
        }
        let mut text = result.to_string();
        for secret in &self.secrets {
            for form in secret_forms(secret) {
                text = text.replace(&form, "[redacted]");
            }
        }
        serde_json::from_str(&text).unwrap_or_else(
            |_| json!({"status":"success","output":"Result withheld: it contained a vault value"}),
        )
    }

    fn driver(&self) -> &dyn Driver {
        &*self.driver
    }

    /// Starts the clock wait_for reports as since_last_action_ms.
    fn acted(&mut self, action: &str) {
        self.last_action = action.to_owned();
        self.last_action_at = Some(Instant::now());
    }

    /// Notes one input on an external site; save_login keeps them to sign in again later.
    fn step(&mut self, step: Value) {
        if !self.signed_in && self.steps.len() < super::MAX_STEPS {
            self.steps.push(step);
        }
    }

    fn fresh(&self, current: &str) -> bool {
        self.observed && self.observed_at.elapsed() <= READ_FRESH && current == self.observed_url
    }
}

impl Service {
    /// The service for `settings`, starting browsers with `factory`.
    #[must_use]
    pub fn new(settings: &Settings, factory: Factory, limits: Limits) -> Arc<Self> {
        Arc::new(Self {
            origin: settings.origin.trim_end_matches('/').to_owned(),
            root: settings.root.clone(),
            sandbox: settings.sandbox,
            factory,
            limits,
            egress: OnceCell::new(),
            states: Mutex::new(HashMap::new()),
            recordings: StdMutex::new(HashMap::new()),
            record_guards: StdMutex::new(HashMap::new()),
        })
    }

    /// The process's egress guard, started on first signed-out use.
    pub async fn egress(&self) -> Result<Arc<Guard>, Fault> {
        self.egress
            .get_or_try_init(Guard::start)
            .await
            .cloned()
            .map_err(|_| Fault::Startup)
    }

    fn recording(&self, sid: &str) -> Option<Arc<Recording>> {
        self.recordings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(sid)
            .cloned()
    }

    /// Invalidates observations and closes the driver, keeping private artifacts.
    async fn kill(state: &mut State) {
        state.observed = false;
        if let Some(pid) = state.driver.pid() {
            // SAFETY: getpgid takes a plain integer.
            if unsafe { libc::getpgid(pid) } == pid {
                // SAFETY: killpg takes plain integers; the driver leads its own group.
                unsafe {
                    libc::killpg(pid, libc::SIGTERM);
                }
            }
        }
        state.driver.quit().await;
        let _ = std::fs::remove_file(state.directory.join("driver.json"));
        let profile = state.directory.join("chrome");
        if profile.exists() && !is_symlink(&profile) {
            let _ = std::fs::remove_dir_all(&profile);
        }
    }

    async fn drop_locked(states: &mut HashMap<String, State>, sid: &str) {
        if let Some(mut state) = states.remove(sid) {
            Self::kill(&mut state).await;
        }
    }

    /// Removes one profile and closes its driver.
    pub async fn close(&self, sid: &str) {
        let mut states = self.states.lock().await;
        Self::drop_locked(&mut states, sid).await;
    }

    /// Closes idle or expired profiles before admitting new browser work.
    async fn purge(states: &mut HashMap<String, State>) {
        let stale: Vec<String> = states
            .iter()
            .filter(|(_, state)| {
                state.expires <= now() || state.touched.elapsed().as_secs() > IDLE_SECONDS
            })
            .map(|(sid, _)| sid.clone())
            .collect();
        for sid in stale {
            Self::drop_locked(states, &sid).await;
        }
    }

    /// Checks the observed URL after browser effects, rejecting cross-origin or
    /// cross-session state; returns the normalized URL.
    async fn safe_url(&self, state: &State) -> Result<String, Fault> {
        let current = wd::current_url(state.driver()).await?;
        if !state.signed_in {
            if current == "about:blank" {
                return Ok(current);
            }
            if !current.starts_with("http://") && !current.starts_with("https://") {
                return Err(Fault::rejected(
                    "No page is open, or the last navigation was blocked or failed; navigate to another URL first",
                ));
            }
            let sites = state.sites.clone().unwrap_or(Sites {
                allow: Vec::new(),
                block: Vec::new(),
            });
            return validate_url(
                &Value::from(current),
                &Scope::SignedOut { sites: &sites },
                &self.origin,
            );
        }
        let parsed = urlsplit(&current)?;
        if urlunsplit(&parsed.scheme, &parsed.netloc, "", "", "") != self.origin {
            return Err(Fault::Uncertain(
                "Browser left the fixed development origin; inspect the action before retrying"
                    .into(),
            ));
        }
        validate_path(
            &Value::from(urlunsplit("", "", &parsed.path, &parsed.query, "")),
            &state.sid,
            &state.agent,
        )
    }

    /// Authenticates a new private profile as the current caller, reusing only a browser
    /// with matching authority.
    async fn open(
        &self,
        states: &mut HashMap<String, State>,
        trusted: &Trusted,
        land: bool,
    ) -> Result<(), Fault> {
        let sid = trusted.session_id.as_str();
        Self::purge(states).await;
        if let Some(state) = states.get_mut(sid) {
            if state.user != trusted.user_id {
                return Err(Fault::rejected("Browser caller mismatch"));
            }
            let changed = state.signed_in != trusted.cookie.is_some()
                || (state.signed_in && Some(&state.cookie) != trusted.cookie.as_ref())
                || state.sites_raw != trusted.sites_raw
                || state.generation != trusted.generation
                || state.agent != trusted.agent_id;
            if !changed {
                // The API renews the lease of the same signed-in session on each call.
                state.expires = trusted.expires_at;
                return Ok(());
            }
            Self::drop_locked(states, sid).await;
        }
        if states.len() >= MAX_SESSIONS {
            return Err(Fault::Capacity);
        }
        let directory = directory(&self.root, sid, &trusted.user_id)?;
        cleanup_prior(&directory).await?;
        let profile = directory.join("chrome");
        if is_symlink(&profile) {
            return Err(Fault::rejected("Invalid browser profile"));
        }
        // Restart never restores an authenticated profile.
        if profile.exists() {
            std::fs::remove_dir_all(&profile).map_err(io_fault)?;
        }
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&profile)
            .map_err(io_fault)?;
        let started = match &trusted.cookie {
            Some(_) => {
                (self.factory)(
                    profile.clone(),
                    Some(self.origin.clone()),
                    Launch {
                        sandbox: self.sandbox,
                        ..Launch::default()
                    },
                )
                .await
            }
            None => match self.egress().await {
                Ok(guard) => {
                    let launch = Launch {
                        offline: false,
                        proxy: Some(guard.port()),
                        sandbox: self.sandbox,
                    };
                    (self.factory)(profile.clone(), None, launch).await
                }
                Err(fault) => Err(fault),
            },
        };
        let driver = match started {
            Ok(driver) => driver,
            Err(_) => {
                let _ = std::fs::remove_dir_all(&profile);
                return Err(Fault::Startup);
            }
        };
        let signed_in = trusted.cookie.is_some();
        let mut state = State {
            sid: sid.to_owned(),
            user: trusted.user_id.clone(),
            agent: trusted.agent_id.clone(),
            generation: trusted.generation.clone(),
            cookie: trusted
                .cookie
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            csrf: String::new(),
            expires: trusted.expires_at,
            driver,
            directory,
            touched: Instant::now(),
            observed: false,
            observed_url: String::new(),
            observed_selector: String::new(),
            observed_at: Instant::now(),
            observed_missing: false,
            viewport: None,
            media: BTreeMap::new(),
            last_action: String::new(),
            last_action_at: None,
            signed_in,
            sites: if signed_in {
                None
            } else {
                trusted.sites.clone()
            },
            sites_raw: if signed_in {
                Value::Null
            } else {
                trusted.sites_raw.clone()
            },
            site: None,
            steps: Vec::new(),
            login_url: String::new(),
            secrets: BTreeSet::new(),
        };
        if !signed_in {
            if let Err(fault) = Self::record_process(&state) {
                Self::kill(&mut state).await;
                return Err(fault);
            }
            states.insert(sid.to_owned(), state);
            return Ok(());
        }
        match self.sign_in(&mut state, trusted, land).await {
            Ok(()) => {
                states.insert(sid.to_owned(), state);
                Ok(())
            }
            Err(_) => {
                Self::kill(&mut state).await;
                Err(Fault::Authentication)
            }
        }
    }

    /// Signs a new signed-in browser in with the trusted cookie and verifies its identity.
    async fn sign_in(&self, state: &mut State, trusted: &Trusted, land: bool) -> Result<(), Fault> {
        Self::record_process(state)?;
        let driver = state.driver();
        wd::get(driver, &format!("{}/health/ready", self.origin)).await?;
        let first = urlsplit(&wd::current_url(driver).await?)?;
        if urlunsplit(&first.scheme, &first.netloc, "", "", "") != self.origin {
            return Err(Fault::rejected("Initial browser origin mismatch"));
        }
        wd::add_cookie(
            driver,
            json!({"name":"v3_session","value":state.cookie,"path":"/","httpOnly":true,
                "secure":self.origin.starts_with("https:"),"sameSite":"Strict",
                "expiry":state.expires as i64}),
        )
        .await?;
        let me = format!("{}/api/v3/auth/me", self.origin);
        wd::get(driver, &me).await?;
        if wd::current_url(driver).await? != me {
            return Err(Fault::rejected("Identity verification redirected"));
        }
        let body = wd::find_element(driver, "body").await?;
        let text = cut(&wd::text(driver, &body).await?, 16384);
        let identity: Value =
            serde_json::from_str(&text).map_err(|_| Fault::backend("JSONDecodeError"))?;
        if identity["user_id"] != state.user.as_str() {
            return Err(Fault::rejected("Authenticated browser identity mismatch"));
        }
        let csrf = match identity.get("csrf_token") {
            None => String::new(),
            Some(Value::String(csrf)) if csrf.chars().count() <= 128 => csrf.clone(),
            Some(_) => return Err(Fault::rejected("Invalid browser identity metadata")),
        };
        state.csrf = csrf;
        // A navigation that opened the browser loads its own route next.
        if land {
            let route =
                validate_path(&Value::from("/app"), &trusted.session_id, &trusted.agent_id)?;
            wd::get(state.driver(), &format!("{}{route}", self.origin)).await?;
            self.safe_url(state).await?;
        }
        Ok(())
    }

    /// Notes the driver process so a later start can reap it after a crash.
    fn record_process(state: &State) -> Result<(), Fault> {
        let Some(pid) = state.driver.pid() else {
            return Ok(());
        };
        let Some(stamp) = process_stamp(pid) else {
            return Ok(());
        };
        create_json(
            &state.directory.join("driver.json"),
            &json!({"pid":pid,"stamp":stamp}),
        )
    }

    /// Stores bounded PNG bytes with ownership, digest and quota metadata.
    fn artifact(&self, state: &State, data: &[u8]) -> Result<Value, Fault> {
        let mut meta = png::metadata(data, self.limits.png)?;
        let folder = state.directory.join("artifacts");
        let mut manifests = Vec::new();
        for entry in std::fs::read_dir(&folder).map_err(io_fault)? {
            let entry = entry.map_err(io_fault)?;
            let kind = entry.file_type().map_err(io_fault)?;
            if kind.is_symlink() {
                return Err(Fault::rejected("Invalid private artifact"));
            }
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                let modified = entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .map_err(io_fault)?;
                manifests.push((modified, path));
            }
        }
        // The newest screenshots stay; the oldest make room. Replies keep their own copies.
        manifests.sort();
        let mut oldest: std::collections::VecDeque<PathBuf> =
            manifests.into_iter().map(|(_, path)| path).collect();
        while oldest.len() >= self.limits.artifacts {
            if let Some(manifest) = oldest.pop_front() {
                evict(&manifest);
            }
        }
        let mut total = 0u64;
        for session in std::fs::read_dir(&self.root).map_err(io_fault)?.flatten() {
            let Ok(images) = std::fs::read_dir(session.path().join("artifacts")) else {
                continue;
            };
            for image in images.flatten() {
                let path = image.path();
                if path.extension().is_some_and(|extension| extension == "png") {
                    let meta = std::fs::symlink_metadata(&path).map_err(io_fault)?;
                    if meta.file_type().is_symlink() {
                        return Err(Fault::rejected("Invalid private artifact"));
                    }
                    total += meta.len();
                }
            }
        }
        let size = data.len() as u64;
        while total + size > MAX_ARTIFACT_BYTES {
            let Some(manifest) = oldest.pop_front() else {
                break;
            };
            total -= evict(&manifest).min(total);
        }
        if total + size > MAX_ARTIFACT_BYTES {
            return Err(Fault::rejected("Private screenshot byte quota reached"));
        }
        let artifact = uuid::Uuid::new_v4().to_string();
        let mut image = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(folder.join(format!("{artifact}.png")))
            .map_err(io_fault)?;
        image.write_all(data).map_err(io_fault)?;
        image.flush().map_err(io_fault)?;
        let mut record = meta.clone();
        record["artifact_id"] = json!(artifact);
        record["session_id"] = json!(state.sid);
        record["user_id"] = json!(state.user);
        record["created_at"] = json!(now() as u64);
        create_json(&folder.join(format!("{artifact}.json")), &record)?;
        meta["artifact_id"] = json!(artifact);
        meta["path"] = json!(format!(
            "/api/v3/sessions/{}/administrator/artifacts/{artifact}",
            state.sid
        ));
        Ok(meta)
    }

    /// Uncaught errors and console errors since the last read, credentials redacted before
    /// truncation; `None` when the log is unavailable.
    async fn page_errors(state: &State) -> Option<Vec<String>> {
        let entries = wd::browser_log(state.driver()).await.ok()?;
        let mut errors: Vec<String> = entries
            .iter()
            .filter(|entry| entry.is_object() && entry["level"] == "SEVERE")
            .filter_map(|entry| {
                let message = text_of(entry.get("message"));
                (!IGNORED_PAGE_ERRORS
                    .iter()
                    .any(|noise| message.contains(noise)))
                .then(|| cut(&state.redact(&message), 300))
            })
            .collect();
        let excess = errors.len().saturating_sub(MAX_PAGE_ERRORS);
        errors.drain(..excess);
        Some(errors)
    }

    async fn errors_value(state: &State) -> Value {
        Self::page_errors(state)
            .await
            .map_or(Value::Null, Value::from)
    }

    /// Re-applies the chosen preset and media features; device emulation does not
    /// reliably survive navigation.
    async fn apply_viewport(state: &State) -> Result<(), Fault> {
        if let Some((_, width, height)) = VIEWPORTS
            .iter()
            .find(|(name, _, _)| Some(*name) == state.viewport)
        {
            let touch = matches!(state.viewport, Some("phone" | "phone_small" | "tablet"));
            wd::cdp(
                state.driver(),
                "Emulation.setDeviceMetricsOverride",
                json!({"width":width,"height":height,"deviceScaleFactor":1,"mobile":touch}),
            )
            .await?;
            wd::cdp(
                state.driver(),
                "Emulation.setTouchEmulationEnabled",
                json!({"enabled":touch}),
            )
            .await?;
        }
        if !state.media.is_empty() {
            fn pick(
                media: &BTreeMap<String, String>,
                key: &str,
                table: &[(&str, &'static str)],
            ) -> &'static str {
                let chosen = media.get(key).map_or("default", String::as_str);
                table
                    .iter()
                    .find(|(name, _)| *name == chosen)
                    .map_or("", |(_, value)| *value)
            }
            wd::cdp(
                state.driver(),
                "Emulation.setEmulatedMedia",
                json!({"features":[
                    {"name":"prefers-color-scheme","value":pick(&state.media, "color_scheme", &COLOR_SCHEMES)},
                    {"name":"prefers-reduced-motion","value":pick(&state.media, "reduced_motion", &REDUCED_MOTION)}]}),
            )
            .await?;
        }
        Ok(())
    }

    /// A vault value for the current page: only on its own origin, as a code when it is an
    /// authenticator secret.
    async fn secret_value(
        state: &mut State,
        secrets: &BTreeMap<String, super::rules::Secret>,
        name: &str,
        totp: bool,
    ) -> Result<String, Fault> {
        let entry = secrets.get(name).ok_or_else(|| {
            Fault::rejected(
                "That vault entry is unavailable; ask the user to save it with request_credential first",
            )
        })?;
        if origin_of(&wd::current_url(state.driver()).await?) != entry.origin {
            return Err(Fault::rejected(format!(
                "That vault entry is only for {}",
                entry.origin
            )));
        }
        let value = if totp {
            totp::code(&entry.value, now())
                .ok_or_else(|| Fault::rejected("That vault entry is not an authenticator secret"))?
        } else {
            entry.value.clone()
        };
        state.secrets.insert(entry.value.clone());
        state.secrets.insert(value.clone());
        Ok(value)
    }

    /// The browser's cookies for one site's host and its parent domains, in the fields
    /// `Network.setCookies` takes.
    async fn site_cookies(state: &State, origin: &str) -> Result<Vec<Value>, Fault> {
        let host = urlsplit(origin)?.hostname().unwrap_or_default();
        let all = wd::cdp(state.driver(), "Network.getAllCookies", json!({})).await?;
        let mut kept = Vec::new();
        for cookie in all["cookies"].as_array().into_iter().flatten() {
            let domain = text_of(cookie.get("domain"))
                .trim_start_matches('.')
                .to_lowercase();
            if domain.is_empty() || !(host == domain || host.ends_with(&format!(".{domain}"))) {
                continue;
            }
            let mut item = Map::new();
            for key in ["name", "value", "domain", "path", "httpOnly", "secure"] {
                if let Some(value) = cookie.get(key) {
                    item.insert(key.to_owned(), value.clone());
                }
            }
            if let Some(same) = cookie
                .get("sameSite")
                .filter(|same| ["Strict", "Lax", "None"].iter().any(|kind| *same == *kind))
            {
                item.insert("sameSite".into(), same.clone());
            }
            if !truthy(cookie.get("session")) {
                if let Some(expires) = cookie
                    .get("expires")
                    .filter(|expires| expires.is_number() && expires.as_f64() > Some(0.0))
                {
                    item.insert("expires".into(), expires.clone());
                }
            }
            kept.push(Value::Object(item));
        }
        kept.truncate(super::MAX_SITE_COOKIES);
        Ok(kept)
    }

    /// Keeps at most one saved site's cookies in the browser: the one being opened.
    /// Staying on it keeps its live cookies.
    async fn restore_site(state: &mut State, site: Option<&Site>) -> Result<(), Fault> {
        let origin = site.map(|site| site.origin.clone());
        if state.site == origin {
            return Ok(());
        }
        wd::cdp(state.driver(), "Network.clearBrowserCookies", json!({})).await?;
        state.site = origin;
        if let Some(site) = site.filter(|site| !site.cookies.is_empty()) {
            wd::cdp(
                state.driver(),
                "Network.setCookies",
                json!({"cookies":site.cookies}),
            )
            .await?;
        }
        Ok(())
    }

    /// Repeats one saved sign-in step, waiting a few seconds for its field to appear.
    async fn replay(
        state: &mut State,
        step: &Value,
        secrets: &BTreeMap<String, super::rules::Secret>,
    ) -> Result<(), Fault> {
        let deadline = Instant::now() + STEP_WAIT;
        let found = loop {
            let mut found = Vec::new();
            if truthy(step.get("selector")) {
                let selector = text_of(step.get("selector"));
                for element in wd::find_elements(state.driver(), &selector).await? {
                    if wd::displayed(state.driver(), &element).await?
                        && wd::enabled(state.driver(), &element).await?
                    {
                        found.push(element);
                    }
                }
            } else {
                found.push(wd::active_element(state.driver()).await?);
            }
            if !found.is_empty() || Instant::now() >= deadline {
                break found;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let Some(element) = found.into_iter().next() else {
            return Err(Fault::NotReady {
                kind: "signin_failed",
                message: "A saved sign-in step's field did not appear; the site may have changed. Sign in step by step and save_login again".into(),
                errors: Self::page_errors(state).await,
            });
        };
        let invalid = || Fault::NotReady {
            kind: "signin_failed",
            message: "A saved sign-in step is invalid; sign in step by step and save_login again"
                .into(),
            errors: None,
        };
        let key = step["key"]
            .as_str()
            .and_then(|key| KEYS.iter().find(|(name, _)| *name == key));
        match step["action"].as_str() {
            Some("click") => wd::click(state.driver(), &element).await,
            Some("press") if key.is_some() => {
                let code = key.map_or("", |(_, code)| *code);
                wd::send_keys(state.driver(), &element, code).await
            }
            Some("type") if step.get("vault").is_some() => {
                let credential = wd::execute(
                    state.driver(),
                    scripts::CREDENTIAL,
                    vec![element.reference()],
                )
                .await?;
                if credential != true {
                    return Err(Fault::NotReady {
                        kind: "signin_failed",
                        message: "A saved step would type a vault value outside a sign-in field; sign in step by step and save_login again".into(),
                        errors: None,
                    });
                }
                let name = text_of(step.get("vault"));
                let value = Self::secret_value(state, secrets, &name, step["totp"] == true).await?;
                wd::send_keys(state.driver(), &element, &value).await
            }
            Some("type") if step["text"].is_string() => {
                let text = step["text"].as_str().unwrap_or_default();
                wd::send_keys(state.driver(), &element, text).await
            }
            _ => Err(invalid()),
        }
    }

    /// After opening a saved site: signed in, or sign in again with its steps once and
    /// report how it went.
    async fn site_signed_in(
        state: &mut State,
        site: &Site,
        url: &str,
        secrets: &BTreeMap<String, super::rules::Secret>,
    ) -> Result<Value, Fault> {
        let signin = |state: &State| {
            let driver = Arc::clone(&state.driver);
            async move {
                wd::execute(&*driver, scripts::SIGNIN, Vec::new())
                    .await
                    .map(|value| truthy(Some(&value)))
            }
        };
        let origin = &site.origin;
        if !signin(state).await? {
            return Ok(json!({"signed_in":true,"site":origin,"site_login":"restored"}));
        }
        if site.steps.is_empty() {
            return Ok(
                json!({"signed_in":false,"site":origin,"site_login":"expired",
                "next_step":"The saved sign-in expired and has no steps to sign in again: sign in step by step, then save_login"}),
            );
        }
        wd::get(state.driver(), &site.login_url).await?;
        for step in &site.steps {
            Self::replay(state, step, secrets).await?;
        }
        let deadline = Instant::now() + STEP_WAIT;
        while signin(state).await? && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        wd::get(state.driver(), url).await?;
        if origin_of(&wd::current_url(state.driver()).await?) != *origin || signin(state).await? {
            return Err(Fault::NotReady {
                kind: "signin_failed",
                message: "Signing in again with the saved steps did not work; the site may have changed or want a new code. Sign in step by step and save_login again".into(),
                errors: Self::page_errors(state).await,
            });
        }
        Ok(
            json!({"signed_in":true,"site":origin,"site_login":"signed_in_again",
            "site_cookies":Self::site_cookies(state, origin).await?}),
        )
    }

    /// Polls one fixed condition every 50 ms until it holds or the bounded timeout ends.
    async fn wait_for(state: &State, args: &Map<String, Value>) -> Result<Value, Fault> {
        let started = Instant::now();
        let timeout = args
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(5000);
        let deadline = started + Duration::from_millis(timeout);
        let condition = text_of(args.get("condition"));
        let call = vec![
            Value::from(condition.clone()),
            Value::from(text_of(args.get("selector"))),
            Value::from(text_of(args.get("text"))),
            args.get("quiet_ms").cloned().unwrap_or_else(|| json!(500)),
        ];
        let met = loop {
            let met = wd::execute(state.driver(), scripts::CHECK, call.clone()).await? == true;
            if met || Instant::now() >= deadline {
                break met;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let now = Instant::now();
        Ok(
            json!({"condition":condition,"met":met,"elapsed_ms":rounded_ms(now - started),
            "last_action":(!state.last_action.is_empty()).then_some(&state.last_action),
            "since_last_action_ms":state.last_action_at.map(|at| rounded_ms(now - at))}),
        )
    }

    /// Waits for the app's boot to finish signed in, or says where it stopped.
    async fn app_ready(&self, state: &State) -> Result<Value, Fault> {
        let started = Instant::now();
        let deadline = started + Duration::from_millis(self.limits.ready_ms);
        loop {
            let stage = wd::execute(state.driver(), scripts::READY, Vec::new()).await?;
            if stage == "ready" {
                return Ok(json!({"ready":true,"ready_ms":rounded_ms(started.elapsed())}));
            }
            if stage == "signed_out" {
                return Err(Fault::NotReady {
                    kind: "app_signed_out",
                    message: "The app opened its sign-in page instead of signing in; navigate to the app again".into(),
                    errors: Self::page_errors(state).await,
                });
            }
            if Instant::now() >= deadline {
                return Err(Fault::NotReady {
                    kind: "app_boot_stalled",
                    message: format!(
                        "The app did not finish booting within {} s; page_errors shows what failed",
                        self.limits.ready_ms / 1000
                    ),
                    errors: Self::page_errors(state).await,
                });
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn mask(state: &State) -> Result<(), Fault> {
        wd::execute(
            state.driver(),
            scripts::MASK,
            vec![
                Value::from(state.cookie.clone()),
                Value::from(state.csrf.clone()),
            ],
        )
        .await
        .map(drop)
    }

    async fn unmask(state: &State) {
        let _ = wd::execute(state.driver(), scripts::UNMASK, Vec::new()).await;
    }

    /// Rendered colours at viewport points, each from a 1x1 capture with the screenshot
    /// masks applied.
    async fn pixels(state: &State, args: &Map<String, Value>) -> Result<Value, Fault> {
        let mut points: Vec<(i64, i64)> = args
            .get("points")
            .and_then(Value::as_array)
            .map(|points| {
                points
                    .iter()
                    .map(|point| {
                        (
                            point["x"].as_i64().unwrap_or(0),
                            point["y"].as_i64().unwrap_or(0),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        if points.is_empty() {
            let center = wd::execute(
                state.driver(),
                scripts::CENTER,
                vec![args.get("selector").cloned().unwrap_or(Value::Null)],
            )
            .await?;
            if !center.is_object() {
                return Err(Fault::rejected("No rendered element matches the selector"));
            }
            let whole = |value: &Value| value.as_f64().map_or(0, |value| value.trunc() as i64);
            points = vec![(whole(&center["x"]), whole(&center["y"]))];
        }
        Self::apply_viewport(state).await?;
        let sampled = async {
            Self::mask(state).await?;
            let mut samples = Vec::new();
            for (x, y) in points {
                let shot = wd::cdp(
                    state.driver(),
                    "Page.captureScreenshot",
                    json!({"format":"png","captureBeyondViewport":false,
                        "clip":{"x":x,"y":y,"width":1,"height":1,"scale":1}}),
                )
                .await?;
                let data = super::base64::decode(shot["data"].as_str().unwrap_or_default())
                    .ok_or_else(|| Fault::backend("Error"))?;
                let [red, green, blue] = png::pixel(&data)?;
                samples.push(json!({"x":x,"y":y,
                    "color":format!("#{red:02x}{green:02x}{blue:02x}"),"rgb":[red,green,blue]}));
            }
            Ok::<_, Fault>(samples)
        }
        .await;
        Self::unmask(state).await;
        Ok(json!({"pixels":sampled?,"masked_color":"#555555"}))
    }

    /// Masks credentials and editable content, captures a PNG, and invalidates the prior
    /// observation.
    async fn screenshot(&self, state: &mut State) -> Result<Value, Fault> {
        let captured = async {
            Self::apply_viewport(state).await?;
            Self::mask(state).await?;
            let data = wd::screenshot(state.driver()).await?;
            self.artifact(state, &data)
        }
        .await;
        Self::unmask(state).await;
        state.observed = false;
        captured
    }

    /// Requires a recent read of this URL whose region holds the target, then re-checks
    /// the target itself at dispatch.
    async fn fresh_target(
        &self,
        state: &State,
        selector: &str,
        vault: bool,
        keys: bool,
    ) -> Result<(Element, Value), Fault> {
        let current = wd::current_url(state.driver()).await?;
        if state.observed_missing
            && state.observed_selector == selector
            && state.observed_url == current
        {
            return Err(Fault::rejected(format!(
                "Target `{selector}` not found on the page; check the selector, or wait_for it to appear"
            )));
        }
        if !state.fresh(&current) {
            return Err(Fault::rejected(
                "Read the current page (a region containing the target) before each input action",
            ));
        }
        let target = wd::execute(
            state.driver(),
            scripts::TARGET,
            vec![
                Value::from(selector),
                Value::from(state.observed_selector.clone()),
            ],
        )
        .await?;
        if !target.is_object() {
            return Err(Fault::backend("AttributeError"));
        }
        if !truthy(target.get("allowed")) {
            return Err(Fault::rejected(
                "Target is not in the region last read; read a selector that contains it",
            ));
        }
        if vault && !truthy(target.get("credential")) {
            return Err(Fault::rejected(
                "Vault values go only into a sign-in field: user name, email, password or one-time code",
            ));
        }
        // Navigation keys type nothing, so Enter may submit a password field.
        if truthy(target.get("secret")) && !(vault || keys) {
            return Err(Fault::rejected(
                "Target contains private credentials; type a vault entry into it instead",
            ));
        }
        let href = text_of(target.get("href"));
        if truthy(target.get("href")) && !state.signed_in {
            if href.starts_with("http://") || href.starts_with("https://") {
                let sites = state.sites.clone().unwrap_or(Sites {
                    allow: Vec::new(),
                    block: Vec::new(),
                });
                validate_url(
                    &Value::from(href),
                    &Scope::SignedOut { sites: &sites },
                    &self.origin,
                )?;
            }
        } else if truthy(target.get("href")) {
            let parsed = urlsplit(&href)?;
            if urlunsplit(&parsed.scheme, &parsed.netloc, "", "", "") != self.origin {
                return Err(Fault::rejected("External navigation is unavailable"));
            }
            validate_path(
                &Value::from(urlunsplit("", "", &parsed.path, &parsed.query, "")),
                &state.sid,
                &state.agent,
            )?;
        }
        let element = wd::find_element(state.driver(), selector).await?;
        if !wd::displayed(state.driver(), &element).await?
            || !wd::enabled(state.driver(), &element).await?
        {
            return Err(Fault::rejected(
                "Input target is not visible and enabled; it may be inside a closed menu or panel, or disabled",
            ));
        }
        let used = json!({"tag":text_of(target.get("tag")),
            "label":state.redact(&text_of(target.get("label")))});
        Ok((element, used))
    }

    /// Runs one validated action with serialized profile access and no automatic input
    /// replay.
    async fn run(
        self: &Arc<Self>,
        name: &str,
        args: &Value,
        base: &Path,
        trusted: &Value,
    ) -> Result<Value, Fault> {
        let trusted = validate_trusted(base, trusted)?;
        let Value::Object(args) = args else {
            return Err(Fault::rejected("Browser arguments must be an object"));
        };
        let mut args = args.clone();
        let action = validate_args(name, &mut args)?;
        // Reject a route before starting Chromium or causing navigation.
        if action == "navigate" {
            validate_url(&args["url"], &trusted.scope(), &self.origin)?;
        }
        let sid = trusted.session_id.clone();
        let mut states = self.states.lock().await;
        Self::purge(&mut states).await;
        if action == "close" {
            if let Some(record) = self.recording(&sid) {
                record.signal();
            }
            Self::drop_locked(&mut states, &sid).await;
            return Ok(json!({"status":"success","closed":true}));
        }
        self.open(&mut states, &trusted, action != "navigate")
            .await?;
        let state = states
            .get_mut(&sid)
            .ok_or_else(|| Fault::rejected("Browser authorization expired"))?;
        if state.expires <= now() {
            return Err(Fault::rejected("Browser authorization expired"));
        }
        // Navigation may leave a page whose URL is outside the routes; other actions may not
        // act on one.
        state.touched = Instant::now();
        // Viewport and emulation only set how pages render, so they may come before the first
        // navigate.
        if !matches!(action, "navigate" | "viewport" | "emulate") {
            self.safe_url(state).await?;
        }
        let result = self.act(state, action, &args, &trusted).await?;
        let mut out = json!({"status":"success","backend":"private_chromium",
            "signed_in":state.signed_in,"session_id":state.sid,
            "source_kind":"untrusted_browser_observation"});
        if let (Value::Object(out), Value::Object(result)) = (&mut out, result) {
            out.extend(result);
        }
        Ok(state.scrub(out))
    }

    async fn act(
        &self,
        state: &mut State,
        action: &str,
        args: &Map<String, Value>,
        trusted: &Trusted,
    ) -> Result<Value, Fault> {
        let selector = args.get("selector").and_then(Value::as_str);
        Ok(match action {
            "navigate" if state.signed_in => {
                let route = validate_url(&args["url"], &trusted.scope(), &self.origin)?;
                state.observed = false;
                state.observed_missing = false;
                let url = format!("{}{route}", self.origin);
                wd::get(state.driver(), &url).await?;
                state.acted("navigate");
                Self::apply_viewport(state).await?;
                self.safe_url(state).await?;
                let mut result = json!({"url":url,"signed_in":true,"identity_verified":true,
                    "user_id":state.user});
                if !route.starts_with("/api/") {
                    if let (Value::Object(result), Value::Object(ready)) =
                        (&mut result, self.app_ready(state).await?)
                    {
                        result.extend(ready);
                    }
                }
                result
            }
            "navigate" => {
                let url = validate_url(&args["url"], &trusted.scope(), &self.origin)?;
                let site = trusted.site.as_ref();
                state.observed = false;
                state.observed_missing = false;
                Self::restore_site(state, site).await?;
                wd::get(state.driver(), &url).await?;
                state.acted("navigate");
                Self::apply_viewport(state).await?;
                let current = wd::current_url(state.driver()).await?;
                if !current.starts_with("http://") && !current.starts_with("https://") {
                    let host = urlsplit(&url)?.hostname().unwrap_or_default();
                    let reason = self.egress.get().and_then(|guard| guard.reason(&host));
                    return Err(Fault::rejected(match reason {
                        Some(reason) => format!("Blocked: {host} \u{2014} {reason}"),
                        None => "The page could not be loaded (blocked, unreachable or not found)"
                            .into(),
                    }));
                }
                let mut result = json!({"url":self.safe_url(state).await?,"signed_in":false});
                state.steps.clear();
                state.login_url = result["url"].as_str().unwrap_or_default().to_owned();
                if let Some(site) = site {
                    let signed = Self::site_signed_in(state, site, &url, &trusted.secrets).await?;
                    if let (Value::Object(result), Value::Object(signed)) = (&mut result, signed) {
                        result.extend(signed);
                    }
                    result["url"] = json!(self.safe_url(state).await?);
                }
                result
            }
            "read" => {
                let read = wd::execute(
                    state.driver(),
                    scripts::READ,
                    vec![
                        Value::from(selector.unwrap_or("body")),
                        Value::from(state.cookie.clone()),
                        json!(MAX_TEXT),
                        args.get("offset").cloned().unwrap_or_else(|| json!(0)),
                    ],
                )
                .await?;
                let Value::Object(mut read) = read else {
                    return Err(Fault::rejected("Invalid bounded DOM result"));
                };
                let Some(text) = read.get("text").and_then(Value::as_str) else {
                    return Err(Fault::rejected("Invalid bounded DOM result"));
                };
                let mut text = cut(text, MAX_TEXT).replace(&state.cookie, "[redacted]");
                if !state.csrf.is_empty() {
                    text = text.replace(&state.csrf, "[redacted]");
                }
                read.insert("text".into(), Value::from(text));
                // Any read that found its region permits input inside that region until
                // navigation or input.
                let missing = truthy(read.get("missing"));
                state.observed = !missing;
                state.observed_missing = missing;
                state.observed_url = wd::current_url(state.driver()).await?;
                state.observed_selector = selector.unwrap_or("body").to_owned();
                state.observed_at = Instant::now();
                // Errors first, so a long page never pushes them out of view.
                let mut result = Map::new();
                result.insert("page_errors".into(), Self::errors_value(state).await);
                result.extend(read);
                result.insert("fresh_for_input".into(), Value::from(state.observed));
                // The app keeps the open conversation in its route, so report it for checks.
                // A route the rules do not accept is left out; it never fails the read.
                if let Ok(url) = self.safe_url(state).await {
                    result.insert("url".into(), Value::from(url));
                }
                Value::Object(result)
            }
            "click" | "type" => {
                let vault = args.get("vault").and_then(Value::as_str);
                if vault.is_some() && state.signed_in {
                    return Err(Fault::rejected(
                        "Vault values are for external sites; the WebAgent app signs in by itself",
                    ));
                }
                let selector = selector.unwrap_or_default();
                let (mut target, mut used) = self
                    .fresh_target(state, selector, vault.is_some(), false)
                    .await?;
                state.observed = false;
                let totp = args.get("totp") == Some(&Value::Bool(true));
                let text = match vault {
                    Some(vault) => {
                        Some(Self::secret_value(state, &trusted.secrets, vault, totp).await?)
                    }
                    None => args.get("text").and_then(Value::as_str).map(str::to_owned),
                };
                let mut redrawn = false;
                let applied = loop {
                    let sent = if action == "click" {
                        wd::click(state.driver(), &target).await
                    } else {
                        wd::send_keys(state.driver(), &target, text.as_deref().unwrap_or("")).await
                    };
                    match sent {
                        Ok(()) => {
                            state.acted(action);
                            break self.safe_url(state).await.map(drop).map_err(drop);
                        }
                        // WebDriver refuses these before dispatching any input, so nothing
                        // happened. A live page that redrew the target is looked up once more
                        // under the same rules; the others are ordinary refusals.
                        Err(Fault::Backend {
                            name,
                            crashed: false,
                        }) if NOT_DISPATCHED.contains(&name.as_str()) => {
                            if name == "StaleElementReferenceException" && !redrawn {
                                redrawn = true;
                                state.observed = true;
                                (target, used) = self
                                    .fresh_target(state, selector, vault.is_some(), false)
                                    .await?;
                                state.observed = false;
                                continue;
                            }
                            return Err(Fault::rejected(
                                "The target changed, is covered or cannot take input; nothing was \
                                 applied. Read the page and try again",
                            ));
                        }
                        Err(_) => break Err(()),
                    }
                };
                if applied.is_err() {
                    return Err(Fault::Uncertain(
                        "Input outcome is uncertain; inspect the page before another action".into(),
                    ));
                }
                let mut step = json!({"action":action,"selector":selector});
                if let Some(vault) = vault {
                    step["vault"] = json!(vault);
                    step["totp"] = json!(totp);
                } else if action == "type" {
                    step["text"] = json!(text);
                }
                state.step(step);
                let mut result = json!({"action":action,"applied":true,"target":used,
                    "read_required":true});
                if let Some(vault) = vault {
                    result["vault"] = json!(vault);
                }
                result
            }
            "save_login" => {
                if state.signed_in {
                    return Err(Fault::rejected(
                        "save_login is for external sites; the WebAgent app signs in by itself",
                    ));
                }
                let origin = text_of(args.get("origin"));
                if origin_of(&wd::current_url(state.driver()).await?) != origin {
                    return Err(Fault::rejected(format!(
                        "The page is not on {origin}; finish signing in there first"
                    )));
                }
                let cookies = Self::site_cookies(state, &origin).await?;
                if cookies.is_empty() {
                    return Err(Fault::rejected(
                        "The site has set no cookies to save yet; finish signing in first",
                    ));
                }
                let steps = if origin_of(&state.login_url) == origin {
                    state.steps.clone()
                } else {
                    Vec::new()
                };
                state.site = Some(origin.clone());
                // The API keeps site_login in the user's store and returns only the counts
                // to the agent.
                let login_url = if steps.is_empty() {
                    format!("{origin}/")
                } else {
                    state.login_url.clone()
                };
                json!({"action":"save_login","origin":origin,"cookies":cookies.len(),
                    "steps":steps.len(),"site_login":{"origin":origin,"cookies":cookies,
                    "steps":steps,"login_url":login_url}})
            }
            "screenshot" => {
                let mut shot = self.screenshot(state).await?;
                // The user's name for this image; it becomes the attachment's file name.
                let label: String = args
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .chars()
                    .filter(|c| c.is_alphanumeric() || " -_.,()·:".contains(*c))
                    .take(80)
                    .collect();
                if !label.trim().is_empty() {
                    shot["label"] = json!(label.trim());
                }
                shot
            }
            "viewport" => {
                // Device emulation changes width, touch and mobile media queries; navigate
                // again so boot-time layout reruns.
                let preset = VIEWPORTS
                    .iter()
                    .find(|(name, _, _)| args.get("preset") == Some(&Value::from(*name)))
                    .copied()
                    .ok_or_else(|| Fault::rejected("Viewport preset must be one of phone"))?;
                state.viewport = Some(preset.0);
                Self::apply_viewport(state).await?;
                state.observed = false;
                json!({"viewport":preset.0,"width":preset.1,"height":preset.2,
                    "navigate_required":true})
            }
            "measure" => {
                // Styles are about 2 KB per element, so a styled page holds fewer elements.
                let styles = args.get("styles") == Some(&Value::Bool(true));
                let mut result = wd::execute(
                    state.driver(),
                    scripts::MEASURE,
                    vec![
                        args.get("selector").cloned().unwrap_or(Value::Null),
                        json!(if styles { 5 } else { MEASURE_PAGE }),
                        args.get("offset").cloned().unwrap_or_else(|| json!(0)),
                        Value::Bool(styles),
                    ],
                )
                .await?;
                if !result.is_object() || !result["elements"].is_array() {
                    return Err(Fault::rejected("Invalid bounded layout result"));
                }
                let redact = |rows: Option<&mut Value>| -> Result<(), Fault> {
                    let rows = rows
                        .and_then(Value::as_array_mut)
                        .ok_or_else(|| Fault::backend("KeyError"))?;
                    for row in rows {
                        let Value::Object(row) = row else {
                            return Err(Fault::backend("AttributeError"));
                        };
                        let label = state.redact(&text_of(row.get("label")));
                        row.insert("label".into(), Value::from(label));
                    }
                    Ok(())
                };
                redact(result.get_mut("elements"))?;
                redact(result.pointer_mut("/checks/under_44px/smallest"))?;
                redact(result.pointer_mut("/checks/content_overflowing/first"))?;
                result
            }
            "pixels" => Self::pixels(state, args).await?,
            "performance" => {
                let result = wd::execute(state.driver(), scripts::PERFORMANCE, Vec::new()).await?;
                if !result.is_object() {
                    return Err(Fault::rejected("Invalid bounded performance result"));
                }
                result
            }
            "animations" => {
                let result = wd::execute(
                    state.driver(),
                    scripts::ANIMATIONS,
                    vec![Value::from(selector.unwrap_or("")), json!(MAX_ELEMENTS)],
                )
                .await?;
                if !result.is_object() || !result["animations"].is_array() {
                    return Err(Fault::rejected("Invalid bounded animation result"));
                }
                result
            }
            "wait_for" => Self::wait_for(state, args).await?,
            "scroll" => {
                let result = wd::execute(
                    state.driver(),
                    scripts::SCROLL,
                    vec![
                        Value::from(selector.unwrap_or("")),
                        args.get("dx").cloned().unwrap_or_else(|| json!(0)),
                        args.get("dy").cloned().unwrap_or_else(|| json!(0)),
                    ],
                )
                .await?;
                if !result.is_object() {
                    return Err(Fault::rejected("Invalid bounded scroll result"));
                }
                if truthy(result.get("missing")) {
                    return Err(Fault::rejected("Scroll target does not exist"));
                }
                state.acted("scroll");
                result
            }
            "press" => self.press(state, args, selector).await?,
            "touch" => {
                self.touch(state, args, selector.unwrap_or_default())
                    .await?
            }
            "emulate" => {
                for key in ["color_scheme", "reduced_motion"] {
                    if let Some(value) = args.get(key).and_then(Value::as_str) {
                        state.media.insert(key.to_owned(), value.to_owned());
                    }
                }
                Self::apply_viewport(state).await?;
                state.observed = false;
                let pick = |key: &str| state.media.get(key).map_or("default", String::as_str);
                json!({"color_scheme":pick("color_scheme"),"reduced_motion":pick("reduced_motion")})
            }
            _ => {
                return Err(Fault::rejected(
                    "Recorder actions use the recorder interface",
                ))
            }
        })
    }

    /// A key is input: it needs a fresh read like a click. Keys type no text, so a sign-in
    /// field may receive one.
    async fn press(
        &self,
        state: &mut State,
        args: &Map<String, Value>,
        selector: Option<&str>,
    ) -> Result<Value, Fault> {
        let (target, used) = match selector {
            Some(selector) => self.fresh_target(state, selector, false, true).await?,
            None => {
                if !state.fresh(&wd::current_url(state.driver()).await?) {
                    return Err(Fault::rejected(
                        "Read the current page before pressing a key",
                    ));
                }
                let focus = wd::execute(state.driver(), scripts::FOCUS, Vec::new()).await?;
                if !focus.is_object() {
                    return Err(Fault::rejected("Invalid focus result"));
                }
                let target = wd::active_element(state.driver()).await?;
                (target, focus_summary(state, &focus, false))
            }
        };
        state.observed = false;
        let key = text_of(args.get("key"));
        let code = KEYS
            .iter()
            .find(|(name, _)| *name == key)
            .map_or("", |(_, code)| *code);
        let applied = async {
            wd::send_keys(state.driver(), &target, code).await?;
            state.acted("press");
            self.safe_url(state).await
        }
        .await;
        if applied.is_err() {
            return Err(Fault::Uncertain(
                "Key outcome is uncertain; inspect the page before another action".into(),
            ));
        }
        let mut step = json!({"action":"press","key":key});
        if let Some(selector) = selector {
            step["selector"] = json!(selector);
        }
        state.step(step);
        let focus = wd::execute(state.driver(), scripts::FOCUS, Vec::new()).await?;
        let focused = if focus.is_object() {
            focus_summary(state, &focus, true)
        } else {
            Value::Null
        };
        Ok(
            json!({"action":"press","key":key,"applied":true,"target":used,
            "focused":focused,"read_required":true}),
        )
    }

    /// A real (trusted) touch on `selector`: a tap, or with `dx`/`dy` a drag, dispatched through
    /// the DevTools input domain so native scrolling and focus changes happen as with a finger.
    /// Reports the element focused afterwards and the target's scroll offsets before and after.
    async fn touch(
        &self,
        state: &mut State,
        args: &Map<String, Value>,
        selector: &str,
    ) -> Result<Value, Fault> {
        if !matches!(state.viewport, Some("phone" | "phone_small" | "tablet")) {
            return Err(Fault::rejected(
                "touch needs a touch viewport: set viewport phone or tablet first",
            ));
        }
        let (_target, used) = self.fresh_target(state, selector, false, true).await?;
        state.observed = false;
        let before =
            wd::execute(state.driver(), scripts::TOUCH_TARGET, vec![json!(selector)]).await?;
        if !before.is_object() {
            return Err(Fault::rejected("No rendered element matches the selector"));
        }
        let number = |value: &Value| value.as_f64().unwrap_or(0.0);
        let (x, y) = (number(&before["x"]), number(&before["y"]));
        let dx = args.get("dx").and_then(Value::as_f64).unwrap_or(0.0);
        let dy = args.get("dy").and_then(Value::as_f64).unwrap_or(0.0);
        let moves = if dx == 0.0 && dy == 0.0 { 0 } else { 10 };
        let point = |step: i32| {
            let share = f64::from(step) / 10.0;
            json!([{"x":x + dx * share,"y":y + dy * share}])
        };
        let applied = async {
            let driver = state.driver();
            wd::cdp(
                driver,
                "Input.dispatchTouchEvent",
                json!({"type":"touchStart","touchPoints":point(0)}),
            )
            .await?;
            for step in 1..=moves {
                wd::cdp(
                    driver,
                    "Input.dispatchTouchEvent",
                    json!({"type":"touchMove","touchPoints":point(step)}),
                )
                .await?;
            }
            wd::cdp(
                driver,
                "Input.dispatchTouchEvent",
                json!({"type":"touchEnd","touchPoints":[]}),
            )
            .await?;
            Ok::<(), Fault>(())
        }
        .await;
        if applied.is_err() {
            return Err(Fault::Uncertain(
                "Touch outcome is uncertain; inspect the page before another action".into(),
            ));
        }
        state.acted("touch");
        self.safe_url(state).await?;
        let mut step = json!({"action":"touch","selector":selector});
        if moves > 0 {
            step["dx"] = json!(dx);
            step["dy"] = json!(dy);
        }
        state.step(step);
        let after =
            wd::execute(state.driver(), scripts::TOUCH_TARGET, vec![json!(selector)]).await?;
        let focus = wd::execute(state.driver(), scripts::FOCUS, Vec::new()).await?;
        let focused = if focus.is_object() {
            focus_summary(state, &focus, true)
        } else {
            Value::Null
        };
        let scroll = |value: &Value| json!({"left":value["scroll_left"],"top":value["scroll_top"]});
        Ok(
            json!({"action":"touch","applied":true,"target":used,"focused":focused,
            "scroll_before":scroll(&before),"scroll_after":scroll(&after),"read_required":true}),
        )
    }

    /// Runs one browser tool call, or a `script` of them, and publishes only sanitized
    /// known or uncertain outcomes.
    pub async fn call(
        self: &Arc<Self>,
        name: &str,
        args: &Value,
        base: &Path,
        trusted: &Value,
    ) -> Value {
        if args.get("action").and_then(Value::as_str) == Some("script") {
            return self.script(name, args, base, trusted).await;
        }
        self.call_one(name, args, base, trusted).await
    }

    /// Runs `steps` in order, each as its own call with its own time bound and checks.
    /// Only steps marked `screenshot` return an image (taken after the step's action); a
    /// failed step stops the script and returns a screenshot of that moment. Input steps
    /// run only in a `browser_input` script, so that call's approval covers them.
    async fn script(
        self: &Arc<Self>,
        name: &str,
        args: &Value,
        base: &Path,
        trusted: &Value,
    ) -> Value {
        let rejected = |message: String| {
            json!({"status":"error","error_kind":"browser_invalid_request",
                "output":format!("Private browser request rejected: {message}")})
        };
        let Some(steps) = args
            .get("steps")
            .and_then(Value::as_array)
            .filter(|steps| (1..=MAX_SCRIPT_STEPS).contains(&steps.len()))
        else {
            return rejected(format!("a script needs 1 to {MAX_SCRIPT_STEPS} steps"));
        };
        let mut results = Vec::new();
        let mut artifacts: Vec<Value> = Vec::new();
        for (index, step) in steps.iter().enumerate() {
            let Some(mut step) = step.as_object().cloned() else {
                return rejected(format!("step {index} must be an object"));
            };
            let shot = step.remove("screenshot") == Some(Value::Bool(true));
            // A step's label names the screenshot taken after it; only a screenshot step keeps it.
            let label = if step.get("action").and_then(Value::as_str) == Some("screenshot") {
                None
            } else {
                step.remove("label")
            };
            let action = step
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let tool = if super::rules::READ_ACTIONS.contains(&action.as_str()) {
                "browser_read"
            } else if name == "browser_input"
                && matches!(action.as_str(), "click" | "type" | "press" | "touch")
            {
                "browser_input"
            } else {
                return rejected(format!(
                    "step {index}: {action} is not a {name} script action"
                ));
            };
            // An input step needs a fresh read of its target; a script reads it first, the same
            // check a separate read call would pass.
            if tool == "browser_input" {
                let mut read = json!({"action":"read"});
                if let Some(selector) = step.get("selector") {
                    read["selector"] = selector.clone();
                }
                let observed = self.call_one("browser_read", &read, base, trusted).await;
                if observed["status"] != "success" {
                    self.script_screenshot(index, None, base, trusted, &mut artifacts)
                        .await;
                    return json!({"status":observed["status"].clone(),
                        "error_kind":observed["error_kind"].clone(),
                        "output":format!("Step {index} ({action}) failed reading its target: {}",
                            observed["output"].as_str().unwrap_or("")),
                        "failed_step":index,"steps":results,"artifacts":artifacts});
                }
                // A read that matched nothing names the target, as an explicit read would;
                // input is still refused.
                if truthy(observed.get("missing")) {
                    self.script_screenshot(index, None, base, trusted, &mut artifacts)
                        .await;
                    let selector = step
                        .get("selector")
                        .and_then(Value::as_str)
                        .unwrap_or("body");
                    return json!({"status":"error","error_kind":"browser_invalid_request",
                        "output":format!("Step {index} ({action}) failed: Target `{selector}` not found on the page; check the selector, or wait_for it to appear"),
                        "failed_step":index,"steps":results,"artifacts":artifacts});
                }
            }
            let output = self
                .call_one(tool, &Value::Object(step), base, trusted)
                .await;
            let mut summary = Self::step_summary(index, &action, &output);
            if output["status"] != "success" {
                self.script_screenshot(index, None, base, trusted, &mut artifacts)
                    .await;
                return json!({"status":output["status"].clone(),
                    "error_kind":output["error_kind"].clone(),
                    "output":format!("Step {index} ({action}) failed: {}",
                        output["output"].as_str().unwrap_or("")),
                    "failed_step":index,"steps":results,"artifacts":artifacts});
            }
            if action == "screenshot" {
                summary = json!({"step":index,"action":action,"screenshot":artifacts.len()});
                artifacts.push(Self::step_summary(index, &action, &output));
            } else if shot {
                if let Some(at) = self
                    .script_screenshot(index, label.as_ref(), base, trusted, &mut artifacts)
                    .await
                {
                    summary["screenshot"] = json!(at);
                } else {
                    summary["screenshot_error"] = json!("screenshot failed");
                }
            }
            results.push(summary);
        }
        json!({"status":"success","steps":results,"artifacts":artifacts})
    }

    /// Takes one script screenshot; its index in `artifacts`, or `None` when it failed.
    async fn script_screenshot(
        self: &Arc<Self>,
        index: usize,
        label: Option<&Value>,
        base: &Path,
        trusted: &Value,
        artifacts: &mut Vec<Value>,
    ) -> Option<usize> {
        let mut args = json!({"action":"screenshot"});
        if let Some(label) = label {
            args["label"] = label.clone();
        }
        let output = self.call_one("browser_read", &args, base, trusted).await;
        (output["status"] == "success").then(|| {
            artifacts.push(Self::step_summary(index, "screenshot", &output));
            artifacts.len() - 1
        })
    }

    /// A step's own result without the fields every browser result repeats.
    fn step_summary(index: usize, action: &str, output: &Value) -> Value {
        let mut summary = json!({"step":index,"action":action});
        if let Value::Object(fields) = output {
            for (key, value) in fields {
                if !matches!(
                    key.as_str(),
                    "status" | "backend" | "signed_in" | "session_id" | "source_kind" | "user_id"
                ) {
                    summary[key] = value.clone();
                }
            }
        }
        summary
    }

    /// Runs one browser tool call under its time bound.
    async fn call_one(
        self: &Arc<Self>,
        name: &str,
        args: &Value,
        base: &Path,
        trusted: &Value,
    ) -> Value {
        let sid = trusted
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let limit = if trusted.get("site").is_some_and(Value::is_object) {
            SITE_CALL_TIMEOUT
        } else {
            CALL_TIMEOUT
        };
        let outcome = tokio::time::timeout(limit, self.run(name, args, base, trusted)).await;
        let drop_session = || async {
            if let Some(sid) = &sid {
                self.close(sid).await;
            }
        };
        match outcome {
            Err(_) => {
                drop_session().await;
                json!({"status":"uncertain","error_kind":"browser_timeout",
                    "output":"Browser operation timed out; inspect before retrying"})
            }
            Ok(Ok(result)) => result,
            Ok(Err(Fault::Uncertain(_))) => {
                drop_session().await;
                json!({"status":"uncertain","error_kind":"browser_uncertain",
                    "output":"Browser changed origin or input outcome is uncertain; no automatic replay. \
                        The browser was closed: start the next call or script with navigate (or read after \
                        navigate); the page from an earlier call is not trusted for input."})
            }
            Ok(Err(fault @ (Fault::Startup | Fault::Authentication | Fault::Capacity))) => {
                let kind = match fault {
                    Fault::Startup => "browser_startup_failed",
                    Fault::Authentication => "browser_authentication_failed",
                    _ => "browser_capacity",
                };
                json!({"status":"error","error_kind":kind,
                    "output":"Private browser operation failed; credentials were not returned"})
            }
            Ok(Err(Fault::NotReady {
                kind,
                message,
                errors,
            })) => {
                json!({"status":"error","error_kind":kind,"output":message,"page_errors":errors})
            }
            // Rejection messages are fixed strings in this module, never page or credential
            // text.
            Ok(Err(Fault::Rejected(message))) => json!({"status":"error",
                "error_kind":"browser_invalid_request",
                "output":format!("Private browser request rejected: {message}")}),
            // WebDriver messages can echo page text, so only the failure's type name and a
            // fixed crash test are used.
            Ok(Err(Fault::Backend { name, crashed })) => {
                if crashed
                    || matches!(
                        name.as_str(),
                        "InvalidSessionIdException" | "NoSuchWindowException"
                    )
                {
                    drop_session().await;
                    json!({"status":"error","error_kind":"browser_crashed",
                        "output":"The browser tab crashed (often out of memory) and was closed; sign in or navigate again to open a fresh one"})
                } else {
                    json!({"status":"error","error_kind":"browser_backend_failed",
                        "output":format!("Private browser backend failed ({name}); credentials were not returned")})
                }
            }
        }
    }

    /// Opens (or reuses) this session's browser without acting on a page.
    async fn ensure_open(&self, trusted: &Trusted) -> Result<(), Fault> {
        let mut states = self.states.lock().await;
        Self::purge(&mut states).await;
        self.open(&mut states, trusted, true).await
    }

    /// Captures bounded private PNG frames until stop, quota, lease or backend failure.
    async fn record_loop(
        self: Arc<Self>,
        base: PathBuf,
        trusted: Value,
        expires: f64,
        record: Arc<Recording>,
    ) {
        for _ in 0..MAX_FRAMES {
            if record.is_stopped() || now() >= expires {
                break;
            }
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(1)) => {}
                () = record.stop.notified() => {}
            }
            if record.is_stopped() {
                break;
            }
            let result = self
                .call(
                    "browser_read",
                    &json!({"action":"screenshot"}),
                    &base,
                    &trusted,
                )
                .await;
            if result["status"] != "success" {
                break;
            }
            let frame: Map<String, Value> = [
                "artifact_id",
                "path",
                "media_type",
                "bytes",
                "width",
                "height",
                "sha256",
            ]
            .iter()
            .map(|key| ((*key).to_owned(), result[*key].clone()))
            .collect();
            record.progress().artifacts.push(Value::Object(frame));
        }
        record.finish();
    }

    /// Starts, stops or reports a caller-bound recording; state changes are serialized per
    /// session, independently of per-frame browser access.
    pub async fn record(self: &Arc<Self>, args: &Value, base: &Path, trusted: &Value) -> Value {
        let rejected = || json!({"status":"error","error_kind":"browser_record_rejected","output":"Recorder request rejected"});
        let Some(sid) = trusted
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|sid| canonical_id(sid))
        else {
            return rejected();
        };
        let lock = Arc::clone(
            self.record_guards
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(sid.to_owned())
                .or_default(),
        );
        let _held = lock.lock().await;
        self.record_locked(args, base, trusted)
            .await
            .unwrap_or_else(|_| rejected())
    }

    async fn record_locked(
        self: &Arc<Self>,
        args: &Value,
        base: &Path,
        raw: &Value,
    ) -> Result<Value, Fault> {
        let trusted = validate_trusted(base, raw)?;
        let Value::Object(args) = args else {
            return Err(Fault::rejected("Browser arguments must be an object"));
        };
        let mut args = args.clone();
        let action = validate_args("browser_record", &mut args)?;
        let sid = trusted.session_id.clone();
        let mut record = self.recording(&sid);
        if let Some(current) = &record {
            if current.user != trusted.user_id {
                return Err(Fault::rejected("Recorder caller mismatch"));
            }
            if current.cookie != trusted.cookie || current.generation != trusted.generation {
                current.cancel().await;
                self.close(&sid).await;
            }
        }
        if action == "start" {
            if record
                .as_ref()
                .is_some_and(|record| record.progress().recording)
            {
                return Err(Fault::rejected("Recorder already running"));
            }
            self.ensure_open(&trusted).await?;
            let started = Arc::new(Recording {
                user: trusted.user_id.clone(),
                generation: trusted.generation.clone(),
                cookie: trusted.cookie.clone(),
                started_at: now() as u64,
                progress: StdMutex::new(Progress {
                    recording: true,
                    artifacts: Vec::new(),
                    completed_at: None,
                }),
                stop: Notify::new(),
                stopped: std::sync::atomic::AtomicBool::new(false),
                task: StdMutex::new(None),
            });
            self.recordings
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(sid.clone(), Arc::clone(&started));
            let task = tokio::spawn(Arc::clone(self).record_loop(
                base.to_owned(),
                raw.clone(),
                trusted.expires_at,
                Arc::clone(&started),
            ));
            *started.task.lock().unwrap_or_else(PoisonError::into_inner) = Some(task);
            record = Some(started);
        } else if action == "stop" {
            if let Some(current) = &record {
                current.signal();
                let task = current
                    .task
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                if let Some(task) = task {
                    let _ = task.await;
                }
            }
        }
        let Some(record) = record else {
            return Ok(json!({"status":"success","recording":false,"frames":0,"artifacts":[]}));
        };
        let progress = record.progress();
        Ok(json!({"status":"success","recording":progress.recording,
            "frames":progress.artifacts.len(),"artifacts":progress.artifacts,
            "started_at":record.started_at,"completed_at":progress.completed_at,
            "format":"bounded_png_frames","max_frames":MAX_FRAMES,"max_seconds":30,"video":false}))
    }

    /// Stops recording and closes one session's profile during a trusted worker run stop.
    pub async fn close_all(&self, base: &Path) {
        let Some(sid) = base
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|sid| canonical_id(sid))
        else {
            return;
        };
        if let Some(record) = self.recording(sid) {
            record.cancel().await;
        }
        self.close(sid).await;
    }

    /// The configured limits.
    #[must_use]
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// The private browser root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}
