//! Interactive terminals in a session workspace (`terminal_*`), ported from the
//! Administrator `terminal_control` adapter. Each terminal runs `bash -c COMMAND` on a
//! pseudo-terminal under its own supervisor (`webagent-v3 --terminal-supervisor`), a
//! subreaper that ends every descendant when the terminal closes. Terminals live as long
//! as the worker; a restart closes their control pipes and never replays input.
use serde_json::{json, Map, Value};
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::ChildStdin;
use tokio::sync::{Mutex, Notify};

/// The tools this module runs.
pub const TOOLS: [&str; 6] = [
    "terminal_open",
    "terminal_read",
    "terminal_send",
    "terminal_wait",
    "terminal_list",
    "terminal_close",
];
/// Output kept per terminal; older output is dropped and reads report truncation.
pub const LIMIT: usize = 262_144;
const IDLE: Duration = Duration::from_secs(900);
const MAX_TERMINALS: usize = 4;

/// Whether this module runs `name`.
#[must_use]
pub fn handles(name: &str) -> bool {
    TOOLS.contains(&name)
}

fn key(name: &str) -> Option<&'static [u8]> {
    Some(match name {
        "enter" => b"\r",
        "esc" => b"\x1b",
        "tab" => b"\t",
        "backspace" => b"\x7f",
        "up" => b"\x1b[A",
        "down" => b"\x1b[B",
        "right" => b"\x1b[C",
        "left" => b"\x1b[D",
        "ctrl+c" => b"\x03",
        "ctrl+d" => b"\x04",
        "ctrl+z" => b"\x1a",
        _ => return None,
    })
}

fn integer(value: Option<&Value>, name: &str, minimum: i64, maximum: i64) -> Result<i64, String> {
    value
        .and_then(Value::as_i64)
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or_else(|| format!("{name} out of range"))
}

/// What a terminal has printed and whether its input may be sent.
struct State {
    output: Vec<u8>,
    start: i64,
    end: i64,
    sequence: i64,
    /// The sequence the last read showed; a send must match it, and uses it up.
    observed: Option<i64>,
    /// `Some` once the supervisor has exited: its exit code, or minus the signal.
    exit: Option<i32>,
    stdin: Option<ChildStdin>,
}

/// One open terminal.
pub struct Terminal {
    id: String,
    base: PathBuf,
    command: String,
    ports: Vec<u16>,
    /// The supervisor's process id, the root of the terminal's process tree.
    pid: i32,
    state: Mutex<State>,
    changed: Notify,
    updated: StdMutex<Instant>,
}

impl Terminal {
    fn touch(&self) {
        *self.updated.lock().unwrap_or_else(PoisonError::into_inner) = Instant::now();
    }

    fn idle(&self) -> bool {
        self.updated
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .elapsed()
            > IDLE
    }

    /// Whether the supervisor still runs; the tunnel uses this for port ownership.
    pub async fn running(&self) -> bool {
        self.state.lock().await.exit.is_none()
    }

    async fn snapshot(&self, cursor: Option<&Value>) -> Result<Value, String> {
        let mut state = self.state.lock().await;
        let cursor = match cursor {
            None | Some(Value::Null) => state.start,
            given => integer(given, "cursor", 0, state.end)?,
        };
        let truncated = cursor < state.start;
        let cursor = cursor.max(state.start);
        let from = usize::try_from(cursor - state.start).unwrap_or(0);
        let output = String::from_utf8_lossy(&state.output[from..]).into_owned();
        state.observed = Some(state.sequence);
        self.touch();
        Ok(
            json!({"terminal_id":self.id,"output":output,"cursor":state.end,
            "sequence":state.sequence,"truncated":truncated,"running":state.exit.is_none(),
            "exit_code":state.exit}),
        )
    }

    /// Closes the control pipe, then stops the supervisor: SIGTERM, SIGKILL after two
    /// seconds. Runs to the end even when the caller hangs up.
    async fn close(self: &Arc<Self>) {
        let terminal = Arc::clone(self);
        let cleanup = tokio::spawn(async move {
            drop(terminal.state.lock().await.stdin.take());
            for (signal, wait) in [(libc::SIGTERM, 2), (libc::SIGKILL, 1)] {
                let deadline = Instant::now() + Duration::from_secs(wait);
                loop {
                    let changed = terminal.changed.notified();
                    {
                        let state = terminal.state.lock().await;
                        if state.exit.is_some() {
                            return;
                        }
                        if Instant::now() >= deadline {
                            break;
                        }
                        // SAFETY: kill takes plain integers; the supervisor is unreaped
                        // while `exit` is unset, so its id is still its own.
                        unsafe {
                            libc::kill(terminal.pid, signal);
                        }
                    }
                    let _ = tokio::time::timeout(Duration::from_millis(100), changed).await;
                }
            }
        });
        let _ = cleanup.await;
    }

