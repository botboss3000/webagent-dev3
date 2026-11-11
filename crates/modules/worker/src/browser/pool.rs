//! The worker's side of Browser Control, ported from `worker.py` `BrowserPackages`: it
//! owns capacity (the live per-app and per-user limits, with a queue beyond them) and each
//! session process's lifecycle. A process is pinned to its caller's authority; a different
//! authority, an exit, an uncertain or closing result, a broken transport, lease expiry or
//! the idle time end it. A broken transport never authorizes replay. Screenshot reads are
//! served here from the private folder without a browser. Session folders are deleted with
//! their chat session, and their bytes are published for the owner's storage quota.
use super::rules::truthy;
use super::service::{read_artifact, Limits};
use super::{now, Settings, IMPLEMENTATION_VERSION};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use webagent_browser::limits::Limits as Running;

/// A queued session that has not retried for this long has given up its place.
const WAIT_TTL: Duration = Duration::from_secs(90);
/// The folder holding the running browser's profile; deleted when it stops, so not counted.
const PROFILE: &str = "chrome";
const LIMIT: usize = 2 * 1024 * 1024;
const CALL_TIMEOUT: Duration = Duration::from_secs(25);
/// Signing in again to a saved site replays its steps, which takes longer than one action.
const SITE_CALL_TIMEOUT: Duration = Duration::from_secs(65);

/// The authority a session process is pinned to, as the worker received it.
type Authority = [Value; 5];

fn authority(trusted: &Value) -> Authority {
    ["session_id", "user_id", "agent_id", "generation", "cookie"]
        .map(|key| trusted.get(key).cloned().unwrap_or(Value::Null))
}

/// One running session process.
struct Item {
    pid: i32,
    process: Mutex<Child>,
    /// The per-session lock: one exchange at a time.
    io: Mutex<(ChildStdin, BufReader<ChildStdout>)>,
    authority: Authority,
    user: String,
    lease: StdMutex<(f64, Instant)>,
}

impl Item {
    async fn exited(&self) -> bool {
        !matches!(self.process.lock().await.try_wait(), Ok(None))
    }

    fn stale(&self, idle: Duration) -> bool {
        let lease = self.lease.lock().unwrap_or_else(PoisonError::into_inner);
        now() >= lease.0 || lease.1.elapsed() > idle
    }

    /// SIGTERM to the process group, SIGKILL after five seconds, then SIGKILL to whatever
    /// of the group is left.
    async fn terminate(&self) {
        let mut process = self.process.lock().await;
        if matches!(process.try_wait(), Ok(None)) {
            // SAFETY: killpg takes plain integers; the leader is unreaped, so the group id
            // is still this session's.
            unsafe {
                libc::killpg(self.pid, libc::SIGTERM);
            }
            if tokio::time::timeout(Duration::from_secs(5), process.wait())
                .await
                .is_err()
            {
                // SAFETY: as above.
                unsafe {
                    libc::killpg(self.pid, libc::SIGKILL);
                }
                let _ = process.wait().await;
            }
        }
        // SAFETY: as above; ESRCH once everything has gone is expected.
        unsafe {
            libc::killpg(self.pid, libc::SIGKILL);
        }
    }
}

/// A session waiting for a browser, in arrival order.
struct Waiter {
    sid: String,
    user: String,
    seen: Instant,
}

#[derive(Default)]
struct Inner {
    sessions: HashMap<String, Arc<Item>>,
    queue: Vec<Waiter>,
    reaping: bool,
}

impl Inner {
    fn running(&self, user: &str) -> usize {
        self.sessions
            .values()
            .filter(|item| item.user == user)
            .count()
    }

    fn forget_stale_waiters(&mut self) {
        self.queue
            .retain(|waiter| waiter.seen.elapsed() <= WAIT_TTL);
    }

