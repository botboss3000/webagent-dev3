//! The v3 tool worker. It owns the tool socket the API and loop call and accepts only the
//! application identity. File tools ([`files`]) and workspace commands ([`commands`]) run
//! here; every other request is forwarded unchanged to the Python worker on a private
//! socket. Tool groups move from the Python worker into this crate one at a time (Rust
//! conversion item A2).
pub mod commands;
pub mod files;
pub mod git;
pub mod launch;
pub mod plugins;
pub mod runner;
pub mod sandbox;

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Notify;

/// Longest request line accepted, matching the Python worker's stream limit.
const REQUEST_LIMIT: u64 = 2 * 1024 * 1024;
/// A caller has this long to send its request line, as with the Python worker.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Where the worker listens, whom it serves and where it forwards.
#[derive(Debug, Clone)]
pub struct Config {
    /// The socket the API and loop connect to.
    pub socket: PathBuf,
    /// The Python worker's private socket.
    pub legacy_socket: PathBuf,
    /// The only caller UID served; other callers are disconnected unanswered.
    pub client_uid: u32,
    /// Where the native file tools work.
    pub files: files::Roots,
    /// The per-command supervisor: its executable and leading arguments.
    pub runner: Vec<String>,
    /// The per-plugin supervisor: its executable and leading arguments.
    pub plugin_runner: Vec<String>,
    /// Where published ability packages live.
    pub packages: plugins::Packages,
}

impl Config {
    /// Reads `V3_TOOL_SOCKET`, `V3_LEGACY_TOOL_SOCKET` and `V3_TOOL_CLIENT_UID`, defaulting
    /// to the container layout.
    ///
    /// # Errors
    /// When `V3_TOOL_CLIENT_UID` is not a number.
    pub fn from_env() -> io::Result<Self> {
        let path = |name: &str, default: &str| {
            PathBuf::from(std::env::var(name).unwrap_or_else(|_| default.into()))
        };
        let client_uid = std::env::var("V3_TOOL_CLIENT_UID")
            .unwrap_or_else(|_| "1100".into())
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid client UID"))?;
        Ok(Self {
            socket: path("V3_TOOL_SOCKET", "/runtime/worker/tools.sock"),
            legacy_socket: path("V3_LEGACY_TOOL_SOCKET", "/runtime/worker-legacy/tools.sock"),
            client_uid,
            files: files::Roots::container(),
            // The app executable's supervisor mode, dispatched before its async runtime.
            runner: vec!["/usr/local/bin/webagent-v3".into(), "--tool-runner".into()],
            plugin_runner: vec![
                "/usr/local/bin/webagent-v3".into(),
                "--plugin-runner".into(),
            ],
            packages: plugins::Packages::container(),
        })
    }
}

/// Serves the tool socket until the listener fails.
///
/// # Errors
/// When the socket cannot be replaced, bound or given its mode.
pub async fn serve(config: Config) -> io::Result<()> {
    match std::fs::remove_file(&config.socket) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let listener = UnixListener::bind(&config.socket)?;
    std::fs::set_permissions(&config.socket, std::fs::Permissions::from_mode(0o660))?;
    let active = Arc::new(Active::default());
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            // Running out of descriptors is temporary; keep serving.
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        if !stream
            .peer_cred()
            .is_ok_and(|peer| peer.uid() == config.client_uid)
        {
            continue;
        }
        let config = config.clone();
        let active = Arc::clone(&active);
        tokio::spawn(async move {
            // A failed write means the caller left; there is nobody to tell.
            let _ = handle(stream, &config, &active).await;
        });
    }
}

/// The commands running in each workspace, so a quiesce can stop them.
/// A running command's workspace and its stop signal.
type Running = (String, Arc<Notify>);

#[derive(Default)]
struct Active {
    /// The last call number handed out, and the running commands by number.
    calls: Mutex<(u64, HashMap<u64, Running>)>,
}

impl Active {
    fn start(&self, workspace: &str) -> (u64, Arc<Notify>) {
        let mut calls = self.calls.lock().unwrap_or_else(PoisonError::into_inner);
        calls.0 += 1;
        let (id, stop) = (calls.0, Arc::new(Notify::new()));
        calls
            .1
            .insert(id, (workspace.to_owned(), Arc::clone(&stop)));
        (id, stop)
    }

