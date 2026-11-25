//! Existing bounded Unix worker/protected-host RPC transport; trusted metadata is supplied by Rust composition.
use super::{failure, outcome};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;
pub async fn execute(
    name: &str,
    args: &Value,
    session: &str,
    socket: &std::path::Path,
    cancel: CancellationToken,
) -> Result<Value> {
    if !matches!(
        name,
        "file_list"
            | "file_read"
            | "file_search"
            | "file_write"
            | "file_edit"
            | "checkout_edit"
            | "workspace_seed"
            | "source_review"
            | "source_patch"
            | "shell"
            | "python"
            | "git_tool"
            | "resolve_conflict"
            | "commit_and_push"
            | "terminal_open"
            | "terminal_read"
            | "terminal_send"
            | "terminal_wait"
            | "terminal_list"
            | "terminal_close"
    ) {
        bail!("unknown tool");
    }
    execute_trusted(name, args, session, socket, None, cancel).await
}

// Only Rust supplies trusted browser/host metadata; it is not a model schema.
pub async fn execute_trusted(
    name: &str,
    args: &Value,
    session: &str,
    socket: &std::path::Path,
    trusted: Option<Value>,
    cancel: CancellationToken,
) -> Result<Value> {
    execute_rpc(name, args, session, socket, trusted, cancel, false).await
}
/// Explicit protected-host protocol; this endpoint is not the low-level worker.
pub async fn execute_host_trusted(
    name: &str,
    args: &Value,
    session: &str,
    socket: &std::path::Path,
    trusted: Option<Value>,
    cancel: CancellationToken,
) -> Result<Value> {
    execute_rpc(name, args, session, socket, trusted, cancel, true).await
}
/// The live limits a worker call carries in `trusted.limits` (stream D1): the command limits
/// (`config/worker/command-limits.json`) for commands and terminals, the search budgets of
/// `config/context/context-compaction.json` for searches. The worker applies them instead of
/// reading the checkout, so a worker without it keeps the live values. `None` (the worker then
/// reads its own copy, as before) for other tools or when the file is unreadable or too large;
/// the worker validates what it gets exactly as it validates the file.
fn call_limits(name: &str, checkout: &std::path::Path) -> Option<Value> {
    let read = |relative: &str, max_bytes: u64| -> Option<Value> {
        let path = checkout.join(relative);
        if std::fs::metadata(&path).ok()?.len() > max_bytes {
            return None;
        }
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    };
    match name {
        "shell" | "python" | "terminal_open" => {
            Some(json!({"command": read("config/worker/command-limits.json", 16 * 1024)?}))
        }
        "file_search" | "file_list" => {
            let config = read("config/context/context-compaction.json", 256 * 1024)?;
            let search: serde_json::Map<String, Value> =
                ["search_output_bytes", "search_line_bytes"]
                    .into_iter()
                    .filter_map(|key| Some((key.to_owned(), config.get(key)?.clone())))
                    .collect();
            Some(json!({ "search": search }))
        }
        _ => None,
    }
}

/// Retry and job-table settings for worker calls (stream D, step W1), read per call from
/// `config/worker/transport.json`.
#[derive(Clone, Debug, Default, PartialEq)]
struct Settings {
    /// The waits before each retry of a call the worker never took; empty: no retry.
    delays: Vec<std::time::Duration>,
    /// The worker's job-table bounds, sent with each call; `None`: the worker keeps its own.
    job_table: Option<Value>,
}

impl Settings {
    /// Reads `config/worker/transport.json` under `checkout`. An absent, unreadable or
    /// invalid file means no retry, as before W1. Retry `n` waits
    /// `retry_first_delay_ms * 2^n`.
    fn read(checkout: &std::path::Path) -> Self {
        let path = checkout.join("config/worker/transport.json");
        let config = std::fs::metadata(&path)
            .ok()
            .filter(|meta| meta.len() <= 16 * 1024)
            .and_then(|_| std::fs::read(&path).ok())
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        let Some(config) = config.filter(Value::is_object) else {
            return Self::default();
        };
        let retries = config["retries"].as_u64().unwrap_or(0).min(10);
        let first = config["retry_first_delay_ms"]
            .as_u64()
            .unwrap_or(250)
            .clamp(10, 5000);
        Self {
            delays: (0..retries)
                .map(|n| std::time::Duration::from_millis((first << n).min(30_000)))
                .collect(),
            job_table: Some(config["job_table"].clone()).filter(Value::is_object),
        }
    }
}

