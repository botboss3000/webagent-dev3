//! Bounded IPC for the administrator-installed, independently replaceable MCP package.
use anyhow::{bail, Context};
use serde_json::{json, Value};
use std::{
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    process::{Child, Command},
};
use tokio_util::sync::CancellationToken;

static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
const FRAME_LIMIT: usize = 1024 * 1024;
const BINARY_LIMIT: u64 = 32 * 1024 * 1024;

pub(crate) fn folder() -> PathBuf {
    PathBuf::from(
        std::env::var("V3_PLUGIN_TEST_ROOT").unwrap_or_else(|_| "/live-checkout/plugins".into()),
    )
    .join("mcp")
}

// Check every path component, including the installation parent: resolving a
// symlink would silently transfer executable authority to a different directory.
fn no_symlinks(path: &Path) -> bool {
    let mut current = PathBuf::new();
    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return false;
        }
        current.push(component);
        if std::fs::symlink_metadata(&current)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(true)
        {
            return false;
        }
    }
    true
}

fn package(root: &Path) -> Option<String> {
    if !no_symlinks(root) {
        return None;
    }
    let directory = std::fs::symlink_metadata(root).ok()?;
    let manifest = std::fs::symlink_metadata(root.join("plugin.json")).ok()?;
    let binary = std::fs::symlink_metadata(root.join("webagent-mcp")).ok()?;
    if !directory.is_dir()
        || directory.permissions().mode() & 0o022 != 0
        || !manifest.is_file()
        || manifest.len() > 4096
        || manifest.permissions().mode() & 0o022 != 0
        || !binary.is_file()
        || binary.len() == 0
        || binary.len() > BINARY_LIMIT
        || binary.permissions().mode() & 0o022 != 0
        || binary.permissions().mode() & 0o111 == 0
        || manifest.uid() != directory.uid()
        || binary.uid() != directory.uid()
    {
        return None;
    }
    let raw = std::fs::read(root.join("plugin.json")).ok()?;
    let value: Value = serde_json::from_slice(&raw).ok()?;
    if value != json!({"version":1,"id":"mcp","executable":"webagent-mcp","protocol":1}) {
        return None;
    }
    Some(crate::digest(&json!([
        directory.dev(),
        directory.ino(),
        directory.uid(),
        directory.mode(),
        manifest.dev(),
        manifest.ino(),
        manifest.uid(),
        manifest.mode(),
        binary.dev(),
        binary.ino(),
        binary.uid(),
        binary.mode(),
        binary.len(),
        binary.mtime(),
        binary.mtime_nsec(),
        binary.ctime(),
        binary.ctime_nsec(),
        raw
    ])))
}

fn fresh(root: &Path, digest: &str) -> bool {
    let state = root.join("mcp");
    std::fs::read_to_string(state.join("plugin.package"))
        .ok()
        .as_deref()
        == Some(digest)
        && std::fs::read_to_string(state.join("plugin.ready"))
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .is_some_and(|time| {
                let now = crate::now();
                time <= now && now - time <= 6
            })
}

pub(crate) fn available() -> bool {
    ROOT.get()
        .is_some_and(|root| package(&folder()).is_some_and(|digest| fresh(root, &digest)))
}

fn response(bytes: &[u8]) -> anyhow::Result<Value> {
    let value: Value = serde_json::from_slice(bytes).context("Invalid MCP plugin response")?;
    if value.get("version").and_then(Value::as_u64) != Some(1) {
        bail!("Unsupported MCP plugin response")
    }
    match value.get("status").and_then(Value::as_str) {
        Some("success") => {
            if value.get("output").is_none() {
                bail!("Missing MCP plugin output")
            }
            Ok(value)
        }
        // Preserve known versus uncertain effects while suppressing diagnostics
        // that may contain credentials or untrusted server payloads.
        Some("error") => Ok(json!({
            "version": 1, "status": "error", "output": "MCP operation failed",
            "uncertain": value["output"]["uncertain"].as_bool() == Some(true)
        })),
        _ => bail!("Invalid MCP plugin response status"),
    }
}

async fn exchange(mut stream: UnixStream, bytes: &[u8]) -> anyhow::Result<Value> {
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(bytes).await?;
    let size = stream.read_u32().await? as usize;
    if size == 0 || size > FRAME_LIMIT {
        bail!("MCP response oversized")
    }
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).await?;
    response(&bytes)
}