    /// Reads the supervisor's output into the buffer until it ends, then records its exit.
    async fn pump(self: Arc<Self>, mut child: tokio::process::Child) {
        let Some(mut stdout) = child.stdout.take() else {
            return;
        };
        let mut chunk = vec![0; 16384];
        loop {
            let read = stdout.read(&mut chunk).await.unwrap_or(0);
            {
                let mut state = self.state.lock().await;
                if read > 0 {
                    state.output.extend_from_slice(&chunk[..read]);
                    state.end += read as i64;
                    if state.output.len() > LIMIT {
                        let excess = state.output.len() - LIMIT;
                        state.output.drain(..excess);
                        state.start += excess as i64;
                    }
                }
                state.sequence += 1;
            }
            self.changed.notify_waiters();
            if read == 0 {
                break;
            }
        }
        let code = child.wait().await.map_or(-1, |status| {
            status
                .code()
                .unwrap_or_else(|| -status.signal().unwrap_or(0))
        });
        self.state.lock().await.exit = Some(code);
        self.changed.notify_waiters();
    }

    fn summary(&self, state: &State) -> Value {
        json!({"terminal_id":self.id,"command":self.command,"running":state.exit.is_none(),
            "sequence":state.sequence,"cursor":state.end,"preview_ports":self.ports})
    }
}

/// Every open terminal, the per-workspace locks and revocation generations.
#[derive(Default)]
pub struct Terminals {
    inner: StdMutex<Inner>,
}

#[derive(Default)]
struct Inner {
    terminals: Vec<Arc<Terminal>>,
    generations: std::collections::HashMap<PathBuf, u64>,
    locks: std::collections::HashMap<PathBuf, Arc<Mutex<()>>>,
}

/// How a terminal is started: the supervisor command line and the workspace's preview
/// ports.
pub struct Launch {
    pub supervisor: Vec<String>,
    pub ports: Vec<u16>,
}

impl Terminals {
    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn generation(&self, base: &Path) -> u64 {
        self.inner().generations.get(base).copied().unwrap_or(0)
    }

    fn lock(&self, base: &Path) -> Arc<Mutex<()>> {
        Arc::clone(self.inner().locks.entry(base.to_owned()).or_default())
    }

    fn of(&self, base: &Path) -> Vec<Arc<Terminal>> {
        let inner = self.inner();
        inner
            .terminals
            .iter()
            .filter(|terminal| terminal.base == base)
            .cloned()
            .collect()
    }

    fn remove(&self, id: &str) {
        self.inner().terminals.retain(|terminal| terminal.id != id);
    }

    /// The supervisor ids of the workspace's running terminals.
    pub async fn roots(&self, base: &Path) -> Vec<i32> {
        let mut roots = Vec::new();
        for terminal in self.of(base) {
            if terminal.running().await {
                roots.push(terminal.pid);
            }
        }
        roots
    }

    /// Open terminals in the workspace; trusted worker status, never a model tool.
    #[must_use]
    pub fn count(&self, base: &Path) -> usize {
        self.of(base).len()
    }

    /// Revocation and run stop: fences pending opens, then closes the workspace's terminals.
    pub async fn close_all(&self, base: &Path) {
        *self.inner().generations.entry(base.to_owned()).or_insert(0) += 1;
        let lock = self.lock(base);
        let _held = lock.lock().await;
        for terminal in self.of(base) {
            terminal.close().await;
            self.remove(&terminal.id);
        }
    }