    /// Admits `sid` when both limits allow it and no session queued before it could start
    /// now; otherwise queues it (once) and returns its 1-based position.
    fn admit(&mut self, sid: &str, user: &str, limits: &Running) -> Result<(), usize> {
        self.forget_stale_waiters();
        let fits = |inner: &Self, user: &str| {
            inner.sessions.len() < limits.per_app && inner.running(user) < limits.per_user
        };
        let place = self.queue.iter().position(|waiter| waiter.sid == sid);
        let ahead = place.unwrap_or(self.queue.len());
        let blocked = self.queue[..ahead]
            .iter()
            .any(|waiter| fits(self, &waiter.user));
        if fits(self, user) && !blocked {
            if let Some(place) = place {
                self.queue.remove(place);
            }
            return Ok(());
        }
        match place {
            Some(place) => {
                self.queue[place].seen = Instant::now();
                Err(place + 1)
            }
            None => {
                self.queue.push(Waiter {
                    sid: sid.to_owned(),
                    user: user.to_owned(),
                    seen: Instant::now(),
                });
                Err(self.queue.len())
            }
        }
    }
}

/// Every session process of this worker.
pub struct Browsers {
    settings: Settings,
    limits: Limits,
    inner: Mutex<Inner>,
    /// Bytes of each session folder, without a running profile.
    usage: StdMutex<BTreeMap<String, u64>>,
}

fn invalid(output: &str) -> Value {
    json!({"status":"error","error_kind":"browser_invalid_request","output":output})
}

