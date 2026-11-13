//! The browser container's service, `webagent-v3 --browser-host [SOCKET]`. The worker in the
//! app container connects here instead of starting browsers itself. Each connection begins
//! with one JSON header line: `{"op":"session"}` starts one chat's `--browser-session`
//! process and relays its frames until either side closes, and the other operations answer
//! once for the private folders the worker cannot reach (screenshots, sizes, deletion).
//! Only the worker's UID may connect; the host's own UID may only ping, which is the
//! container health check `--browser-host ping`.
use super::pool::{folder_bytes, folder_of, sweep_folders};
use super::service::{read_artifact, Limits};
use super::Settings;
use serde_json::{json, Value};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// Where the worker finds the browser host.
#[must_use]
pub fn default_socket() -> PathBuf {
    webagent_configuration::paths::runtime().join("browser/host.sock")
}
/// Largest header line and reply (a screenshot in base64).
pub const MAX_REPLY: usize = 32 * 1024 * 1024;
/// How long a session process may take to close its browsers after the worker leaves.
const CLOSE_GRACE: Duration = Duration::from_secs(5);

/// Serves `spec` (the socket, [`default_socket`] when empty) or, for `ping`, checks that a
/// running host answers; returns the exit code.
#[must_use]
pub fn run(spec: &str) -> i32 {
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    else {
        return 1;
    };
    if spec == "ping" {
        let socket = default_socket();
        return i32::from(!runtime.block_on(ping(&socket)));
    }
    let socket = if spec.is_empty() {
        default_socket()
    } else {
        PathBuf::from(spec)
    };
    let client = std::env::var("V3_BROWSER_CLIENT_UID")
        .ok()
        .and_then(|uid| uid.parse().ok())
        .unwrap_or(1103);
    runtime.block_on(serve(&socket, Settings::from_env(), client))
}

async fn ping(socket: &Path) -> bool {
    let check = async {
        let mut stream = UnixStream::connect(socket).await.ok()?;
        stream.write_all(b"{\"op\":\"ping\"}\n").await.ok()?;
        let mut reply = Vec::new();
        BufReader::new(stream)
            .take(64)
            .read_until(b'\n', &mut reply)
            .await
            .ok()?;
        (serde_json::from_slice::<Value>(&reply).ok()?["ok"] == true).then_some(())
    };
    tokio::time::timeout(Duration::from_secs(5), check)
        .await
        .is_ok_and(|ok| ok.is_some())
}

/// Accepts connections from `client` (the worker's UID) on `socket` until the process ends.
pub async fn serve(socket: &Path, settings: Settings, client: u32) -> i32 {
    let _ = std::fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(&settings.root);
    let _ = std::fs::remove_file(socket);
    let Ok(listener) = UnixListener::bind(socket) else {
        return 1;
    };
    // The folder's group lets the worker reach the socket; the peer check admits only it.
    if std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o660)).is_err() {
        return 1;
    }
    let settings = Arc::new(settings);
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let settings = Arc::clone(&settings);
        tokio::spawn(async move {
            let _ = connection(stream, &settings, client).await;
        });
    }
}

async fn connection(stream: UnixStream, settings: &Settings, client: u32) -> std::io::Result<()> {
    let peer = stream.peer_cred()?.uid();
    // SAFETY: getuid takes no arguments and cannot fail.
    let own = peer == unsafe { libc::getuid() };
    if peer != client && !own {
        return Ok(());
    }
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    let mut line = Vec::new();
    (&mut read)
        .take(MAX_REPLY as u64 + 1)
        .read_until(b'\n', &mut line)
        .await?;
    let header: Value = serde_json::from_slice(&line).unwrap_or(Value::Null);
    // The container's own health check may only ping.
    if peer != client && header["op"] != "ping" {
        return Ok(());
    }
    if header["op"] == "session" {
        return session(read, write, settings).await;
    }
    let root = settings.root.clone();
    let reply = tokio::task::spawn_blocking(move || folders(&root, &header))
        .await
        .unwrap_or_else(|_| json!({"error":true}));
    let mut data = reply.to_string().into_bytes();
    data.push(b'\n');
    write.write_all(&data).await
}