/// Which protocol a call speaks.
enum Peer<'a> {
    /// The protected host's flat envelope: one line each way, no acknowledgement.
    Host,
    /// The tool worker's protocol 2 with acknowledgement, retry and resume.
    Worker(&'a Settings),
}

/// A socket file's identity (device, inode, change time): a restarted worker binds a new
/// file, so a caller can tell the worker it learned about from a replacement.
type Identity = (u64, u64, i64, i64);

#[cfg(unix)]
fn identity(socket: &std::path::Path) -> Option<Identity> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(socket).ok()?;
    Some((meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec()))
}

/// Named pipes have no file identity: a call there is never retried after it was sent.
#[cfg(not(unix))]
fn identity(_socket: &std::path::Path) -> Option<Identity> {
    None
}

/// The sockets on which a worker that sends acknowledgements answered this process, with
/// the identity of the socket file it answered on. Only such a worker is retried after a
/// connection that ended before any line: an older worker may have run the call. (A refused
/// connect sent nothing and is always retried.) A process talks to one or two workers.
static ACKED: std::sync::Mutex<Vec<(std::path::PathBuf, Identity)>> =
    std::sync::Mutex::new(Vec::new());

fn acked_on(socket: &std::path::Path) -> Option<Identity> {
    let acked = ACKED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    acked
        .iter()
        .find(|(path, _)| path == socket)
        .map(|(_, identity)| *identity)
}

fn learn(socket: &std::path::Path, identity: Identity) {
    let mut acked = ACKED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    acked.retain(|(path, _)| path != socket);
    if acked.len() >= 64 {
        acked.remove(0);
    }
    acked.push((socket.to_owned(), identity));
}

/// A worker line that precedes the reply.
enum Frame {
    /// The worker registered the call and has not started it; its instance id.
    Ack(String),
    /// A dispatcher holds the call in a queue (W2); ignored.
    Queued,
}

/// The acknowledgement or queued line for call `id`; `None` for any other line, which is
/// then validated as the reply.
fn frame(line: &str, id: &str) -> Option<Frame> {
    if line.len() > 4096 {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("result").is_some() || value["version"] != 2 || value["id"] != id {
        return None;
    }
    if value["ack"] == true {
        return value["instance"]
            .as_str()
            .map(|instance| Frame::Ack(instance.to_owned()));
    }
    (value["queued"] == true).then_some(Frame::Queued)
}

/// Most acknowledgement and queued lines read before the reply.
const MAX_FRAMES: usize = 4;

/// The fixed parts of one call's connections.
struct Link<'a, F> {
    socket: &'a std::path::Path,
    id: &'a str,
    cancel: &'a CancellationToken,
    fail: &'a F,
}

/// What one connection produced.
enum Attempt {
    /// The reply line, still to be validated, and the instance that acknowledged it.
    Reply(String, Option<String>),
    /// Connecting failed: nothing was sent.
    NotConnected(failure::Failure),
    /// The connection ended before any line. `true` when the socket file is the one an
    /// acknowledging worker answered on: that worker never registered the call, so nothing ran.
    Lost(failure::Failure, bool),
    /// Acknowledged by worker `instance` on the socket file `identity`, then ended.
    Acked(failure::Failure, Option<Identity>, String),
}

