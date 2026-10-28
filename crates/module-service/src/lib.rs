//! Bidirectional framed RPC between the host and module service processes.
//!
//! A frame is a 4-byte big-endian length followed by one JSON message. Either
//! side can call the other, so a module service reaches the host's capability
//! ports over the same private pipe and needs no ambient authority. Calls carry
//! ids and run concurrently; each has a deadline, and losing the peer fails
//! every pending call.
//!
//! `#[module_interface(version = N)]` on a trait generates its service client
//! (`<Trait>Client`) and server (`<Trait>Server`); linked builds call the
//! implementation directly instead.
// shortcut: JSON payloads (serde_json is already locked); switch to a binary
// encoding if module release measurements show serialization cost.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::os::fd::{FromRawFd, OwnedFd};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::oneshot;

pub use webagent_module_interface::module_interface;
#[doc(hidden)]
pub use {anyhow as __anyhow, serde_json as __serde_json};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// One interface served on a peer; implemented by the generated `<Trait>Server`.
pub trait Service: Send + Sync + 'static {
    /// `<Trait>/<version>`, the prefix of every method name this interface answers.
    fn interface(&self) -> &'static str;
    /// The call's result, or None when this interface has no such method.
    fn dispatch(&self, method: &str, args: Value) -> Option<BoxFuture<Result<Value>>>;
}

/// Builds the services a peer answers, given the peer itself so they can call back.
pub type Services = Arc<dyn Fn(&Peer) -> Vec<Arc<dyn Service>> + Send + Sync>;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Largest frame either side may send or accept.
    pub max_frame_bytes: usize,
    /// Deadline for one outgoing call, including nested host callbacks.
    pub call_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_frame_bytes: 16 * 1024 * 1024,
            call_timeout: Duration::from_secs(30),
        }
    }
}

// Error text crossing the boundary is bounded; the receiver only reports it.
const MAX_ERROR_CHARS: usize = 2048;

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Message {
    Call {
        id: u64,
        method: String,
        args: Value,
    },
    Reply {
        id: u64,
        result: Value,
    },
    Error {
        id: u64,
        message: String,
    },
}

type Waiters = HashMap<u64, oneshot::Sender<Result<Value>>>;

struct Shared {
    writer: tokio::sync::Mutex<Pin<Box<dyn AsyncWrite + Send>>>,
    // None once the connection is lost; later calls fail at once.
    pending: Mutex<Option<Waiters>>,
    services: Mutex<Vec<Arc<dyn Service>>>,
    next_id: AtomicU64,
    limits: Limits,
}

/// One end of a module connection: makes calls and answers the other side's calls.
#[derive(Clone)]
pub struct Peer {
    shared: Arc<Shared>,
}

impl Peer {
    /// Connect over `reader`/`writer` and answer incoming calls with `services`.
    /// The returned task ends when the other side closes or breaks the connection.
    pub fn start(
        reader: impl AsyncRead + Send + 'static,
        writer: impl AsyncWrite + Send + 'static,
        limits: Limits,
        services: &Services,
    ) -> (Peer, tokio::task::JoinHandle<Result<()>>) {
        let peer = Peer {
            shared: Arc::new(Shared {
                writer: tokio::sync::Mutex::new(Box::pin(writer)),
                pending: Mutex::new(Some(HashMap::new())),
                services: Mutex::new(Vec::new()),
                next_id: AtomicU64::new(1),
                limits,
            }),
        };
        // Services may hold this peer for callbacks; `close` drops them to end the cycle.
        if let Ok(mut served) = peer.shared.services.lock() {
            *served = services(&peer);
        }
        let task = tokio::spawn(peer.clone().read_loop(Box::pin(reader)));
        (peer, task)
    }

    /// True once the connection is lost.
    pub fn is_closed(&self) -> bool {
        self.waiters().map_or(true, |pending| pending.is_none())
    }