    fn end(&self, id: u64) {
        let mut calls = self.calls.lock().unwrap_or_else(PoisonError::into_inner);
        calls.1.remove(&id);
    }

    /// Stops every command in `workspace`; a stop sent before the command waits is kept.
    fn stop(&self, workspace: &str) {
        let calls = self.calls.lock().unwrap_or_else(PoisonError::into_inner);
        for (owner, stop) in calls.1.values() {
            if owner == workspace {
                stop.notify_one();
            }
        }
    }
}

/// Removes a command from [`Active`] however its call ends.
struct Registered<'a> {
    active: &'a Active,
    id: u64,
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        self.active.end(self.id);
    }
}

async fn write_reply(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    id: &Value,
    result: Value,
) -> io::Result<()> {
    let mut line = json!({"version":2,"id":id,"result":result})
        .to_string()
        .into_bytes();
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.shutdown().await
}

/// Answers one call: file tools and commands run here, anything else goes to the Python
/// worker. A caller that hangs up (or sends more) before a forwarded reply ends the
/// forwarded connection, which cancels the call exactly as a direct disconnect would; a
/// command is stopped the same way. A native file call always completes, so a write is
/// never left half done.
async fn handle(client: UnixStream, config: &Config, active: &Active) -> io::Result<()> {
    let (client_read, mut client_write) = client.into_split();
    let mut client_read = BufReader::new(client_read);
    let mut request = Vec::new();
    let problem = {
        let mut limited = (&mut client_read).take(REQUEST_LIMIT + 1);
        let read = limited.read_until(b'\n', &mut request);
        match tokio::time::timeout(REQUEST_TIMEOUT, read).await {
            Err(_) => Some(""),
            Ok(Err(_)) => Some("Request unreadable"),
            Ok(Ok(_)) if request.len() as u64 > REQUEST_LIMIT => Some("Request too large"),
            Ok(Ok(_)) => None,
        }
    };
    if let Some(problem) = problem {
        return reply(&mut client_write, None, problem).await;
    }
    let parsed = serde_json::from_slice::<Value>(&request)
        .ok()
        .filter(|request| request["version"] == 2);
    let name = parsed
        .as_ref()
        .and_then(|request| request["name"].as_str())
        .unwrap_or_default();
    if let Some(parsed) = parsed.as_ref().filter(|_| files::handles(name)) {
        let roots = config.files.clone();
        let request = parsed.clone();
        let result = tokio::task::spawn_blocking(move || files::call(&roots, &request))
            .await
            .unwrap_or_else(|_| json!({"status":"failed","output":"File tool failed"}));
        return write_reply(&mut client_write, &parsed["id"], result).await;
    }
    if let Some(parsed) = parsed.as_ref().filter(|_| git::handles(name)) {
        let workspace = parsed["workspace"].as_str().unwrap_or_default();
        let (id, stop) = active.start(workspace);
        let _registered = Registered { active, id };
        let result = tokio::select! {
            result = git_call(&config.files, parsed) => result,
            () = hang_up(&mut client_read) => return Ok(()),
            // As the Python worker answers a call its quiesce cancelled.
            () = stop.notified() => json!({"status":"failed","output":"Session not found"}),
        };
        return write_reply(&mut client_write, &parsed["id"], result).await;
    }
    if let Some(parsed) = parsed.as_ref().filter(|_| commands::handles(name)) {
        let workspace = parsed["workspace"].as_str().unwrap_or_default();
        let (id, stop) = active.start(workspace);
        let _registered = Registered { active, id };
        // Dropping the call (hang-up or quiesce) ends the command's session.
        let result = tokio::select! {
            result = commands::call(&config.files, &config.runner, parsed) => result,
            () = hang_up(&mut client_read) => return Ok(()),
            // As the Python worker answers a call its quiesce cancelled.
            () = stop.notified() => json!({"status":"failed","output":"Session not found"}),
        };
        return write_reply(&mut client_write, &parsed["id"], result).await;
    }
    if let Some(parsed) = parsed.as_ref().filter(|_| name == plugins::TOOL) {
        let workspace = parsed["workspace"].as_str().unwrap_or_default();
        let (id, stop) = active.start(workspace);
        let _registered = Registered { active, id };
        let result = tokio::select! {
            result = plugin_call(config, parsed) => result,
            () = hang_up(&mut client_read) => return Ok(()),
            () = stop.notified() => json!({"status":"failed","output":"Session not found"}),
        };
        return write_reply(&mut client_write, &parsed["id"], result).await;
    }
    if name == "administrator_quiesce" {
        // Commands run here now; stop them before the Python worker quiesces the rest.
        if let Some(workspace) = parsed.as_ref().and_then(|p| p["workspace"].as_str()) {
            active.stop(workspace);
        }
    }
    let upstream = match UnixStream::connect(&config.legacy_socket).await {
        Ok(upstream) => upstream,
        Err(_) => {
            let id = serde_json::from_slice::<Value>(&request)
                .ok()
                .and_then(|value| value.get("id").cloned());
            return reply(&mut client_write, id, "Tool worker unavailable").await;
        }
    };
    let (mut upstream_read, mut upstream_write) = upstream.into_split();
    upstream_write.write_all(&request).await?;
    tokio::select! {
        copied = tokio::io::copy(&mut upstream_read, &mut client_write) => {
            copied?;
            client_write.shutdown().await
        }
        () = hang_up(&mut client_read) => Ok(()),
    }
}