impl Browsers {
    /// An empty pool whose session processes use `settings`.
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            limits: Limits::default(),
            inner: Mutex::new(Inner::default()),
            usage: StdMutex::new(BTreeMap::new()),
        }
    }

    /// Open private browsers; trusted worker status, never a model tool.
    pub async fn count(&self) -> usize {
        self.inner.lock().await.sessions.len()
    }

    /// Removes and ends the session's process when it is still `item` (any, when `None`).
    async fn drop_locked(inner: &mut Inner, sid: &str, item: Option<&Arc<Item>>) {
        let Some(current) = inner.sessions.get(sid) else {
            return;
        };
        if item.is_some_and(|item| !Arc::ptr_eq(item, current)) {
            return;
        }
        if let Some(current) = inner.sessions.remove(sid) {
            current.terminate().await;
        }
    }

    /// Ends the session's browser process and its place in the queue: a run stop,
    /// cancellation and revocation.
    pub async fn close(&self, sid: &str) {
        let mut inner = self.inner.lock().await;
        inner.queue.retain(|waiter| waiter.sid != sid);
        Self::drop_locked(&mut inner, sid, None).await;
    }

    /// Ends the session's browser and deletes its private folder: the chat session was
    /// deleted.
    pub async fn purge(&self, sid: &str) {
        self.close(sid).await;
        if let Some(folder) = self.folder(sid) {
            let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(folder)).await;
        }
        self.usage
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(sid);
        self.publish_usage().await;
    }

    /// Deletes every folder whose session is not in `keep` (the chat sessions that still
    /// exist), and the Chromium profile a crashed browser left in a kept folder (a restart
    /// never restores it), then recounts the rest. Called once the API has listed every
    /// user's sessions.
    pub async fn sweep(&self, keep: &[String]) -> usize {
        let root = self.settings.root.clone();
        let keep = keep.to_vec();
        let running: Vec<String> = self.inner.lock().await.sessions.keys().cloned().collect();
        let swept = tokio::task::spawn_blocking(move || {
            let Ok(entries) = std::fs::read_dir(&root) else {
                return (0, BTreeMap::new());
            };
            let mut removed = 0;
            let mut usage = BTreeMap::new();
            for entry in entries.flatten() {
                let Some(sid) = entry.file_name().to_str().and_then(session_name) else {
                    continue;
                };
                let path = entry.path();
                if running.contains(&sid) {
                    usage.insert(sid, folder_bytes(&path));
                } else if keep.contains(&sid) {
                    let profile = path.join("chrome");
                    if !is_link(&path) && profile.is_dir() && !is_link(&profile) {
                        let _ = std::fs::remove_dir_all(&profile);
                    }
                    usage.insert(sid, folder_bytes(&path));
                } else if !is_link(&path) && std::fs::remove_dir_all(&path).is_ok() {
                    removed += 1;
                }
            }
            (removed, usage)
        })
        .await
        .unwrap_or_default();
        *self.usage.lock().unwrap_or_else(PoisonError::into_inner) = swept.1;
        self.publish_usage().await;
        swept.0
    }

    /// The session's folder when `sid` is a canonical UUID and the folder is a real one.
    fn folder(&self, sid: &str) -> Option<PathBuf> {
        let sid = session_name(sid)?;
        let folder = self.settings.root.join(sid);
        (folder.is_dir() && !is_link(&folder)).then_some(folder)
    }

    /// Recounts one session's folder after a call that may have stored a screenshot.
    async fn recount(&self, sid: &str) {
        let Some(folder) = self.folder(sid) else {
            return;
        };
        let bytes = tokio::task::spawn_blocking(move || folder_bytes(&folder))
            .await
            .unwrap_or(0);
        let changed = self
            .usage
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(sid.to_owned(), bytes)
            != Some(bytes);
        if changed {
            self.publish_usage().await;
        }
    }

    /// Writes `browser-usage.json` beside the browser root (group-readable by the API),
    /// replacing it atomically: `{"<session id>": bytes}`.
    async fn publish_usage(&self) {
        let body = {
            let usage = self.usage.lock().unwrap_or_else(PoisonError::into_inner);
            serde_json::to_vec(&*usage).unwrap_or_default()
        };
        let path = usage_path(&self.settings.root);
        let _ = tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::OpenOptionsExt;
            let temporary = path.with_extension("json.tmp");
            let _ = std::fs::remove_file(&temporary);
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o640)
                .open(&temporary)?;
            std::io::Write::write_all(&mut file, &body)?;
            std::fs::rename(&temporary, &path)
        })
        .await;
    }

    async fn drop_item(&self, sid: &str, item: &Arc<Item>) {
        let mut inner = self.inner.lock().await;
        Self::drop_locked(&mut inner, sid, Some(item)).await;
    }

    /// Enforces leases even when no later tool call arrives.
    async fn reap(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let limits = Running::live();
            let mut inner = self.inner.lock().await;
            if inner.sessions.is_empty() {
                inner.reaping = false;
                return;
            }
            inner.forget_stale_waiters();
            // A waiting session gets an idle browser's place sooner.
            let idle = Duration::from_secs(if inner.queue.is_empty() {
                limits.idle_seconds
            } else {
                limits.queued_idle_seconds
            });
            let items: Vec<(String, Arc<Item>)> = inner
                .sessions
                .iter()
                .map(|(sid, item)| (sid.clone(), Arc::clone(item)))
                .collect();
            for (sid, item) in items {
                if item.exited().await || item.stale(idle) {
                    Self::drop_locked(&mut inner, &sid, Some(&item)).await;
                }
            }
        }
    }

    /// Starts a session process without passing credentials through argv or environment.
    fn start(&self, supervisor: &str, trusted: &Value) -> std::io::Result<Arc<Item>> {
        let mut command = tokio::process::Command::new(supervisor);
        command
            .arg("--browser-session")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("V3_BROWSER_ROOT", &self.settings.root)
            .env("V3_PUBLIC_ORIGIN", &self.settings.origin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(false);
        // SAFETY: setsid is async-signal-safe; the process leads its own group, which
        // Chromium's processes join, so the whole browser ends with one killpg.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        let pid = child
            .id()
            .and_then(|pid| i32::try_from(pid).ok())
            .ok_or_else(|| std::io::Error::other("browser process id"))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(std::io::Error::other("browser process pipes"));
        };
        Ok(Arc::new(Item {
            pid,
            process: Mutex::new(child),
            io: Mutex::new((stdin, BufReader::new(stdout))),
            authority: authority(trusted),
            user: trusted["user_id"].as_str().unwrap_or_default().to_owned(),
            lease: StdMutex::new((
                trusted["expires_at"].as_f64().unwrap_or(0.0),
                Instant::now(),
            )),
        }))
    }

    /// Runs one browser call for the admitted workspace `base`: artifact reads here, every
    /// other call in the session's pinned process, started from `supervisor`
    /// (`webagent-v3`) on first use.
    pub async fn call(
        self: &Arc<Self>,
        supervisor: &str,
        name: &str,
        args: &Value,
        base: &Path,
        trusted: &Value,
    ) -> Value {
        let sid = base
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
        if !trusted.is_object()
            || trusted["session_id"] != sid.as_str()
            || trusted["allowed_session_ids"] != json!([sid])
        {
            return invalid("Trusted browser session required");
        }
        let expires = trusted["expires_at"]
            .as_f64()
            .filter(|_| trusted["expires_at"].is_number());
        if !expires.is_some_and(|expires| now() < expires && expires <= now() + 601.0) {
            self.close(&sid).await;
            return invalid("Browser authorization expired");
        }
        let artifact = name == "administrator_artifact_get";
        let item = {
            let mut inner = self.inner.lock().await;
            let identity = authority(trusted);
            if let Some(old) = inner.sessions.get(&sid).cloned() {
                if old.authority != identity || old.exited().await {
                    Self::drop_locked(&mut inner, &sid, Some(&old)).await;
                }
            }
            if artifact {
                None
            } else if let Some(item) = inner.sessions.get(&sid) {
                Some(Arc::clone(item))
            } else {
                if name == "browser_read" && args["action"] == "close" {
                    return json!({"status":"success","closed":true});
                }
                let limits = Running::live();
                let user = trusted["user_id"].as_str().unwrap_or_default();
                if let Err(position) = inner.admit(&sid, user, &limits) {
                    return queued(position, limits.queued_idle_seconds);
                }
                match self.start(supervisor, trusted) {
                    Ok(item) => {
                        inner.sessions.insert(sid.clone(), Arc::clone(&item));
                        if !inner.reaping {
                            inner.reaping = true;
                            tokio::spawn(Arc::clone(self).reap());
                        }
                        Some(item)
                    }
                    Err(_) => return transport_failed(name, args),
                }
            }
        };
        let Some(item) = item else {
            return self.artifact(args, base, trusted).await;
        };
        let mut io = item.io.lock().await;
        // The API renews a signed-in session's lease on each call; the process lives as
        // long as it.
        *item.lease.lock().unwrap_or_else(PoisonError::into_inner) =
            (expires.unwrap_or(0.0), Instant::now());
        let limit = if trusted["site"].is_object() {
            SITE_CALL_TIMEOUT
        } else {
            CALL_TIMEOUT
        };
        let exchanged =
            tokio::time::timeout(limit, exchange(&mut io, name, args, base, trusted)).await;
        drop(io);
        self.recount(&sid).await;
        match exchanged {
            Ok(Some(result)) => {
                if result["status"] == "uncertain"
                    || truthy(result.get("closed"))
                    || result["error_kind"] == "browser_authentication_failed"
                {
                    self.drop_item(&sid, &item).await;
                }
                result
            }
            _ => {
                self.drop_item(&sid, &item).await;
                transport_failed(name, args)
            }
        }
    }

    /// Serves a private screenshot after checking its owner and bytes; any failure reads as
    /// a broken (non-mutating) transport, as the Python package process reported it.
    async fn artifact(&self, args: &Value, base: &Path, trusted: &Value) -> Value {
        let root = self.settings.root.clone();
        let limits = self.limits.clone();
        let base = base.to_owned();
        let trusted = trusted.clone();
        let artifact = args.get("artifact_id").cloned();
        let read = tokio::task::spawn_blocking(move || {
            let artifact = artifact.ok_or(super::Fault::Startup)?;
            read_artifact(&root, &limits, &base, &trusted, &artifact)
        })
        .await;
        match read {
            Ok(Ok(content)) => {
                let mut result = json!({"status":"success",
                    "png_base64":super::base64::encode(&content)});
                stamp(&mut result);
                result
            }
            _ => transport_failed("administrator_artifact_get", args),
        }
    }
}