async fn attempt<F>(link: &Link<'_, F>, encoded: &[u8], frames: bool) -> Result<Attempt>
where
    F: Fn(
        failure::Stage,
        failure::Code,
        failure::Delivery,
        Option<&std::io::Error>,
    ) -> failure::Failure,
{
    use failure::{Code, Delivery, Stage};
    let fail = link.fail;
    let before = identity(link.socket);
    let mut stream = match webagent_platform::ipc::connect(link.socket).await {
        Ok(stream) => stream,
        Err(error) => {
            let code = match error.kind() {
                std::io::ErrorKind::NotFound => Code::SocketUnavailable,
                std::io::ErrorKind::ConnectionRefused => Code::ConnectionRefused,
                _ => Code::IoFailure,
            };
            let failure = fail(Stage::Connect, code, Delivery::NotSent, Some(&error));
            return Ok(Attempt::NotConnected(failure));
        }
    };
    // The file connected to: unchanged across the connect, so no restart raced it.
    let connected = before.filter(|before| identity(link.socket).as_ref() == Some(before));
    // Cancellation may race a partial write. Delivery is deliberately conservative.
    tokio::select! {
        _ = link.cancel.cancelled() => Err(fail(Stage::RequestWrite, Code::Cancelled, Delivery::PossiblySent, None).into()),
        result = stream.write_all(encoded) => result.map_err(|error| anyhow::Error::from(fail(Stage::RequestWrite, Code::IoFailure, Delivery::PossiblySent, Some(&error)))),
    }?;
    let mut reader = BufReader::new(stream).take(2 * 1024 * 1024);
    let mut acked = None;
    for _ in 0..=MAX_FRAMES {
        let mut line = String::new();
        let read = tokio::select! {
            _ = link.cancel.cancelled() => Err(fail(Stage::ResponseRead, Code::Cancelled, Delivery::PossiblySent, None)),
            result = reader.read_line(&mut line) => Ok(result),
        }?;
        let lost = match read {
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                let failure = fail(
                    Stage::ResponseValidation,
                    Code::MalformedResponse,
                    Delivery::ResponseReceived,
                    Some(&error),
                );
                return Err(failure.into());
            }
            Err(error) => fail(
                Stage::ResponseRead,
                Code::IoFailure,
                Delivery::PossiblySent,
                Some(&error),
            ),
            Ok(_) if line.is_empty() => fail(
                Stage::ResponseRead,
                Code::DisconnectedResponse,
                Delivery::PossiblySent,
                None,
            ),
            Ok(_) => match frames.then(|| frame(&line, link.id)).flatten() {
                Some(Frame::Ack(instance)) if acked.is_none() => {
                    if let Some(identity) = connected {
                        learn(link.socket, identity);
                    }
                    acked = Some(instance);
                    continue;
                }
                Some(Frame::Queued) => continue,
                Some(Frame::Ack(_)) => break,
                None => return Ok(Attempt::Reply(line, acked)),
            },
        };
        return Ok(match acked {
            Some(instance) => Attempt::Acked(lost, connected, instance),
            None => {
                let retry = connected.is_some() && connected == acked_on(link.socket);
                Attempt::Lost(lost, retry)
            }
        });
    }
    let failure = fail(
        Stage::ResponseValidation,
        Code::MalformedResponse,
        Delivery::ResponseReceived,
        None,
    );
    Err(failure.into())
}

/// Asks the worker that acknowledged the call for its outcome; a resume never runs
/// anything. `None` when it cannot answer: the socket file changed (the worker restarted
/// and its job table went with it), another instance answered, it keeps nothing for the
/// call, or the schedule ran out.
async fn resume<F>(
    link: &Link<'_, F>,
    name: &str,
    session: &str,
    acked: Option<Identity>,
    instance: &str,
    delays: &[std::time::Duration],
) -> Result<Option<String>>
where
    F: Fn(
        failure::Stage,
        failure::Code,
        failure::Delivery,
        Option<&std::io::Error>,
    ) -> failure::Failure,
{
    use failure::{Code, Delivery, Stage};
    let Some(acked) = acked else {
        return Ok(None);
    };
    let mut request = json!({"version":webagent_context_contracts::execution::WORKER_PROTOCOL,
        "id":link.id,"name":name,"workspace":session,"ack":true,"resume":true})
    .to_string()
    .into_bytes();
    request.push(b'\n');
    for delay in std::iter::once(&std::time::Duration::ZERO).chain(delays) {
        tokio::select! {
            _ = link.cancel.cancelled() => return Err((link.fail)(Stage::ResponseRead, Code::Cancelled, Delivery::PossiblySent, None).into()),
            () = tokio::time::sleep(*delay) => {}
        }
        if identity(link.socket) != Some(acked) {
            return Ok(None);
        }
        if let Attempt::Reply(line, by) = attempt(link, &request, true).await? {
            let unknown = serde_json::from_str::<Value>(&line)
                .is_ok_and(|value| value["result"]["error_kind"] == "unknown_job");
            return Ok((by.as_deref() == Some(instance) && !unknown).then_some(line));
        }
    }
    Ok(None)
}

