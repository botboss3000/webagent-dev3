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
/// The folder of the plugin's database (`logs.db` and its WAL files). Since stream D5 it is the
/// plugin's own folder in the app's state (beside its socket), not the package folder in the
/// checkout, which the API mounts read-only; an isolated test root keeps both in one folder.
pub fn data_folder() -> PathBuf {
    if std::env::var_os("V3_PLUGIN_TEST_ROOT").is_some() {
        return folder();
    }
    ROOT.get()
        .cloned()
        .unwrap_or_else(webagent_configuration::paths::state)
        .join("diagnostics")
}

/// The plugin's database while its package is installed: removing the package still removes
/// archive access (the database is kept, and comes back with the package).
pub fn database() -> Option<PathBuf> {
    let path = data_folder().join("logs.db");
    (folder().is_dir() && path.is_file()).then_some(path)
}

/// Stream D5, once: copies the database the plugin kept in its package folder into
/// `data` when `data` has none. The plugin is not running then (its parent API was just
/// replaced), so the database and its WAL file are a consistent pair. Both are copied under
/// temporary names first; a failed copy leaves neither behind and is tried again at the next
/// start, and the WAL file is put in place before the database so a `logs.db` never appears
/// without its WAL.
fn adopt_package_database(package: &Path, data: &Path) -> std::io::Result<()> {
    use webagent_platform::fs::PermissionsExt;
    let regular = |path: &Path| std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file());
    if package == data
        || data.join("logs.db").exists()
        || !regular(package.join("logs.db").as_path())
    {
        return Ok(());
    }
    std::fs::create_dir_all(data)?;
    std::fs::set_permissions(data, std::fs::Permissions::from_mode(0o700))?;
    let names: Vec<&str> = ["logs.db-wal", "logs.db"]
        .into_iter()
        .filter(|name| regular(package.join(name).as_path()))
        .collect();
    let temporary = |name: &str| data.join(format!("{name}.adopting"));
    let copied = names.iter().copied().try_for_each(|name| {
        std::fs::copy(package.join(name), temporary(name))?;
        std::fs::set_permissions(temporary(name), std::fs::Permissions::from_mode(0o600))
    });
    let placed = copied.and_then(|()| {
        names
            .iter()
            .copied()
            .try_for_each(|name| std::fs::rename(temporary(name), data.join(name)))
    });
    if placed.is_err() {
        for name in names.iter().copied() {
            let _ = std::fs::remove_file(temporary(name));
        }
        if !data.join("logs.db").exists() {
            let _ = std::fs::remove_file(data.join("logs.db-wal"));
        }
    }
    placed
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
        let data = data_folder();
        {
            let (package, data) = (folder.clone(), data.clone());
            let _ =
                tokio::task::spawn_blocking(move || adopt_package_database(&package, &data)).await;
        }
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
                        // The plugin keeps its database in this folder (`V3_PLUGIN_PACKAGE`
                        // is the one folder it opens `logs.db` in).
                        .env("V3_PLUGIN_PACKAGE", &data)
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
    fn package_database_is_copied_once_and_never_through_a_link() {
        let root = tempfile::tempdir().unwrap();
        let (package, data) = (
            root.path().join("package"),
            root.path().join("state/diagnostics"),
        );
        std::fs::create_dir_all(&package).unwrap();
        // Nothing to adopt: no folder is made.
        adopt_package_database(&package, &data).unwrap();
        assert!(!data.exists());
        std::fs::write(package.join("logs.db"), b"db").unwrap();
        std::fs::write(package.join("logs.db-wal"), b"wal").unwrap();
        adopt_package_database(&package, &data).unwrap();
        assert_eq!(std::fs::read(data.join("logs.db")).unwrap(), b"db");
        assert_eq!(std::fs::read(data.join("logs.db-wal")).unwrap(), b"wal");
        assert!(!data.join("logs.db.adopting").exists());
        // A database already in the data folder is never replaced.
        std::fs::write(package.join("logs.db"), b"newer").unwrap();
        adopt_package_database(&package, &data).unwrap();
        assert_eq!(std::fs::read(data.join("logs.db")).unwrap(), b"db");
        // A linked package database is not followed.
        let (linked, other) = (root.path().join("linked"), root.path().join("other"));
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(root.path().join("secret"), b"secret").unwrap();
        webagent_platform::fs::symlink(root.path().join("secret"), linked.join("logs.db")).unwrap();
        adopt_package_database(&linked, &other).unwrap();
        assert!(!other.join("logs.db").exists());
        // The same folder (isolated test root) is left alone.
        adopt_package_database(&package, &package).unwrap();
        assert_eq!(std::fs::read(package.join("logs.db")).unwrap(), b"newer");
    }
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
