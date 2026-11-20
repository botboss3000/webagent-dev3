use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
};
static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
pub fn folder() -> PathBuf {
    std::env::var_os("V3_PLUGIN_TEST_ROOT")
        .map_or_else(
            || webagent_configuration::paths::checkout().join("plugins"),
            PathBuf::from,
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
        let mut stream = connect(root).await?;
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
/// The plugin's socket, checked to belong to the app user. The plugin process and its
/// peer-checked socket are Unix-only; elsewhere `init` never starts it.
#[cfg(unix)]
async fn connect(root: &Path) -> anyhow::Result<UnixStream> {
    let stream = UnixStream::connect(root.join("diagnostics/plugin.sock")).await?;
    if stream.peer_cred()?.uid() != 1100 && std::env::var("V3_PLUGIN_TEST_ROOT").is_err() {
        anyhow::bail!("Plugin peer rejected")
    }
    Ok(stream)
}

#[cfg(not(unix))]
async fn connect(_root: &Path) -> anyhow::Result<tokio::io::DuplexStream> {
    anyhow::bail!("Diagnostics plugin is not supported on this platform")
}

pub async fn configuration() -> anyhow::Result<(super::config::Config, String)> {
    let root = ROOT
        .get()
        .ok_or_else(|| anyhow::anyhow!("Plugin not initialized"))?;
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut stream = connect(root).await?;
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
    use webagent_platform::fs::PermissionsExt;
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
        use webagent_platform::fs::MetadataExt;
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
    if role != "api" || cfg!(not(unix)) {
        return;
    }
    let root = root.to_path_buf();
    let plugins = std::env::var("V3_PLUGIN_TEST_ROOT").unwrap_or_else(|_| {
        webagent_configuration::paths::checkout()
            .join("plugins")
            .to_string_lossy()
            .into_owned()
    });
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
        use webagent_platform::fs::PermissionsExt;
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
        webagent_platform::fs::symlink("/bin/true", &b).unwrap();
        assert!(package(root.path()).is_none());
    }
}