async fn execute_rpc(
    name: &str,
    args: &Value,
    session: &str,
    socket: &std::path::Path,
    trusted: Option<Value>,
    cancel: CancellationToken,
    host: bool,
) -> Result<Value> {
    if host {
        return execute_on(name, args, session, socket, trusted, cancel, Peer::Host).await;
    }
    let settings = Settings::read(&webagent_configuration::paths::checkout());
    execute_on(
        name,
        args,
        session,
        socket,
        trusted,
        cancel,
        Peer::Worker(&settings),
    )
    .await
}

/// One call. To the worker, it asks for an acknowledgement: a call the worker never took
/// (refused connect, or a connection that ended before the acknowledgement) is retried on
/// the settings' schedule and, once that runs out, fails as not sent and flagged
/// `retried` (the run reports "did not start, no effect"); a call acknowledged and then
/// cut off is resumed from the same worker, or stays possibly sent.
async fn execute_on(
    name: &str,
    args: &Value,
    session: &str,
    socket: &std::path::Path,
    trusted: Option<Value>,
    cancel: CancellationToken,
    peer: Peer<'_>,
) -> Result<Value> {
    use failure::{Code, Delivery, Failure, Stage};
    let host = matches!(peer, Peer::Host);
    let start = std::time::Instant::now();
    let request_id = uuid::Uuid::new_v4().to_string();
    let fail = |stage, code, delivery, io: Option<&std::io::Error>| {
        let mut error = Failure::transport(stage, code, delivery, start, &request_id, io);
        error.request_id = args["request_id"]
            .as_str()
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 128
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-:.".contains(&b))
            })
            .map(str::to_owned);
        error
    };
    uuid::Uuid::parse_str(session).map_err(|_| {
        fail(
            Stage::Preparation,
            Code::InvalidRequest,
            Delivery::NotSent,
            None,
        )
    })?;
    if cancel.is_cancelled() {
        return Err(fail(Stage::Preparation, Code::Cancelled, Delivery::NotSent, None).into());
    }
    let timeout = args["timeout_seconds"]
        .as_u64()
        .unwrap_or(120)
        .clamp(1, 600);
    let mut request = json!({"id":request_id,"name":name,"arguments":args,"workspace":session,"timeout_seconds":timeout,"trusted":trusted});
    let mut delays: &[std::time::Duration] = &[];
    if let Peer::Worker(settings) = peer {
        request["version"] = json!(webagent_context_contracts::execution::WORKER_PROTOCOL);
        request["ack"] = json!(true);
        if let Some(bounds) = &settings.job_table {
            request["job_table"] = bounds.clone();
        }
        delays = &settings.delays;
        if let Some(limits) = call_limits(name, &webagent_configuration::paths::checkout()) {
            let trusted = &mut request["trusted"];
            if trusted.is_null() {
                *trusted = json!({});
            }
            if let Some(trusted) = trusted.as_object_mut() {
                trusted.entry("limits").or_insert(limits);
            }
        }
    }
    let mut encoded = serde_json::to_vec(&request).map_err(|_| {
        fail(
            Stage::Preparation,
            Code::InvalidRequest,
            Delivery::NotSent,
            None,
        )
    })?;
    if encoded.len() > 1024 * 1024 {
        return Err(fail(
            Stage::Preparation,
            Code::InvalidRequest,
            Delivery::NotSent,
            None,
        )
        .into());
    }
    encoded.push(b'\n');
    let link = Link {
        socket,
        id: &request_id,
        cancel: &cancel,
        fail: &fail,
    };
    let mut waits = delays.iter();
    let mut retried = false;
    let response = loop {
        // Each failure here proves the worker never took the call.
        let (failure, refused_connect) = match attempt(&link, &encoded, !host).await? {
            Attempt::Reply(line, _) => break line,
            Attempt::NotConnected(failure) => (failure, true),
            Attempt::Lost(failure, true) => (failure, false),
            Attempt::Lost(failure, false) => return Err(failure.into()),
            Attempt::Acked(failure, identity, instance) => {
                match resume(&link, name, session, identity, &instance, delays).await? {
                    Some(line) => break line,
                    None => return Err(failure.into()),
                }
            }
        };
        let Some(wait) = waits.next() else {
            if !retried {
                return Err(failure.into());
            }
            let failure = if refused_connect {
                Failure { retried, ..failure }
            } else {
                Failure {
                    stage: Stage::Acknowledge,
                    code: Code::NotAcknowledged,
                    delivery: Delivery::NotSent,
                    retried,
                    ..failure
                }
            };
            return Err(failure.into());
        };
        retried = true;
        tokio::select! {
            _ = cancel.cancelled() => return Err(fail(Stage::Connect, Code::Cancelled, Delivery::NotSent, None).into()),
            () = tokio::time::sleep(*wait) => {}
        }
    };
    let value: Value = serde_json::from_str(&response).map_err(|_| {
        let code = if response.len() >= 2 * 1024 * 1024 && !response.ends_with('\n') {
            Code::ResponseTooLarge
        } else {
            Code::MalformedResponse
        };
        fail(
            Stage::ResponseValidation,
            code,
            Delivery::ResponseReceived,
            None,
        )
    })?;
    if value["id"].as_str() != Some(request_id.as_str()) {
        return Err(fail(
            Stage::ResponseValidation,
            Code::ResponseIdMismatch,
            Delivery::ResponseReceived,
            None,
        )
        .into());
    }
    let content = if host {
        webagent_context_contracts::execution::host_reply(value, &request_id)
    } else {
        webagent_context_contracts::execution::worker_reply(value, &request_id)
    }
    .map_err(|_| {
        fail(
            Stage::ResponseValidation,
            Code::MalformedResponse,
            Delivery::ResponseReceived,
            None,
        )
    })?;
    outcome::rpc(if host { "build-host" } else { "tool-worker" }, &request_id).map_err(|_| {
        fail(
            Stage::ResponseValidation,
            Code::InternalError,
            Delivery::ResponseReceived,
            None,
        )
    })?;
    Ok(content)
}