/// The reply to a session waiting for a browser: nothing ran, so retrying is safe.
fn queued(position: usize, wait: u64) -> Value {
    json!({"status":"error","error_kind":"browser_queued","queue_position":position,
        "retry_after_seconds":wait,
        "output":format!("All private browsers are busy. This chat is number {position} in the \
            queue; nothing ran. Retry the same call in about {wait} seconds to keep the place.")})
}

/// `name` when it is a canonical (lower-case, hyphenated) UUID: a session folder.
fn session_name(name: &str) -> Option<String> {
    let parsed = uuid::Uuid::parse_str(name).ok()?;
    (parsed.hyphenated().to_string() == name).then(|| name.to_owned())
}

fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// Bytes stored under a session folder, skipping links and the running profile.
fn folder_bytes(folder: &Path) -> u64 {
    fn walk(path: &Path, top: bool) -> u64 {
        let Ok(entries) = std::fs::read_dir(path) else {
            return 0;
        };
        let mut total = 0u64;
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                if !(top && entry.file_name() == PROFILE) {
                    total = total.saturating_add(walk(&entry.path(), false));
                }
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
        total
    }
    walk(folder, true)
}

/// Where the API reads browser folder bytes: `browser-usage.json` beside the browser root.
fn usage_path(root: &Path) -> PathBuf {
    root.with_file_name("browser-usage.json")
}