pub(super) async fn call(mut request: Value, cancel: CancellationToken) -> anyhow::Result<Value> {
    let root = ROOT
        .get()
        .ok_or_else(|| anyhow::anyhow!("MCP plugin not initialized"))?;
    let digest = package(&folder()).ok_or_else(|| anyhow::anyhow!("MCP package unavailable"))?;
    if !fresh(root, &digest) {
        bail!("MCP plugin unavailable")
    }
    let object = request
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Invalid MCP request"))?;
    object.insert("version".into(), json!(1));
    let bytes = serde_json::to_vec(&request)?;
    if bytes.len() > FRAME_LIMIT {
        bail!("MCP request oversized")
    }
    let execution = async {
        let stream = UnixStream::connect(root.join("mcp/plugin.sock"))
            .await
            .context("MCP plugin connection failed")?;
        let expected_uid = if std::env::var_os("V3_PLUGIN_TEST_ROOT").is_some() {
            // Test overrides still enforce an identity, never bypass peer checks.
            std::fs::metadata("/proc/self")
                .context("MCP peer identity unavailable")?
                .uid()
        } else {
            1100
        };
        if stream.peer_cred()?.uid() != expected_uid {
            bail!("MCP plugin peer rejected")
        }
        exchange(stream, &bytes).await
    };
    // Dropping execution closes the socket. A cancelled consequential external
    // call remains uncertain; core recovery must never infer that it did not run.
    let output = tokio::select! {
        biased;
        _ = cancel.cancelled() => bail!("MCP operation cancelled"),
        result = tokio::time::timeout(Duration::from_secs(35), execution) =>
            result.context("MCP operation timed out")??,
    };
    if package(&folder()).as_deref() != Some(&digest) || !fresh(root, &digest) {
        bail!("MCP package changed during operation")
    }
    Ok(output)
}