/// Admits and runs one Git Control request or repository watch.
async fn git_call(roots: &files::Roots, request: &Value) -> Value {
    let admitted = {
        let roots = roots.clone();
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            files::admit(&roots, &request)
                .map(|(base, _, _)| base)
                .map_err(|failure| files::failed(&failure))
        })
        .await
    };
    let base = match admitted {
        Ok(Ok(base)) => base,
        Ok(Err(failed)) => return failed,
        Err(_) => return json!({"status":"failed","output":"Git admission failed"}),
    };
    if request["name"] == "administrator_git_watch" {
        return git::watch(&base).await;
    }
    let name = request["name"].as_str().unwrap_or_default();
    let empty = json!({});
    let args = request.get("arguments").unwrap_or(&empty);
    git::execute(
        &base,
        name,
        args,
        request.get("trusted").unwrap_or(&Value::Null),
    )
    .await
}

/// Admits and runs one `ability_plugin_invoke` request.
async fn plugin_call(config: &Config, request: &Value) -> Value {
    let binding = &request["trusted"]["ability_plugin"];
    if !binding.is_object() {
        return json!({"status":"failed","output":"Trusted plugin binding required"});
    }
    let admitted = {
        let roots = config.files.clone();
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            files::admit(&roots, &request)
                .map(|(base, _, _)| base)
                .map_err(|failure| files::failed(&failure))
        })
        .await
    };
    let base = match admitted {
        Ok(Ok(base)) => base,
        Ok(Err(failed)) => return failed,
        Err(_) => return json!({"status":"failed","output":"Plugin admission failed"}),
    };
    let args = request
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    plugins::invoke(
        &config.packages,
        &config.plugin_runner,
        binding,
        &args,
        &base,
    )
    .await
    .unwrap_or_else(|refused| json!({"status":"failed","output":refused.to_string()}))
}

/// Completes when the caller closes its side or sends anything after its request.
async fn hang_up(reader: &mut (impl AsyncRead + Unpin)) {
    let mut byte = [0_u8; 1];
    let _ = reader.read(&mut byte).await;
}

