//! The v3 tool worker. It owns the tool socket the API and loop call and accepts only the
//! application identity. File tools run here ([`files`]); every other request is forwarded
//! unchanged to the Python worker on a private socket. Tool groups move from the Python
//! worker into this crate one at a time (Rust conversion item A2).
pub mod files;

use serde_json::{json, Value};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

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
            socket: path("V3_TOOL_SOCKET", "/run/v3/worker/tools.sock"),
            legacy_socket: path("V3_LEGACY_TOOL_SOCKET", "/run/v3/worker-legacy/tools.sock"),
            client_uid,
            files: files::Roots::container(),
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
        tokio::spawn(async move {
            // A failed write means the caller left; there is nobody to tell.
            let _ = handle(stream, &config).await;
        });
    }
}

/// Answers one call: a file tool runs here, anything else goes to the Python worker. A
/// caller that hangs up (or sends more) before a forwarded reply ends the forwarded
/// connection, which cancels the call exactly as a direct disconnect would. A native file
/// call always completes, so a write is never left half done.
async fn handle(client: UnixStream, config: &Config) -> io::Result<()> {
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
    let parsed = serde_json::from_slice::<Value>(&request).ok();
    if let Some(parsed) = parsed.filter(|request| {
        request["version"] == 2 && request["name"].as_str().is_some_and(files::handles)
    }) {
        let roots = config.files.clone();
        let id = parsed["id"].clone();
        let result = tokio::task::spawn_blocking(move || files::call(&roots, &parsed))
            .await
            .unwrap_or_else(|_| json!({"status":"failed","output":"File tool failed"}));
        let mut line = json!({"version":2,"id":id,"result":result})
            .to_string()
            .into_bytes();
        line.push(b'\n');
        client_write.write_all(&line).await?;
        return client_write.shutdown().await;
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
            },
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
            .write_all(b"{\"version\":2,\"id\":\"b\",\"name\":\"shell\"}\n")
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
            .write_all(b"{\"version\":2,\"id\":\"c\",\"name\":\"shell\"}\n")
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
