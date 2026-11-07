//! The worker's side of Browser Control, ported from `worker.py` `BrowserPackages`: it
//! owns global capacity (two private browsers) and each session process's lifecycle. A
//! process is pinned to its caller's authority; a different authority, an exit, an
//! uncertain or closing result, a broken transport, lease expiry or three idle minutes end
//! it. A broken transport never authorizes replay. Screenshot reads are served here from
//! the private folder without a browser.
use super::rules::truthy;
use super::service::{read_artifact, Limits};
use super::{now, Settings, IMPLEMENTATION_VERSION};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

const CAPACITY: usize = 2;
const IDLE: Duration = Duration::from_secs(180);
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
    lease: StdMutex<(f64, Instant)>,
}

impl Item {
    async fn exited(&self) -> bool {
        !matches!(self.process.lock().await.try_wait(), Ok(None))
    }

    fn stale(&self) -> bool {
        let lease = self.lease.lock().unwrap_or_else(PoisonError::into_inner);
        now() >= lease.0 || lease.1.elapsed() > IDLE
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

#[derive(Default)]
struct Inner {
    sessions: HashMap<String, Arc<Item>>,
    reaping: bool,
}

/// Every session process of this worker.
pub struct Browsers {
    settings: Settings,
    limits: Limits,
    inner: Mutex<Inner>,
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

    /// Ends the session's browser process: quiescence, cancellation and revocation.
    pub async fn close(&self, sid: &str) {
        let mut inner = self.inner.lock().await;
        Self::drop_locked(&mut inner, sid, None).await;
    }

    async fn drop_item(&self, sid: &str, item: &Arc<Item>) {
        let mut inner = self.inner.lock().await;
        Self::drop_locked(&mut inner, sid, Some(item)).await;
    }

    /// Enforces leases even when no later tool call arrives.
    async fn reap(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let mut inner = self.inner.lock().await;
            if inner.sessions.is_empty() {
                inner.reaping = false;
                return;
            }
            let items: Vec<(String, Arc<Item>)> = inner
                .sessions
                .iter()
                .map(|(sid, item)| (sid.clone(), Arc::clone(item)))
                .collect();
            for (sid, item) in items {
                if item.exited().await || item.stale() {
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
                if inner.sessions.len() >= CAPACITY {
                    return json!({"status":"error","error_kind":"browser_capacity",
                        "output":"Two private browsers are already active; close one before opening another"});
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
        "output":"Browser process interrupted; inspect before retrying an input action"})
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