    /// Runs one terminal tool in `base` (an admitted, canonical workspace).
    ///
    /// # Errors
    /// The refusal text the caller reports as a failed call.
    pub async fn execute(
        self: &Arc<Self>,
        name: &str,
        args: &Value,
        base: &Path,
        launch: impl FnOnce() -> Result<Launch, String>,
    ) -> Result<Value, String> {
        let Value::Object(args) = args else {
            return Err("arguments must be an object".into());
        };
        let allowed: &[&str] = match name {
            "terminal_open" => &["command", "cols", "rows"],
            "terminal_read" => &["terminal_id", "cursor"],
            "terminal_send" => &["terminal_id", "expected_sequence", "text", "keys"],
            "terminal_wait" => &["terminal_id", "cursor", "timeout_seconds"],
            "terminal_list" => &[],
            "terminal_close" => &["terminal_id"],
            _ => return Err("unknown tool or argument".into()),
        };
        if args.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err("unknown tool or argument".into());
        }
        let generation = self.generation(base);
        let lock = self.lock(base);
        let held = lock.lock().await;
        if self.generation(base) != generation {
            return Err("terminal authority changed during dispatch".into());
        }
        for terminal in self.of(base) {
            if terminal.idle() {
                terminal.close().await;
                self.remove(&terminal.id);
            }
        }
        match name {
            "terminal_open" => return self.open(args, base, generation, launch).await,
            "terminal_list" => {
                let mut terminals = Vec::new();
                for terminal in self.of(base) {
                    let state = terminal.state.lock().await;
                    terminals.push(terminal.summary(&state));
                }
                return Ok(json!({"terminals":terminals}));
            }
            _ => {}
        }
        let wanted = args.get("terminal_id").and_then(Value::as_str);
        let terminal = self
            .of(base)
            .into_iter()
            .find(|terminal| Some(terminal.id.as_str()) == wanted)
            .ok_or("terminal missing in this workspace; restart never replays input")?;
        terminal.touch();
        if name == "terminal_close" {
            terminal.close().await;
            self.remove(&terminal.id);
            let exit = terminal.state.lock().await.exit;
            return Ok(json!({"terminal_id":terminal.id,"closed":true,"exit_code":exit}));
        }
        // Reads, waits and sends do not hold the workspace lock while they wait.
        drop(held);
        match name {
            "terminal_wait" => {
                let timeout = integer(
                    Some(args.get("timeout_seconds").unwrap_or(&json!(5))),
                    "timeout_seconds",
                    1,
                    30,
                )?;
                let changed = terminal.changed.notified();
                let (end, running) = {
                    let state = terminal.state.lock().await;
                    (state.end, state.exit.is_none())
                };
                let cursor = match args.get("cursor") {
                    None => end,
                    given => integer(given, "cursor", 0, end)?,
                };
                if cursor == end && running {
                    let _ =
                        tokio::time::timeout(Duration::from_secs(timeout as u64), changed).await;
                }
                terminal.snapshot(Some(&json!(cursor))).await
            }
            "terminal_read" => {
                tokio::task::yield_now().await;
                terminal.snapshot(args.get("cursor")).await
            }
            _ => send(&terminal, args).await,
        }
    }

    async fn open(
        self: &Arc<Self>,
        args: &Map<String, Value>,
        base: &Path,
        generation: u64,
        launch: impl FnOnce() -> Result<Launch, String>,
    ) -> Result<Value, String> {
        if self.count(base) >= MAX_TERMINALS {
            return Err("workspace terminal limit reached".into());
        }
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .filter(|command| {
                !command.trim().is_empty() && command.len() <= 8192 && !command.contains('\0')
            })
            .ok_or("command must be bounded nonempty text")?
            .to_owned();
        let cols = integer(
            Some(args.get("cols").unwrap_or(&json!(100))),
            "cols",
            20,
            300,
        )?;
        let rows = integer(Some(args.get("rows").unwrap_or(&json!(30))), "rows", 5, 100)?;
        let launch = launch()?;
        let ports = launch.ports.clone();
        let listen = ports
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let base_text = base.to_string_lossy().into_owned();
        // SAFETY: getpid has no preconditions.
        let worker_pid = unsafe { libc::getpid() };
        let spec = json!({"base":base_text,"command":command,"cols":cols,"rows":rows,
            "worker_pid":worker_pid});
        let mut supervisor = tokio::process::Command::new(&launch.supervisor[0]);
        supervisor
            .args(&launch.supervisor[1..])
            .arg(spec.to_string())
            .env_clear()
            .env("PATH", "/opt/rust/bin:/usr/local/bin:/usr/bin:/bin")
            .env("HOME", base)
            .env("TMPDIR", base)
            .env("TERM", "xterm-256color")
            .env("LANG", "C.UTF-8")
            .env("CARGO_HOME", base.join(".cargo"))
            .env("CARGO_BUILD_JOBS", "1")
            .env("V3_ALLOWED_LISTEN_PORTS", listen)
            .current_dir(base)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // SAFETY: setsid is async-signal-safe.
        unsafe {
            supervisor.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = supervisor.spawn().map_err(|error| error.to_string())?;
        let pid = child.id().map_or(0, |pid| pid as i32);
        let terminal = Arc::new(Terminal {
            id: uuid::Uuid::new_v4().to_string(),
            base: base.to_owned(),
            command,
            ports: ports.clone(),
            pid,
            state: Mutex::new(State {
                output: Vec::new(),
                start: 0,
                end: 0,
                sequence: 0,
                observed: None,
                exit: None,
                stdin: child.stdin.take(),
            }),
            changed: Notify::new(),
            updated: StdMutex::new(Instant::now()),
        });
        tokio::spawn(Arc::clone(&terminal).pump(child));
        if self.generation(base) != generation {
            terminal.close().await;
            return Err("terminal authority changed during launch".into());
        }
        self.inner().terminals.push(Arc::clone(&terminal));
        let registry = Arc::downgrade(self);
        let watched = Arc::downgrade(&terminal);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let (Some(registry), Some(terminal)) = (registry.upgrade(), watched.upgrade())
                else {
                    return;
                };
                if !registry
                    .inner()
                    .terminals
                    .iter()
                    .any(|t| Arc::ptr_eq(t, &terminal))
                {
                    return;
                }
                if terminal.idle() {
                    terminal.close().await;
                    registry.remove(&terminal.id);
                    return;
                }
            }
        });
        Ok(
            json!({"terminal_id":terminal.id,"sequence":0,"cursor":0,"running":true,
            "requires_read":true,"preview_ports":ports}),
        )
    }
}