#[cfg(test)]
mod limit_tests {
    use super::call_limits;
    use serde_json::json;

    #[test]
    fn commands_and_searches_carry_the_live_limits_others_none() {
        let dir = tempfile::tempdir().expect("temporary directory");
        for folder in ["config/worker", "config/context"] {
            std::fs::create_dir_all(dir.path().join(folder)).expect("folder");
        }
        // Absent files: nothing is sent and the worker reads its own copy, as before D1.
        assert_eq!(call_limits("shell", dir.path()), None);
        assert_eq!(call_limits("file_search", dir.path()), None);
        let command = json!({"schema_version":1,"memory_mb":512,"processes":64,"cpu_seconds":60});
        std::fs::write(
            dir.path().join("config/worker/command-limits.json"),
            command.to_string(),
        )
        .expect("write");
        std::fs::write(
            dir.path().join("config/context/context-compaction.json"),
            json!({"search_output_bytes":8192,"search_line_bytes":512,"other":1}).to_string(),
        )
        .expect("write");
        for name in ["shell", "python", "terminal_open"] {
            assert_eq!(
                call_limits(name, dir.path()),
                Some(json!({ "command": command }))
            );
        }
        for name in ["file_search", "file_list"] {
            assert_eq!(
                call_limits(name, dir.path()),
                Some(json!({"search":{"search_output_bytes":8192,"search_line_bytes":512}}))
            );
        }
        for name in ["file_read", "file_write", "git_tool", "terminal_read"] {
            assert_eq!(call_limits(name, dir.path()), None);
        }
        // A file the worker would refuse as too large is not sent either.
        std::fs::write(
            dir.path().join("config/worker/command-limits.json"),
            " ".repeat(16 * 1024 + 1),
        )
        .expect("write");
        assert_eq!(call_limits("shell", dir.path()), None);
    }
}