pub(crate) async fn init(root: &Path, role: &str) {
    let _ = ROOT.set(root.to_path_buf());
    if role != "api" {
        return;
    }
    let root = root.to_path_buf();
    tokio::spawn(futures_util::FutureExt::boxed(async move {
        let folder = folder();
        let mut child: Option<Child> = None;
        let mut digest: Option<String> = None;
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        loop {
            interval.tick().await;
            let candidate = folder.clone();
            let wanted = tokio::task::spawn_blocking(move || package(&candidate))
                .await
                .ok()
                .flatten();
            let exited = child
                .as_mut()
                .is_some_and(|child| !matches!(child.try_wait(), Ok(None)));
            if wanted == digest && !exited {
                continue;
            }
            if let Some(mut child) = child.take() {
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
            for entry in ["plugin.ready", "plugin.sock", "plugin.package"] {
                let _ = tokio::fs::remove_file(root.join("mcp").join(entry)).await;
            }
            digest = None;
            if let Some(wanted) = wanted {
                // Only create runtime state. Removal of the live package stays
                // removed until an administrator installs another package.
                if tokio::fs::create_dir_all(root.join("mcp")).await.is_err()
                    || tokio::fs::set_permissions(
                        root.join("mcp"),
                        std::fs::Permissions::from_mode(0o700),
                    )
                    .await
                    .is_err()
                {
                    continue;
                }
                let mut command = Command::new(folder.join("webagent-mcp"));
                command
                    .env_clear()
                    .env("V3_PLUGIN_STATE", &root)
                    .env("V3_PLUGIN_PACKAGE", &folder)
                    .env("PATH", "/usr/bin:/bin")
                    .env("LANG", "C.UTF-8")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .kill_on_drop(true);
                if package(&folder).as_deref() != Some(&wanted) {
                    continue;
                }
                if let Ok(mut started) = command.spawn() {
                    if package(&folder).as_deref() != Some(&wanted)
                        || tokio::fs::write(root.join("mcp/plugin.package"), &wanted)
                            .await
                            .is_err()
                    {
                        let _ = started.kill().await;
                        let _ = started.wait().await;
                    } else {
                        child = Some(started);
                        digest = Some(wanted);
                    }
                }
            }
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("plugin.json"),
            serde_json::to_vec(
                &json!({"version":1,"id":"mcp","executable":"webagent-mcp","protocol":1}),
            )
            .unwrap(),
        )
        .unwrap();
        let binary = root.path().join("webagent-mcp");
        std::fs::write(&binary, b"fixture").unwrap();
        std::fs::set_permissions(binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        root
    }
    #[test]
    fn package_rejects_insecure_permissions_and_symlinks() {
        let root = fixture();
        assert!(package(root.path()).is_some());
        for name in ["plugin.json", "webagent-mcp"] {
            let path = root.path().join(name);
            let original = std::fs::metadata(&path).unwrap().permissions();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
            assert!(package(root.path()).is_none());
            std::fs::set_permissions(&path, original).unwrap();
        }
        let outer = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(root.path(), outer.path().join("linked")).unwrap();
        assert!(package(&outer.path().join("linked")).is_none());
        std::fs::remove_file(root.path().join("plugin.json")).unwrap();
        std::os::unix::fs::symlink("/dev/null", root.path().join("plugin.json")).unwrap();
        assert!(package(root.path()).is_none());
    }
    #[test]
    fn package_rejects_nonexecutable_oversized_and_unknown_manifest() {
        let root = fixture();
        let binary = root.path().join("webagent-mcp");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(package(root.path()).is_none());
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&binary)
            .unwrap()
            .set_len(BINARY_LIMIT + 1)
            .unwrap();
        assert!(package(root.path()).is_none());
        std::fs::write(&binary, b"fixture").unwrap();
        std::fs::write(root.path().join("plugin.json"), b"{}").unwrap();
        assert!(package(root.path()).is_none());
    }
    #[test]
    fn package_replacement_and_removal_invalidate_freshness() {
        let root = fixture();
        let digest = package(root.path()).unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::create_dir(state.path().join("mcp")).unwrap();
        std::fs::write(state.path().join("mcp/plugin.package"), &digest).unwrap();
        std::fs::write(
            state.path().join("mcp/plugin.ready"),
            crate::now().to_string(),
        )
        .unwrap();
        assert!(fresh(state.path(), &digest));
        let replacement = root.path().join("new");
        std::fs::write(&replacement, b"replacement").unwrap();
        std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(replacement, root.path().join("webagent-mcp")).unwrap();
        assert_ne!(package(root.path()).as_deref(), Some(digest.as_str()));
        std::fs::remove_file(root.path().join("webagent-mcp")).unwrap();
        assert!(package(root.path()).is_none());
        std::fs::write(
            state.path().join("mcp/plugin.ready"),
            crate::now().saturating_sub(7).to_string(),
        )
        .unwrap();
        assert!(!fresh(state.path(), &digest));
    }
    #[tokio::test]
    async fn ipc_frames_are_length_prefixed_and_bounded() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let peer = tokio::spawn(async move {
            let size = server.read_u32().await.unwrap() as usize;
            let mut frame = vec![0; size];
            server.read_exact(&mut frame).await.unwrap();
            assert_eq!(frame, b"{\"version\":1}");
            let result = br#"{"version":1,"status":"success","output":42}"#;
            server.write_u32(result.len() as u32).await.unwrap();
            server.write_all(result).await.unwrap();
        });
        assert_eq!(
            exchange(client, br#"{"version":1}"#).await.unwrap(),
            json!({"version":1,"status":"success","output":42})
        );
        peer.await.unwrap();
        let (client, mut server) = UnixStream::pair().unwrap();
        let peer = tokio::spawn(async move {
            let size = server.read_u32().await.unwrap() as usize;
            let mut frame = vec![0; size];
            server.read_exact(&mut frame).await.unwrap();
            server.write_u32((FRAME_LIMIT + 1) as u32).await.unwrap();
        });
        assert!(exchange(client, b"{}")
            .await
            .unwrap_err()
            .to_string()
            .contains("oversized"));
        peer.await.unwrap();
    }
    #[tokio::test]
    async fn dropped_execution_closes_socket() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let pending = tokio::spawn(async move { exchange(client, b"{}").await });
        let size = server.read_u32().await.unwrap() as usize;
        let mut frame = vec![0; size];
        server.read_exact(&mut frame).await.unwrap();
        pending.abort();
        let _ = pending.await;
        let mut byte = [0];
        assert_eq!(server.read(&mut byte).await.unwrap(), 0);
    }
    #[test]
    fn response_envelope_is_strict_and_errors_are_private() {
        assert_eq!(
            response(br#"{"version":1,"status":"success","output":{"ok":true}}"#).unwrap(),
            json!({"version":1,"status":"success","output":{"ok":true}})
        );
        for invalid in [
            br#"{"version":2,"status":"success","output":null}"#.as_slice(),
            br#"{"version":1,"status":"success"}"#.as_slice(),
            br#"{"version":1,"status":"unknown","output":null}"#.as_slice(),
        ] {
            assert!(response(invalid).is_err());
        }
        let error = response(br#"{"version":1,"status":"error","output":"SECRET"}"#).unwrap();
        assert!(!error.to_string().contains("SECRET"));
        assert_eq!(error["uncertain"], json!(false));
        let uncertain = response(
            br#"{"version":1,"status":"error","output":{"uncertain":true,"reason":"SECRET"}}"#,
        )
        .unwrap();
        assert_eq!(uncertain["uncertain"], json!(true));
        assert!(!uncertain.to_string().contains("SECRET"));
    }
}