/// One answer about the private folders under `root`.
fn folders(root: &Path, header: &Value) -> Value {
    let sid = header["sid"].as_str().unwrap_or_default();
    let strings = |value: &Value| -> Vec<String> {
        value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    match header["op"].as_str().unwrap_or_default() {
        "ping" => json!({"ok":true}),
        "usage" => json!({"bytes": folder_of(root, sid).map_or(0, |folder| folder_bytes(&folder))}),
        "purge" => {
            if let Some(folder) = folder_of(root, sid) {
                let _ = std::fs::remove_dir_all(folder);
            }
            json!({"ok":true})
        }
        "sweep" => {
            let (removed, usage) = sweep_folders(
                root,
                &strings(&header["keep"]),
                &strings(&header["running"]),
            );
            json!({"removed":removed,"usage":usage})
        }
        "artifact" => {
            let base = Path::new(header["base"].as_str().unwrap_or_default());
            match read_artifact(
                root,
                &Limits::default(),
                base,
                &header["trusted"],
                &header["artifact_id"],
            ) {
                Ok(content) => json!({"png_base64":super::base64::encode(&content)}),
                Err(_) => json!({"error":true}),
            }
        }
        _ => json!({"error":true}),
    }
}

/// Starts one `--browser-session` process in its own group and relays frames both ways;
/// when either side ends, gives the process a moment to close its browsers, then ends the
/// whole group.
async fn session(
    read: impl AsyncRead + Unpin,
    mut write: impl AsyncWrite + Unpin,
    settings: &Settings,
) -> std::io::Result<()> {
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    command
        .arg("--browser-session")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("V3_BROWSER_ROOT", &settings.root)
        .env("V3_PUBLIC_ORIGIN", &settings.origin)
        .env(
            "V3_BROWSER_SANDBOX",
            if settings.sandbox { "1" } else { "0" },
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(false);
    // SAFETY: setsid is async-signal-safe; Chromium's processes join this group.
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
        .unwrap_or(0);
    let (Some(mut stdin), Some(mut stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(std::io::Error::other("browser process pipes"));
    };
    let mut read = read;
    tokio::select! {
        _ = tokio::io::copy(&mut read, &mut stdin) => {}
        _ = tokio::io::copy(&mut stdout, &mut write) => {}
    }
    drop(stdin);
    let _ = write.shutdown().await;
    if tokio::time::timeout(CLOSE_GRACE, child.wait())
        .await
        .is_err()
        && pid > 0
    {
        // SAFETY: killpg takes plain integers; the unreaped leader keeps the group id ours.
        unsafe {
            libc::killpg(pid, libc::SIGTERM);
        }
        if tokio::time::timeout(CLOSE_GRACE, child.wait())
            .await
            .is_err()
        {
            // SAFETY: as above.
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
            }
            let _ = child.wait().await;
        }
    }
    if pid > 0 {
        // SAFETY: as above; ESRCH once everything has gone is expected.
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn ask(socket: &Path, request: &str) -> Value {
        let mut stream = UnixStream::connect(socket).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        stream.write_all(b"\n").await.unwrap();
        let mut reply = Vec::new();
        BufReader::new(stream)
            .read_until(b'\n', &mut reply)
            .await
            .unwrap();
        serde_json::from_slice(&reply).unwrap_or(Value::Null)
    }

    #[tokio::test]
    async fn folder_requests_answer_only_the_client() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("browser");
        let sid = "11111111-1111-4111-8111-111111111111";
        std::fs::create_dir_all(root.join(sid).join("artifacts")).unwrap();
        std::fs::write(root.join(sid).join("artifacts/a.png"), vec![0; 7]).unwrap();
        let settings = Settings {
            root: root.clone(),
            origin: "https://example.test".into(),
            host: None,
            sandbox: false,
        };
        let me = std::fs::metadata(dir.path())
            .map(|m| std::os::unix::fs::MetadataExt::uid(&m))
            .unwrap();
        let socket = dir.path().join("host.sock");
        tokio::spawn({
            let socket = socket.clone();
            let settings = settings.clone();
            async move { serve(&socket, settings, me).await }
        });
        for _ in 0..100 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(ping(&socket).await);
        let usage = format!(r#"{{"op":"usage","sid":"{sid}"}}"#);
        assert_eq!(ask(&socket, &usage).await["bytes"], 7);
        let sweep = format!(r#"{{"op":"sweep","keep":["{sid}"],"running":[]}}"#);
        assert_eq!(ask(&socket, &sweep).await["usage"][sid], 7);
        assert_eq!(
            ask(&socket, r#"{"op":"purge","sid":"../x"}"#).await["ok"],
            true
        );
        assert!(root.join(sid).exists());
        let purge = format!(r#"{{"op":"purge","sid":"{sid}"}}"#);
        assert_eq!(ask(&socket, &purge).await["ok"], true);
        assert!(!root.join(sid).exists());
        // A host serving another client UID.
        let stranger = tokio::spawn({
            let socket = dir.path().join("other.sock");
            async move { serve(&socket, settings, me + 1).await }
        });
        for _ in 0..100 {
            if dir.path().join("other.sock").exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let other = dir.path().join("other.sock");
        // This process's own UID may still ping (the container health check) but not ask.
        assert!(ping(&other).await);
        let usage = format!(r#"{{"op":"usage","sid":"{sid}"}}"#);
        assert_eq!(ask(&other, &usage).await, Value::Null);
        stranger.abort();
    }
}