#[cfg(test)]
mod ack_tests {
    //! The caller's side of W1 against a scripted worker on a real Unix socket. No process
    //! starts and no step depends on timing: every retry waits zero.
    use super::{execute_on, Peer, Settings};
    use crate::tools::failure::{refused_before_dispatch, refused_result, Failure};
    use serde_json::{json, Value};
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    use tokio_util::sync::CancellationToken;

    const SESSION: &str = "00000000-0000-4000-8000-000000000001";

    /// What the scripted worker does with one connection after reading its request.
    #[derive(Clone, Copy)]
    enum Step {
        /// An older worker: the result line only.
        Answer,
        Ack,
        /// Close without a line (a worker that never registered the call).
        Drop,
        /// Acknowledge, then close without the result.
        AckDrop,
        /// Acknowledge as another worker instance, then answer.
        OtherInstance,
        /// Acknowledge, then answer that no result is kept.
        Unknown,
        /// `n` queued lines, then acknowledge and answer.
        Queued(usize),
    }

    fn worker(listener: UnixListener, steps: Vec<Step>) -> tokio::task::JoinHandle<Vec<Value>> {
        tokio::spawn(async move {
            let mut seen = Vec::new();
            for step in steps {
                let (stream, _) = listener.accept().await.expect("accept");
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.expect("request");
                let request: Value = serde_json::from_str(&line).expect("request JSON");
                let id = &request["id"];
                let ack = |instance: &str| {
                    json!({"version":2,"id":id,"ack":true,"instance":instance}).to_string() + "\n"
                };
                let answer =
                    |result: Value| json!({"version":2,"id":id,"result":result}).to_string() + "\n";
                let ok = answer(json!({"status":"success","output":"ok"}));
                let text = match step {
                    Step::Answer => ok,
                    Step::Ack => ack("one") + &ok,
                    Step::Drop => String::new(),
                    Step::AckDrop => ack("one"),
                    Step::OtherInstance => ack("two") + &ok,
                    Step::Unknown => {
                        ack("one") + &answer(json!({"status":"failed","error_kind":"unknown_job"}))
                    }
                    Step::Queued(n) => {
                        let queued = json!({"version":2,"id":id,"queued":true}).to_string() + "\n";
                        queued.repeat(n) + &ack("one") + &ok
                    }
                };
                let mut stream = reader.into_inner();
                let _ = stream.write_all(text.as_bytes()).await;
                seen.push(request);
            }
            seen
        })
    }

    fn settings(retries: usize) -> Settings {
        Settings {
            delays: vec![Duration::ZERO; retries],
            job_table: Some(json!({"max_jobs":64})),
        }
    }

    async fn call(socket: &Path, settings: &Settings) -> anyhow::Result<Value> {
        tokio::time::timeout(
            Duration::from_secs(10),
            execute_on(
                "file_write",
                &json!({"path":"a.txt","content":"x"}),
                SESSION,
                socket,
                None,
                CancellationToken::new(),
                Peer::Worker(settings),
            ),
        )
        .await
        .expect("call finished")
    }

    fn failure(result: anyhow::Result<Value>) -> Value {
        json!(result
            .expect_err("call failed")
            .downcast_ref::<Failure>()
            .expect("typed"))
    }