/// The package fields every browser result carries.
fn stamp(result: &mut Value) {
    result["plugin_revision"] = Value::Null;
    result["implementation_sha256"] = Value::Null;
    result["implementation_version"] = json!(IMPLEMENTATION_VERSION);
}

/// A broken transport: uncertain for input and navigation, which may have taken effect.
fn transport_failed(name: &str, args: &Value) -> Value {
    let mutating =
        name == "browser_input" || (name == "browser_read" && args["action"] == "navigate");
    json!({"status":if mutating {"uncertain"} else {"error"},"uncertain":mutating,
        "error_kind":"browser_transport_failed",
        "output":"Browser process interrupted (the app may have restarted); navigate again, check the page, then retry"})
}

/// Sends one frame and reads its reply; `None` when the transport or reply is broken.
async fn exchange(
    io: &mut (ChildStdin, BufReader<ChildStdout>),
    name: &str,
    args: &Value,
    base: &Path,
    trusted: &Value,
) -> Option<Value> {
    let invocation = uuid::Uuid::new_v4().to_string();
    let payload = json!({"protocol":1,"invocation_id":invocation,"tool":name,"arguments":args,
        "base":base.to_string_lossy(),"trusted":trusted});
    let mut frame = payload.to_string().into_bytes();
    frame.push(b'\n');
    if frame.len() > LIMIT {
        return None;
    }
    let (stdin, stdout) = io;
    stdin.write_all(&frame).await.ok()?;
    stdin.flush().await.ok()?;
    let mut raw = Vec::new();
    stdout
        .take(LIMIT as u64 + 1)
        .read_until(b'\n', &mut raw)
        .await
        .ok()?;
    if !raw.ends_with(b"\n") || raw.len() > LIMIT {
        return None;
    }
    let reply: Value = serde_json::from_slice(&raw).ok()?;
    if reply["protocol"].as_u64() != Some(1)
        || reply["invocation_id"] != invocation.as_str()
        || !reply["result"].is_object()
    {
        return None;
    }
    let mut result = reply["result"].clone();
    if !["success", "error", "failed", "uncertain"]
        .iter()
        .any(|status| result["status"] == *status)
    {
        return None;
    }
    stamp(&mut result);
    result["implementation_version"] = reply
        .get("implementation_version")
        .cloned()
        .unwrap_or(Value::Null);
    Some(result)
}

#[cfg(test)]
mod queue_tests {
    use super::*;

    fn limits(per_app: usize, per_user: usize) -> Running {
        Running {
            per_app,
            per_user,
            ..Running::default()
        }
    }

    /// Marks `sid` running for `user` without a process; admission only counts entries.
    fn run(inner: &mut Inner, sid: &str, user: &str) {
        let child = tokio::process::Command::new("true")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut child = child;
        let (stdin, stdout) = (child.stdin.take().unwrap(), child.stdout.take().unwrap());
        inner.sessions.insert(
            sid.into(),
            Arc::new(Item {
                pid: 0,
                process: Mutex::new(child),
                io: Mutex::new((stdin, BufReader::new(stdout))),
                authority: Default::default(),
                user: user.into(),
                lease: StdMutex::new((0.0, Instant::now())),
            }),
        );
    }