    /// Call `method` on the other side with JSON `args`.
    pub async fn call(&self, method: &str, args: Value) -> Result<Value> {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        match self.waiters()?.as_mut() {
            Some(pending) => pending.insert(id, sender),
            None => bail!("module service unavailable"),
        };
        if let Err(error) = self
            .send(&Message::Call {
                id,
                method: method.to_owned(),
                args,
            })
            .await
        {
            self.forget(id);
            return Err(error);
        }
        match tokio::time::timeout(self.shared.limits.call_timeout, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => bail!("module service unavailable"),
            Err(_) => {
                self.forget(id);
                bail!("module call timed out: {method}")
            }
        }
    }

    fn waiters(&self) -> Result<MutexGuard<'_, Option<Waiters>>> {
        self.shared
            .pending
            .lock()
            .map_err(|_| anyhow!("module connection state poisoned"))
    }

    fn forget(&self, id: u64) {
        if let Ok(mut pending) = self.waiters() {
            pending.as_mut().map(|pending| pending.remove(&id));
        }
    }

    fn settle(&self, id: u64, result: Result<Value>) {
        let waiter = self
            .waiters()
            .ok()
            .and_then(|mut pending| pending.as_mut()?.remove(&id));
        // A waiter that already timed out is gone; its late reply is dropped.
        if let Some(waiter) = waiter {
            let _ = waiter.send(result);
        }
    }

    /// Fail every pending call (dropping its sender), refuse new ones and stop serving.
    fn close(&self) {
        if let Ok(mut pending) = self.waiters() {
            pending.take();
        }
        if let Ok(mut served) = self.shared.services.lock() {
            served.clear();
        }
    }

    async fn send(&self, message: &Message) -> Result<()> {
        let bytes = serde_json::to_vec(message)?;
        if bytes.len() > self.shared.limits.max_frame_bytes {
            bail!("module frame limit");
        }
        let length = u32::try_from(bytes.len()).context("module frame limit")?;
        let mut writer = self.shared.writer.lock().await;
        let written = async {
            writer.write_all(&length.to_be_bytes()).await?;
            writer.write_all(&bytes).await?;
            writer.flush().await
        }
        .await;
        if written.is_err() {
            self.close();
        }
        written.context("module service unavailable")
    }

    async fn read_loop(self, mut reader: Pin<Box<dyn AsyncRead + Send>>) -> Result<()> {
        let result = async {
            while let Some(frame) =
                read_frame(&mut reader, self.shared.limits.max_frame_bytes).await?
            {
                match serde_json::from_slice(&frame).context("module frame rejected")? {
                    Message::Call { id, method, args } => {
                        tokio::spawn(self.clone().answer(id, method, args));
                    }
                    Message::Reply { id, result } => self.settle(id, Ok(result)),
                    Message::Error { id, message } => self.settle(id, Err(anyhow!(message))),
                }
            }
            Ok(())
        }
        .await;
        self.close();
        result
    }

    async fn answer(self, id: u64, method: String, args: Value) {
        let services = self
            .shared
            .services
            .lock()
            .map(|served| served.clone())
            .unwrap_or_default();
        let interface = method
            .rsplit_once('/')
            .map_or("", |(interface, _)| interface);
        let call = services
            .iter()
            .find(|service| service.interface() == interface)
            .and_then(|service| service.dispatch(&method, args));
        let reply = match call {
            Some(call) => match call.await {
                Ok(result) => Message::Reply { id, result },
                Err(error) => Message::Error {
                    id,
                    message: bounded(&format!("{error:#}")),
                },
            },
            None => Message::Error {
                id,
                message: format!("unknown module method: {}", bounded(&method)),
            },
        };
        if let Err(error) = self.send(&reply).await {
            // An oversized reply is still answered, so the caller does not wait for its deadline.
            let message = bounded(&format!("{error:#}"));
            let _ = self.send(&Message::Error { id, message }).await;
        }
    }
}

fn bounded(text: &str) -> String {
    text.chars().take(MAX_ERROR_CHARS).collect()
}