async fn send(terminal: &Terminal, args: &Map<String, Value>) -> Result<Value, String> {
    let mut text = args.get("text").filter(|text| !text.is_null());
    let mut keys = args.get("keys").filter(|keys| !keys.is_null());
    // Schema optional fields may be emitted as empty defaults. Only the absent
    // alternative is normalized; two meaningful payloads still fail.
    let empty_keys = keys.is_some_and(|keys| keys.as_array().is_some_and(Vec::is_empty));
    let given_keys = keys.is_some_and(|keys| keys.as_array().is_some_and(|k| !k.is_empty()));
    if text
        .and_then(Value::as_str)
        .is_some_and(|text| !text.is_empty())
        && empty_keys
    {
        keys = None;
    } else if given_keys && text.and_then(Value::as_str) == Some("") {
        text = None;
    }
    if text.is_none() == keys.is_none() {
        return Err("provide exactly one of text or keys".into());
    }
    let payload = if let Some(text) = text {
        let text = text
            .as_str()
            .filter(|text| text.len() <= 8192 && !text.contains('\0'))
            .ok_or("text exceeds input bound")?;
        text.as_bytes().to_vec()
    } else {
        let keys = keys
            .and_then(Value::as_array)
            .filter(|keys| (1..=32).contains(&keys.len()))
            .ok_or("unsupported terminal key")?;
        let mut payload = Vec::new();
        for name in keys {
            payload.extend_from_slice(
                name.as_str()
                    .and_then(key)
                    .ok_or("unsupported terminal key")?,
            );
        }
        payload
    };
    let expected = integer(
        args.get("expected_sequence"),
        "expected_sequence",
        0,
        i64::MAX,
    )?;
    tokio::task::yield_now().await;
    let mut state = terminal.state.lock().await;
    if expected != state.sequence || state.observed != Some(expected) {
        return Err("stale terminal observation; read before every send".into());
    }
    if state.exit.is_some() {
        return Err("terminal has exited".into());
    }
    state.observed = None;
    let uncertain = || "terminal closed; input outcome uncertain, do not replay".to_owned();
    let stdin = state.stdin.as_mut().ok_or_else(uncertain)?;
    stdin.write_all(&payload).await.map_err(|_| uncertain())?;
    stdin.flush().await.map_err(|_| uncertain())?;
    Ok(json!({"terminal_id":terminal.id,"sent_bytes":payload.len(),
        "sequence":state.sequence,"requires_read":true}))
}

static STOPPING: AtomicBool = AtomicBool::new(false);

extern "C" fn stop(_signal: libc::c_int) {
    STOPPING.store(true, Ordering::SeqCst);
}

/// The terminal supervisor (`webagent-v3 --terminal-supervisor SPEC`): runs the command on
/// a pseudo-terminal, relays its stdin to the terminal and the terminal to its stdout, and
/// ends every descendant when the command, the control pipe or the hour limit ends. The
/// command runs under the same workspace sandbox as `shell` (writes only its workspace,
/// reads system and checkout source files, connects only to web and its own preview ports).
#[must_use]
pub fn supervise(spec: &str) -> i32 {
    match supervised(spec) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("terminal supervisor: {error}");
            1
        }
    }
}