/// Writes the worker's failure envelope for a call that never reached a tool.
async fn reply(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    id: Option<Value>,
    output: &str,
) -> io::Result<()> {
    let envelope = json!({"version":2,"id":id,"result":{"status":"failed","output":output}});
    let mut line = envelope.to_string().into_bytes();
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    struct Fixture {
        _dir: tempfile::TempDir,
        socket: PathBuf,
        legacy: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("temporary directory");
        let socket = dir.path().join("tools.sock");
        let legacy = dir.path().join("legacy.sock");
        let control = dir.path().join("control");
        std::fs::create_dir_all(&control).expect("control");
        std::fs::write(control.join("dispatch.lock"), "").expect("lock");
        std::fs::write(control.join("shutoff"), "0").expect("shutoff");
        std::fs::create_dir_all(dir.path().join("workspaces")).expect("workspaces");
        let config = Config {
            files: files::Roots {
                workspaces: dir.path().join("workspaces"),
                live_checkout: dir.path().join("absent-checkout"),
                control,
                context_config: dir.path().join("absent-context.json"),
                port_claims: dir.path().join("port-claims"),
            },
            runner: vec!["/nonexistent-runner".into()],
            plugin_runner: vec!["/nonexistent-runner".into()],
            packages: plugins::Packages::container(),
            socket: socket.clone(),
            legacy_socket: legacy.clone(),
            // The test process owns the directory it just created.
            client_uid: std::fs::metadata(dir.path()).expect("directory").uid(),
        };
        tokio::spawn(serve(config));
        Fixture {
            _dir: dir,
            socket,
            legacy,
        }
    }

    async fn connect(path: &Path) -> UnixStream {
        for _ in 0..100 {
            if let Ok(stream) = UnixStream::connect(path).await {
                return stream;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("worker socket never appeared");
    }

    async fn read_line(stream: &mut UnixStream) -> Value {
        let mut line = Vec::new();
        BufReader::new(stream)
            .read_until(b'\n', &mut line)
            .await
            .expect("reply");
        serde_json::from_slice(&line).expect("reply is JSON")
    }

    #[tokio::test]
    async fn a_call_reaches_the_python_worker_and_its_reply_comes_back() {
        let fixture = fixture();
        let legacy = UnixListener::bind(&fixture.legacy).expect("legacy socket");
        tokio::spawn(async move {
            let (stream, _) = legacy.accept().await.expect("forwarded call");
            let (read, mut write) = stream.into_split();
            let mut line = Vec::new();
            BufReader::new(read)
                .read_until(b'\n', &mut line)
                .await
                .expect("request");
            let request: Value = serde_json::from_slice(&line).expect("request is JSON");
            let reply = json!({"version":2,"id":request["id"],"result":{"status":"success","output":request["name"]}});
            write
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .expect("reply");
        });
        let mut client = connect(&fixture.socket).await;
        client
            .write_all(b"{\"version\":2,\"id\":\"a\",\"name\":\"terminal_read\"}\n")
            .await
            .expect("request");
        let reply = read_line(&mut client).await;
        assert_eq!(reply["id"], "a");
        assert_eq!(reply["result"]["output"], "terminal_read");
    }

    #[tokio::test]
    async fn a_file_tool_runs_in_the_worker_without_the_python_worker() {
        let fixture = fixture();
        let mut client = connect(&fixture.socket).await;
        let request = json!({"version":2,"id":"n","name":"file_list",
            "workspace":"2244991a-c2e3-415b-ae5c-054570c91d32","arguments":{"path":"."}});
        client
            .write_all(format!("{request}\n").as_bytes())
            .await
            .expect("request");
        let reply = read_line(&mut client).await;
        assert_eq!(reply["id"], "n");
        assert_eq!(reply["result"]["status"], "success", "{reply}");
        assert_eq!(reply["result"]["implementation_version"], "2.0.0");
    }

    #[tokio::test]
    async fn a_missing_python_worker_fails_the_call_with_its_id() {
        let fixture = fixture();
        let mut client = connect(&fixture.socket).await;
        client
            .write_all(b"{\"version\":2,\"id\":\"b\",\"name\":\"terminal_send\"}\n")
            .await
            .expect("request");
        let reply = read_line(&mut client).await;
        assert_eq!(reply["id"], "b");
        assert_eq!(reply["result"]["status"], "failed");
        assert_eq!(reply["result"]["output"], "Tool worker unavailable");
    }

    #[tokio::test]
    async fn a_caller_that_hangs_up_closes_the_forwarded_call() {
        let fixture = fixture();
        let legacy = UnixListener::bind(&fixture.legacy).expect("legacy socket");
        let closed = tokio::spawn(async move {
            let (stream, _) = legacy.accept().await.expect("forwarded call");
            let mut reader = BufReader::new(stream);
            let mut line = Vec::new();
            reader.read_until(b'\n', &mut line).await.expect("request");
            // Like the Python worker, wait for the caller to leave instead of replying.
            let mut rest = Vec::new();
            reader.read_to_end(&mut rest).await.expect("end of call");
            rest.is_empty()
        });
        let mut client = connect(&fixture.socket).await;
        client
            .write_all(b"{\"version\":2,\"id\":\"c\",\"name\":\"terminal_send\"}\n")
            .await
            .expect("request");
        drop(client);
        let closed = tokio::time::timeout(Duration::from_secs(5), closed)
            .await
            .expect("forwarded call closed")
            .expect("legacy task");
        assert!(closed);
    }
}