/// The next frame, or None at a clean end of stream between frames.
async fn read_frame(
    reader: &mut Pin<Box<dyn AsyncRead + Send>>,
    max_frame_bytes: usize,
) -> Result<Option<Vec<u8>>> {
    let mut length = [0u8; 4];
    match reader.read_exact(&mut length).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let length = usize::try_from(u32::from_be_bytes(length))?;
    if length > max_frame_bytes {
        bail!("module frame limit");
    }
    let mut frame = vec![0u8; length];
    reader.read_exact(&mut frame).await?;
    Ok(Some(frame))
}

/// A running module service process and the peer on its pipes. Dropping the last
/// handle kills the process, so admitted calls finish on the generation they started on.
pub struct Process {
    peer: Peer,
    _child: tokio::process::Child,
}

impl Process {
    /// Start `command` with only its private pipes: no environment, stdio or working tree.
    pub fn spawn(
        mut command: tokio::process::Command,
        limits: Limits,
        services: &Services,
    ) -> Result<Self> {
        let mut child = command
            .env_clear()
            .current_dir("/")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("module service start")?;
        let (Some(writer), Some(reader)) = (child.stdin.take(), child.stdout.take()) else {
            bail!("module service pipes unavailable");
        };
        let (peer, _) = Peer::start(reader, writer, limits, services);
        Ok(Self {
            peer,
            _child: child,
        })
    }

    pub fn peer(&self) -> &Peer {
        &self.peer
    }
}

enum State {
    Withdrawn,
    Stopped,
    Running(Arc<Process>),
}

/// One module service: started on first use, restarted after a crash, replaced by
/// a new generation on `replace`, and refused while withdrawn.
pub struct Supervised {
    command: Box<dyn Fn() -> tokio::process::Command + Send + Sync>,
    limits: Limits,
    services: Services,
    state: tokio::sync::Mutex<State>,
}

impl Supervised {
    pub fn new(
        command: impl Fn() -> tokio::process::Command + Send + Sync + 'static,
        limits: Limits,
        services: Services,
    ) -> Self {
        Self {
            command: Box::new(command),
            limits,
            services,
            state: tokio::sync::Mutex::new(State::Stopped),
        }
    }

    /// The current generation, started if needed. Hold it for the whole call.
    pub async fn current(&self) -> Result<Arc<Process>> {
        let mut state = self.state.lock().await;
        match &*state {
            State::Withdrawn => bail!("module service withdrawn"),
            State::Running(process) if !process.peer.is_closed() => return Ok(process.clone()),
            State::Running(_) | State::Stopped => {}
        }
        let process = Arc::new(Process::spawn(
            (self.command)(),
            self.limits,
            &self.services,
        )?);
        *state = State::Running(process.clone());
        Ok(process)
    }

    /// Later calls start a new generation; admitted calls finish on the old one.
    pub async fn replace(&self) {
        let mut state = self.state.lock().await;
        if !matches!(*state, State::Withdrawn) {
            *state = State::Stopped;
        }
    }

    /// Refuse later calls until `restore`; admitted calls finish.
    pub async fn withdraw(&self) {
        *self.state.lock().await = State::Withdrawn;
    }

    pub async fn restore(&self) {
        let mut state = self.state.lock().await;
        if matches!(*state, State::Withdrawn) {
            *state = State::Stopped;
        }
    }
}

/// Extra syscalls the service's single-threaded event loop needs beyond pipe I/O.
/// tokio's signal driver (linked with its process support) opens a self-connected
/// socketpair; no other socket can be created.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const EVENT_LOOP_SYSCALLS: [libc::c_long; 6] = [
    libc::SYS_socketpair,
    libc::SYS_epoll_create1,
    libc::SYS_epoll_ctl,
    libc::SYS_epoll_wait,
    libc::SYS_epoll_pwait,
    libc::SYS_eventfd2,
];
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
const EVENT_LOOP_SYSCALLS: [i64; 0] = [];

fn nonblocking(fd: &OwnedFd) -> Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: fcntl on a descriptor this process owns; only the status flags change.
    let set = unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        flags >= 0 && libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) == 0
    };
    if !set {
        bail!("module pipe setup failed");
    }
    Ok(())
}