fn os() -> std::io::Error {
    std::io::Error::last_os_error()
}

fn supervised(spec: &str) -> std::io::Result<i32> {
    let invalid =
        |what: &str| std::io::Error::new(std::io::ErrorKind::InvalidInput, what.to_owned());
    let spec: Value = serde_json::from_str(spec).map_err(|_| invalid("terminal spec"))?;
    let command = spec["command"].as_str().ok_or_else(|| invalid("command"))?;
    let dimension = |name: &str| {
        spec[name]
            .as_u64()
            .and_then(|value| u16::try_from(value).ok())
            .ok_or_else(|| invalid(name))
    };
    let (rows, cols) = (dimension("rows")?, dimension("cols")?);
    let base = spec["base"].as_str().ok_or_else(|| invalid("base"))?;
    let ports: Vec<u16> = std::env::var("V3_ALLOWED_LISTEN_PORTS")
        .unwrap_or_default()
        .split(',')
        .filter(|port| !port.is_empty())
        .map(|port| port.parse().map_err(|_| invalid("listen ports")))
        .collect::<std::io::Result<_>>()?;
    let read_paths = crate::runner::command_read_paths();
    let read_paths: Vec<&Path> = read_paths.iter().map(PathBuf::as_path).collect();
    let sandbox = crate::sandbox::prepare(Path::new(base), &read_paths, &ports, true)?;
    let bash = CString::new("/bin/bash")?;
    let argv: Vec<CString> = ["bash", "--noprofile", "--norc", "-c", command]
        .into_iter()
        .map(CString::new)
        .collect::<Result<_, _>>()?;
    let environment: Vec<CString> = std::env::vars_os()
        .filter_map(|(key, value)| {
            let mut pair = key.into_encoded_bytes();
            pair.push(b'=');
            pair.extend(value.into_encoded_bytes());
            CString::new(pair).ok()
        })
        .collect();
    let argv_pointers: Vec<*const libc::c_char> = argv
        .iter()
        .map(|arg| arg.as_ptr())
        .chain([std::ptr::null()])
        .collect();
    let env_pointers: Vec<*const libc::c_char> = environment
        .iter()
        .map(|pair| pair.as_ptr())
        .chain([std::ptr::null()])
        .collect();
    // SAFETY: plain system calls on integers and pointers that outlive them; this process
    // is single-threaded (dispatched before any runtime), so fork is safe.
    unsafe {
        // Errors join the output stream.
        libc::dup2(1, 2);
        // Each supervisor is its own subreaper, so adopted children never cross terminal
        // or workspace ownership.
        if libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0 {
            return Err(os());
        }
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = stop as extern "C" fn(libc::c_int) as usize;
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0) != 0 {
            return Err(os());
        }
        if spec["worker_pid"]
            .as_i64()
            .is_some_and(|worker| i64::from(libc::getppid()) != worker)
        {
            STOPPING.store(true, Ordering::SeqCst);
        }
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let (mut master, mut slave) = (0, 0);
        if libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &size,
        ) != 0
        {
            return Err(os());
        }
        let child = libc::fork();
        if child < 0 {
            return Err(os());
        }
        if child == 0 {
            libc::close(master);
            libc::setsid();
            libc::ioctl(slave, libc::TIOCSCTTY, 0);
            for fd in 0..3 {
                libc::dup2(slave, fd);
            }
            if slave > 2 {
                libc::close(slave);
            }
            // The boundary is applied in the child only: the supervisor must still signal
            // and reap the command outside it.
            if sandbox.apply().is_err() {
                let message = b"terminal sandbox failed\r\n";
                libc::write(2, message.as_ptr().cast(), message.len());
                libc::_exit(126);
            }
            libc::execve(bash.as_ptr(), argv_pointers.as_ptr(), env_pointers.as_ptr());
            let message = b"terminal sandbox or launch failed\r\n";
            libc::write(2, message.as_ptr().cast(), message.len());
            libc::_exit(126);
        }
        libc::close(slave);
        let master = OwnedFd::from_raw_fd(master);
        let status = relay(master.as_raw_fd(), child);
        Ok(status.clamp(0, 255))
    }
}

fn nonblocking(fd: i32) {
    // SAFETY: fcntl on a descriptor this process owns.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
}

