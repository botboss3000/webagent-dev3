use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    process::{Child, Command},
};
static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
pub fn folder() -> PathBuf {
    PathBuf::from(
        std::env::var("V3_PLUGIN_TEST_ROOT").unwrap_or_else(|_| "/live-checkout/plugins".into()),
    )
    .join("diagnostics")
}
pub fn available() -> bool {
    ROOT.get().is_some_and(|root| {
        std::fs::read_to_string(root.join("diagnostics/plugin.ready"))
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .is_some_and(|t| crate::now().saturating_sub(t) <= 6)
    })
}
pub async fn send(
    operation: &str,
    event: Value,
    sealed: Option<(&[u8], usize)>,
) -> anyhow::Result<()> {
    let root = ROOT
        .get()
        .ok_or_else(|| anyhow::anyhow!("Plugin not initialized"))?;
    let mut frame = json!({"version":1,"operation":operation,"event":event});
    if let Some((sealed, bytes)) = sealed {
        frame["sealed"] = json!(hex::encode(sealed));
        frame["bytes"] = json!(bytes);
    }
    let bytes = serde_json::to_vec(&frame)?;
    if bytes.len() > 136 * 1024 * 1024 {
        anyhow::bail!("Plugin frame oversized")
    }
    tokio::time::timeout(Duration::from_secs(12), async {
        let mut stream = UnixStream::connect(root.join("diagnostics/plugin.sock")).await?;
        if stream.peer_cred()?.uid() != 1100 && std::env::var("V3_PLUGIN_TEST_ROOT").is_err() {
            anyhow::bail!("Plugin peer rejected")
        }
        stream.write_u32(bytes.len() as u32).await?;
        stream.write_all(&bytes).await?;
        if stream.read_u8().await? != 1 {
            anyhow::bail!("Plugin acknowledgement rejected")
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}
pub async fn configuration() -> anyhow::Result<(super::config::Config, String)> {
    let root = ROOT
        .get()
        .ok_or_else(|| anyhow::anyhow!("Plugin not initialized"))?;
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut stream = UnixStream::connect(root.join("diagnostics/plugin.sock")).await?;
        if stream.peer_cred()?.uid() != 1100 && std::env::var("V3_PLUGIN_TEST_ROOT").is_err() {
            anyhow::bail!("Plugin peer rejected")
        }
        let request = serde_json::to_vec(&json!({"version":1,"operation":"configuration"}))?;
        stream.write_u32(request.len() as u32).await?;
        stream.write_all(&request).await?;
        let size = stream.read_u32().await? as usize;
        if size > 65536 {
            anyhow::bail!("Configuration oversized")
        }
        let mut bytes = vec![0; size];
        stream.read_exact(&mut bytes).await?;
        let response: Value = serde_json::from_slice(&bytes)?;
        let config: super::config::Config = serde_json::from_value(response["config"].clone())?;
        config.validate()?;
        let digest = response["digest"]
            .as_str()
            .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| anyhow::anyhow!("Configuration digest rejected"))?
            .to_owned();
        Ok((config, digest))
    })
    .await?
}
fn package(root: &Path) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    if root.is_symlink() {
        return None;
    }
    let m = root.join("plugin.json");
    let b = root.join("webagent-diagnostics");
    let mi = std::fs::symlink_metadata(&m).ok()?;
    let bi = std::fs::symlink_metadata(&b).ok()?;
    if !mi.is_file()
        || mi.len() > 4096
        || !bi.is_file()
        || bi.len() > 32 * 1024 * 1024
        || bi.permissions().mode() & 0o022 != 0
    {
        return None;
    }
    let raw = std::fs::read(m).ok()?;
    let v: Value = serde_json::from_slice(&raw).ok()?;
    if v != json!({"version":1,"id":"diagnostics","executable":"webagent-diagnostics","protocol":1})
    {
        return None;
    }
    {
        use std::os::unix::fs::MetadataExt;
        Some(crate::digest(&json!([
            bi.dev(),
            bi.ino(),
            bi.len(),
            bi.mtime(),
            bi.mtime_nsec(),
            raw
        ])))
    }
}
pub async fn init(root: &Path, role: &str) {
    let _ = ROOT.set(root.to_path_buf());
    if role != "api" {
        return;
    }
    let root = root.to_path_buf();
    let plugins =
        std::env::var("V3_PLUGIN_TEST_ROOT").unwrap_or_else(|_| "/live-checkout/plugins".into());
    tokio::spawn(async move {
        let folder = PathBuf::from(plugins).join("diagnostics");
        let mut child: Option<Child> = None;
        let mut digest = None;
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        loop {
            interval.tick().await;
            let f = folder.clone();
            let wanted = tokio::task::spawn_blocking(move || package(&f))
                .await
                .ok()
                .flatten();
            let exited = child
                .as_mut()
                .is_some_and(|c| c.try_wait().ok().flatten().is_some());
            if wanted != digest || exited {
                if let Some(mut c) = child.take() {
                    let _ = c.kill().await;
                    let _ = c.wait().await;
                }
                let _ = tokio::fs::remove_file(root.join("diagnostics/plugin.ready")).await;
                let _ = tokio::fs::remove_file(root.join("diagnostics/plugin.sock")).await;
                digest = None;
                if let Some(wanted) = wanted {
                    let mut command = Command::new(folder.join("webagent-diagnostics"));
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
                    if let Ok(c) = command.spawn() {
                        child = Some(c);
                        digest = Some(wanted);
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manifests_and_executable_permissions_are_strict() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let b = root.path().join("webagent-diagnostics");
        std::fs::write(
            root.path().join("plugin.json"),
            include_bytes!("plugin.json"),
        )
        .unwrap();
        std::fs::write(&b, b"fixture").unwrap();
        std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(package(root.path()).is_some());
        std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o775)).unwrap();
        assert!(package(root.path()).is_none());
        std::fs::remove_file(&b).unwrap();
        std::os::unix::fs::symlink("/bin/true", &b).unwrap();
        assert!(package(root.path()).is_none());
    }
}