    fn socket() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temporary directory");
        let socket = dir.path().join("tools.sock");
        (dir, socket)
    }

    #[tokio::test]
    async fn a_call_cut_off_before_its_acknowledgement_is_retried_with_the_same_id() {
        let (_dir, path) = socket();
        let server = worker(
            UnixListener::bind(&path).expect("bind"),
            vec![Step::Ack, Step::Drop, Step::Ack],
        );
        // The first call learns that this socket's worker acknowledges.
        assert_eq!(
            call(&path, &settings(2)).await.expect("first")["output"],
            "ok"
        );
        assert_eq!(
            call(&path, &settings(2)).await.expect("retried")["output"],
            "ok"
        );
        let seen = server.await.expect("worker");
        assert_eq!(seen[1]["id"], seen[2]["id"], "the retry reuses the job id");
        assert_ne!(seen[0]["id"], seen[1]["id"]);
        for request in &seen {
            assert_eq!(request["ack"], true);
            assert_eq!(request["job_table"], json!({"max_jobs":64}));
            assert!(request.get("resume").is_none());
        }
    }

    #[tokio::test]
    async fn a_call_never_acknowledged_through_the_schedule_did_not_start() {
        let (_dir, path) = socket();
        let steps = vec![Step::Ack, Step::Drop, Step::Drop, Step::Drop];
        let server = worker(UnixListener::bind(&path).expect("bind"), steps);
        call(&path, &settings(2)).await.expect("learn");
        let value = failure(call(&path, &settings(2)).await);
        assert_eq!(value["stage"], "acknowledge");
        assert_eq!(value["code"], "not_acknowledged");
        assert_eq!(value["delivery"], "not_sent");
        assert_eq!(value["retried"], true);
        let refused = refused_before_dispatch(&value).expect("no effect");
        let result = refused_result(&refused);
        assert_eq!(result["error_kind"], "not_started");
        assert_eq!(result["no_effect"], true);
        assert_eq!(server.await.expect("worker").len(), 4);
    }

    #[tokio::test]
    async fn a_refused_connect_is_retried_then_did_not_start() {
        let (_dir, path) = socket();
        // A missing socket file and a stale one (its worker gone, as while it restarts).
        let value = failure(call(&path, &settings(3)).await);
        assert_eq!(value["code"], "socket_unavailable");
        assert_eq!(value["retried"], true);
        drop(std::os::unix::net::UnixListener::bind(&path).expect("bind"));
        let value = failure(call(&path, &settings(3)).await);
        assert_eq!(value["stage"], "connect");
        assert_eq!(value["code"], "connection_refused");
        assert_eq!(value["retried"], true);
        assert_eq!(
            refused_result(&refused_before_dispatch(&value).expect("no effect"))["error_kind"],
            "not_started"
        );
        // With no retries configured nothing changes from before W1.
        let value = failure(call(&path, &settings(0)).await);
        assert!(value.get("retried").is_none());
        assert!(refused_before_dispatch(&value).is_none());
    }

    #[tokio::test]
    async fn an_older_or_replaced_worker_that_closes_without_a_line_stays_possibly_sent() {
        let (_dir, path) = socket();
        // An older worker answers without an acknowledgement; its silence is uncertain.
        let server = worker(
            UnixListener::bind(&path).expect("bind"),
            vec![Step::Answer, Step::Drop],
        );
        assert_eq!(
            call(&path, &settings(3)).await.expect("old")["output"],
            "ok"
        );
        let value = failure(call(&path, &settings(3)).await);
        assert_eq!(value["code"], "disconnected_response");
        assert_eq!(value["delivery"], "possibly_sent");
        assert_eq!(server.await.expect("worker").len(), 2, "never retried");
        // An acknowledging worker was replaced by a new socket file (a restart, maybe a
        // rollback to an older worker): its silence is uncertain too.
        let (_dir, path) = socket();
        let server = worker(UnixListener::bind(&path).expect("bind"), vec![Step::Ack]);
        call(&path, &settings(3)).await.expect("learn");
        server.await.expect("worker");
        std::fs::remove_file(&path).expect("remove");
        let server = worker(UnixListener::bind(&path).expect("bind"), vec![Step::Drop]);
        let value = failure(call(&path, &settings(3)).await);
        assert_eq!(value["delivery"], "possibly_sent");
        assert_eq!(server.await.expect("worker").len(), 1, "never retried");
    }

    #[tokio::test]
    async fn a_call_cut_off_after_its_acknowledgement_is_resumed_never_rerun() {
        let (_dir, path) = socket();
        let server = worker(
            UnixListener::bind(&path).expect("bind"),
            vec![Step::AckDrop, Step::Ack],
        );
        assert_eq!(
            call(&path, &settings(2)).await.expect("resumed")["output"],
            "ok"
        );
        let seen = server.await.expect("worker");
        assert_eq!(seen[1]["resume"], true);
        assert_eq!(seen[1]["id"], seen[0]["id"]);
        assert_eq!(seen[1]["name"], "file_write");
        assert!(
            seen[1].get("arguments").is_none(),
            "a resume carries no arguments"
        );
        // Another instance answering, or no kept result: possibly sent, as before W1.
        for step in [Step::OtherInstance, Step::Unknown] {
            let (_dir, path) = socket();
            let server = worker(
                UnixListener::bind(&path).expect("bind"),
                vec![Step::AckDrop, step],
            );
            let value = failure(call(&path, &settings(2)).await);
            assert_eq!(value["delivery"], "possibly_sent");
            assert_eq!(value["code"], "disconnected_response");
            server.await.expect("worker");
        }
    }

    #[tokio::test]
    async fn queued_lines_are_skipped_up_to_the_bound() {
        let (_dir, path) = socket();
        let server = worker(
            UnixListener::bind(&path).expect("bind"),
            vec![Step::Queued(3), Step::Queued(4)],
        );
        assert_eq!(
            call(&path, &settings(0)).await.expect("queued")["output"],
            "ok"
        );
        // Four queued lines and an acknowledgement exceed the four lines read before a reply.
        let value = failure(call(&path, &settings(0)).await);
        assert_eq!(value["code"], "malformed_response");
        server.await.expect("worker");
    }

    #[tokio::test]
    async fn a_stop_during_a_retry_wait_is_a_cancellation_not_sent() {
        let (_dir, path) = socket();
        let server = worker(UnixListener::bind(&path).expect("bind"), vec![Step::Ack]);
        call(&path, &settings(0)).await.expect("learn");
        server.await.expect("worker");
        let waiting = Settings {
            delays: vec![Duration::from_secs(600)],
            job_table: None,
        };
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let task = tokio::spawn(async move {
            execute_on(
                "file_write",
                &json!({}),
                SESSION,
                &path,
                None,
                cancel,
                Peer::Worker(&waiting),
            )
            .await
        });
        stop.cancel();
        let value = failure(
            tokio::time::timeout(Duration::from_secs(10), task)
                .await
                .expect("stopped")
                .expect("task"),
        );
        assert_eq!(value["code"], "cancelled");
        assert_eq!(value["delivery"], "not_sent");
    }

    #[test]
    fn settings_come_from_the_live_file_or_mean_no_retry() {
        let dir = tempfile::tempdir().expect("temporary directory");
        assert_eq!(Settings::read(dir.path()), Settings::default());
        std::fs::create_dir_all(dir.path().join("config/worker")).expect("folder");
        let file = dir.path().join("config/worker/transport.json");
        std::fs::write(&file, "not json").expect("write");
        assert_eq!(Settings::read(dir.path()), Settings::default());
        let table = json!({"ttl_seconds":600,"max_jobs":1024});
        std::fs::write(
            &file,
            json!({"retries":6,"retry_first_delay_ms":250,"job_table":table}).to_string(),
        )
        .expect("write");
        let read = Settings::read(dir.path());
        let delays: Vec<u128> = read.delays.iter().map(Duration::as_millis).collect();
        assert_eq!(delays, [250, 500, 1000, 2000, 4000, 8000]);
        assert_eq!(read.job_table, Some(table));
        // Out-of-range values are clamped; one wait is at most 30 s.
        std::fs::write(
            &file,
            json!({"retries":99,"retry_first_delay_ms":1}).to_string(),
        )
        .expect("write");
        let read = Settings::read(dir.path());
        assert_eq!(read.delays.len(), 10);
        assert_eq!(read.delays[0], Duration::from_millis(10));
        assert_eq!(read.job_table, None);
        std::fs::write(
            &file,
            json!({"retries":10,"retry_first_delay_ms":5000}).to_string(),
        )
        .expect("write");
        assert_eq!(
            Settings::read(dir.path()).delays[9],
            Duration::from_secs(30)
        );
    }
}