/// The supervisor's relay loop, then the cleanup of every descendant; returns the
/// command's exit code (minus the signal when one ended it).
fn relay(master: i32, child: i32) -> i32 {
    nonblocking(master);
    nonblocking(1);
    let started = Instant::now();
    let mut child = Some(child);
    let original = child;
    let mut status = 0;
    let mut pending_input: Vec<u8> = Vec::new();
    let mut pending_output: Vec<u8> = Vec::new();
    let mut eof = false;
    let mut buffer = vec![0u8; 16384];
    let code = |raw: i32| {
        if libc::WIFEXITED(raw) {
            libc::WEXITSTATUS(raw)
        } else {
            -libc::WTERMSIG(raw)
        }
    };
    while !STOPPING.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(3600) {
        let mut fds = Vec::new();
        let watch_master = !eof && pending_output.len() < 65536;
        let master_events = if watch_master { libc::POLLIN } else { 0 }
            | if !pending_input.is_empty() && child.is_some() {
                libc::POLLOUT
            } else {
                0
            };
        fds.push(libc::pollfd {
            fd: if master_events == 0 { -1 } else { master },
            events: master_events,
            revents: 0,
        });
        fds.push(libc::pollfd {
            fd: if pending_input.len() < 65536 && child.is_some() {
                0
            } else {
                -1
            },
            events: libc::POLLIN,
            revents: 0,
        });
        fds.push(libc::pollfd {
            fd: if pending_output.is_empty() { -1 } else { 1 },
            events: libc::POLLOUT,
            revents: 0,
        });
        // SAFETY: poll, read and write on owned descriptors with buffers that outlive them.
        unsafe {
            libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 100);
        }
        let ready = |index: usize, mask: libc::c_short| {
            fds[index].fd >= 0 && fds[index].revents & mask != 0
        };
        let failure = libc::POLLHUP | libc::POLLERR;
        if ready(2, libc::POLLOUT | failure) {
            // SAFETY: as above.
            let written =
                unsafe { libc::write(1, pending_output.as_ptr().cast(), pending_output.len()) };
            if written > 0 {
                pending_output.drain(..written as usize);
            }
        }
        if ready(0, libc::POLLOUT) {
            // SAFETY: as above.
            let written =
                unsafe { libc::write(master, pending_input.as_ptr().cast(), pending_input.len()) };
            if written > 0 {
                pending_input.drain(..written as usize);
            } else if written < 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::WouldBlock
            {
                pending_input.clear();
            }
        }
        let master_ready = ready(0, libc::POLLIN | failure) && watch_master;
        if master_ready {
            // SAFETY: as above.
            let read = unsafe { libc::read(master, buffer.as_mut_ptr().cast(), buffer.len()) };
            if read > 0 {
                pending_output.extend_from_slice(&buffer[..read as usize]);
            } else if read == 0
                || std::io::Error::last_os_error().kind() != std::io::ErrorKind::WouldBlock
            {
                // EIO once the terminal's last writer has gone.
                eof = true;
            }
        }
        if ready(1, libc::POLLIN | failure) {
            // SAFETY: as above.
            let read = unsafe { libc::read(0, buffer.as_mut_ptr().cast(), 8192) };
            if read == 0 {
                break;
            }
            if read > 0 {
                pending_input.extend_from_slice(&buffer[..read as usize]);
            }
        }
        if let Some(pid) = child {
            let mut raw = 0;
            // SAFETY: waitpid on this process's own child.
            if unsafe { libc::waitpid(pid, &mut raw, libc::WNOHANG) } == pid {
                status = code(raw);
                child = None;
            }
        }
        if pending_output.is_empty() && (eof || child.is_none() && !master_ready) {
            break;
        }
    }
    if let Some(pid) = child {
        // SAFETY: killpg on the command's own session.
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
    }
    // Repeat: killing children reparents double-fork descendants here.
    for _ in 0..100 {
        let children = crate::runner::descendants();
        for pid in &children {
            // SAFETY: kill takes plain integers; a vanished pid is ignored.
            unsafe {
                libc::kill(*pid, libc::SIGKILL);
            }
        }
        loop {
            let mut raw = 0;
            // SAFETY: reaping this process's own children.
            let pid = unsafe { libc::waitpid(-1, &mut raw, libc::WNOHANG) };
            if pid <= 0 {
                break;
            }
            if Some(pid) == original {
                status = code(raw);
            }
        }
        if children.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    status
}