    #[tokio::test]
    async fn per_user_limit_queues_and_other_users_still_start() {
        let mut inner = Inner::default();
        let limits = limits(5, 2);
        for sid in ["a1", "a2"] {
            assert_eq!(inner.admit(sid, "alice", &limits), Ok(()));
            run(&mut inner, sid, "alice");
        }
        assert_eq!(inner.admit("a3", "alice", &limits), Err(1));
        // A waiter that cannot start does not block another user.
        assert_eq!(inner.admit("b1", "bob", &limits), Ok(()));
        run(&mut inner, "b1", "bob");
        // Retrying keeps the place.
        assert_eq!(inner.admit("a3", "alice", &limits), Err(1));
        inner.sessions.remove("a1");
        assert_eq!(inner.admit("a3", "alice", &limits), Ok(()));
        assert!(inner.queue.is_empty());
    }

    #[tokio::test]
    async fn app_limit_serves_the_queue_in_order() {
        let mut inner = Inner::default();
        let limits = limits(2, 4);
        for sid in ["s1", "s2"] {
            assert_eq!(inner.admit(sid, "alice", &limits), Ok(()));
            run(&mut inner, sid, "alice");
        }
        assert_eq!(inner.admit("s3", "alice", &limits), Err(1));
        assert_eq!(inner.admit("s4", "bob", &limits), Err(2));
        inner.sessions.remove("s1");
        // The first in line gets the free browser, not a later caller.
        assert_eq!(inner.admit("s4", "bob", &limits), Err(2));
        assert_eq!(inner.admit("s3", "alice", &limits), Ok(()));
        run(&mut inner, "s3", "alice");
        assert_eq!(inner.admit("s4", "bob", &limits), Err(1));
    }

    #[test]
    fn silent_waiters_give_up_their_place() {
        let mut inner = Inner::default();
        inner.queue.push(Waiter {
            sid: "gone".into(),
            user: "alice".into(),
            seen: Instant::now() - WAIT_TTL - Duration::from_secs(1),
        });
        inner.forget_stale_waiters();
        assert!(inner.queue.is_empty());
    }

    #[tokio::test]
    async fn sweep_deletes_gone_sessions_and_crash_left_profiles() {
        let top = tempfile::tempdir().unwrap();
        let root = top.path().join("browser");
        let (kept, gone) = (
            "11111111-1111-4111-8111-111111111111",
            "22222222-2222-4222-8222-222222222222",
        );
        for sid in [kept, gone] {
            std::fs::create_dir_all(root.join(sid).join("chrome/Default")).unwrap();
            std::fs::write(root.join(sid).join("chrome/Default/Cookies"), b"c").unwrap();
            std::fs::write(root.join(sid).join("owner.json"), b"{}").unwrap();
        }
        let browsers = Browsers::new(Settings {
            root: root.clone(),
            origin: "https://example.test".into(),
        });
        assert_eq!(browsers.sweep(&[kept.to_owned()]).await, 1);
        assert!(!root.join(gone).exists());
        // The kept session keeps its folder but not the signed-in profile.
        assert!(root.join(kept).join("owner.json").exists());
        assert!(!root.join(kept).join("chrome").exists());
    }

    #[test]
    fn folder_bytes_skip_the_running_profile() {
        let folder = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder.path().join("chrome/x")).unwrap();
        std::fs::write(folder.path().join("chrome/x/big"), vec![0; 1000]).unwrap();
        std::fs::create_dir_all(folder.path().join("artifacts")).unwrap();
        std::fs::write(folder.path().join("artifacts/a.png"), vec![0; 10]).unwrap();
        std::fs::write(folder.path().join("owner.json"), b"{}").unwrap();
        assert_eq!(folder_bytes(folder.path()), 12);
        assert_eq!(session_name("not-a-uuid"), None);
    }
}