/// Service process entry: isolate, then answer the host on stdin/stdout with
/// `services` until the host closes the pipes.
pub fn run(limits: Limits, services: Services) -> Result<()> {
    webagent_engine_service::isolate(&EVENT_LOOP_SYSCALLS)?;
    // SAFETY: descriptors 0 and 1 are the host's pipes; this process never touches
    // std's stdin/stdout handles, so these are their only owners.
    let (input, output) = unsafe { (OwnedFd::from_raw_fd(0), OwnedFd::from_raw_fd(1)) };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    for fd in [&input, &output] {
        nonblocking(fd)?;
    }
    runtime.block_on(async {
        // The checked constructors fstat the descriptor, which older glibc issues as
        // `newfstatat`; the seccomp profile denies path syscalls, so skip that probe.
        let reader = tokio::net::unix::pipe::Receiver::from_owned_fd_unchecked(input)?;
        let writer = tokio::net::unix::pipe::Sender::from_owned_fd_unchecked(output)?;
        let (_peer, task) = Peer::start(reader, writer, limits, &services);
        task.await?
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Doubler;

    impl Service for Doubler {
        fn interface(&self) -> &'static str {
            "Doubler/1"
        }

        fn dispatch(&self, method: &str, args: Value) -> Option<BoxFuture<Result<Value>>> {
            match method {
                "Doubler/1/double" => Some(Box::pin(async move {
                    let value = args.as_u64().context("number")?;
                    Ok(json!(value * 2))
                })),
                "Doubler/1/fail" => Some(Box::pin(async { bail!("refused") })),
                _ => None,
            }
        }
    }

    fn pair(limits: Limits) -> (Peer, Peer) {
        let (left, right) = tokio::io::duplex(64 * 1024);
        let (left_read, left_write) = tokio::io::split(left);
        let (right_read, right_write) = tokio::io::split(right);
        let none: Services = Arc::new(|_: &Peer| Vec::new());
        let doubler: Services = Arc::new(|_: &Peer| vec![Arc::new(Doubler) as Arc<dyn Service>]);
        let (client, _) = Peer::start(left_read, left_write, limits, &none);
        let (_server, _) = Peer::start(right_read, right_write, limits, &doubler);
        (client, _server)
    }

    #[tokio::test]
    async fn calls_reply_and_report_remote_errors() {
        let (client, _server) = pair(Limits::default());
        assert_eq!(
            client.call("Doubler/1/double", json!(21)).await.ok(),
            Some(json!(42))
        );
        let failed = client.call("Doubler/1/fail", Value::Null).await;
        assert!(format!("{:#}", failed.expect_err("error")).contains("refused"));
        let unknown = client.call("Doubler/1/missing", Value::Null).await;
        assert!(format!("{:#}", unknown.expect_err("error")).contains("unknown module method"));
    }

    #[tokio::test]
    async fn oversized_frames_fail_without_waiting_for_the_deadline() {
        let limits = Limits {
            max_frame_bytes: 256,
            call_timeout: Duration::from_secs(5),
        };
        let (client, _server) = pair(limits);
        let started = std::time::Instant::now();
        let result = client
            .call("Doubler/1/double", json!("x".repeat(1024)))
            .await;
        assert!(format!("{:#}", result.expect_err("error")).contains("module frame limit"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn a_lost_connection_fails_calls_at_once() {
        let (left, right) = tokio::io::duplex(64);
        drop(right);
        let (read, write) = tokio::io::split(left);
        let none: Services = Arc::new(|_: &Peer| Vec::new());
        let (orphan, task) = Peer::start(read, write, Limits::default(), &none);
        let _ = task.await;
        assert!(orphan.is_closed());
        let result = orphan.call("Doubler/1/double", json!(1)).await;
        assert!(format!("{:#}", result.expect_err("error")).contains("unavailable"));
    }

    #[test]
    fn errors_crossing_the_boundary_are_bounded() {
        assert_eq!(
            bounded(&"e".repeat(MAX_ERROR_CHARS * 2)).len(),
            MAX_ERROR_CHARS
        );
    }
}
