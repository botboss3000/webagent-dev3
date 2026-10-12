//! Administrator-trusted live plugin. The broker retains workload authority.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
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
pub(crate) const FILES: [&str; 6] = [
    "plugin.json",
    "optimization.json",
    "optimization.skill.md",
    "webagent-optimization",
    "measure.py",
    "workload.py",
];
pub(crate) fn folder() -> PathBuf {
    PathBuf::from(
        std::env::var("V3_PLUGIN_TEST_ROOT").unwrap_or_else(|_| "/live-checkout/plugins".into()),
    )
    .join("optimization")
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    links: u64,
}
impl FileIdentity {
    fn from(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            device: meta.dev(),
            inode: meta.ino(),
            uid: meta.uid(),
            gid: meta.gid(),
            mode: meta.mode(),
            len: meta.len(),
            modified: (meta.mtime(), meta.mtime_nsec()),
            changed: (meta.ctime(), meta.ctime_nsec()),
            links: meta.nlink(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct PackageIdentity {
    path: PathBuf,
    directory: FileIdentity,
    assets: Vec<FileIdentity>,
}
type ValidatedPackage = std::sync::Arc<(String, Vec<u8>, String)>;
type PackageCache = std::sync::Mutex<Option<(PackageIdentity, ValidatedPackage)>>;
static PACKAGE_CACHE: PackageCache = std::sync::Mutex::new(None);

pub(crate) fn package(path: &Path) -> Option<ValidatedPackage> {
    package_cached(path, &PACKAGE_CACHE)
}

/// Reuse validated bytes only while every freshly pinned asset retains its
/// metadata identity. This is one bounded cache entry, never a time-based lease.
fn package_cached(path: &Path, cache: &PackageCache) -> Option<ValidatedPackage> {
    let result = package_cached_inner(path, cache);
    if result.is_none() {
        // Inner validation may fail before acquiring the cache mutex. Withdraw
        // retained executable bytes too; the inner guard has already dropped.
        if let Ok(mut cached) = cache.lock() {
            cached.take();
        }
    }
    result
}

fn package_cached_inner(path: &Path, cache: &PackageCache) -> Option<ValidatedPackage> {
    use std::{
        io::Read,
        os::{
            fd::AsRawFd,
            unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        },
    };
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(0x10000 | 0x20000)
        .open(path)
        .ok()?;
    let directory_meta = directory.metadata().ok()?;
    if !directory_meta.is_dir() || directory_meta.permissions().mode() & 0o022 != 0 {
        return None;
    }
    let pinned = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
    let mut files = Vec::with_capacity(FILES.len());
    let mut assets = Vec::with_capacity(FILES.len());
    for name in FILES {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(0x20000 | 0x800)
            .open(pinned.join(name))
            .ok()?;
        let meta = file.metadata().ok()?;
        let limit = if name == "webagent-optimization" {
            32 * 1024 * 1024
        } else {
            65536
        };
        if !meta.is_file()
            || meta.len() > limit
            || meta.permissions().mode() & 0o022 != 0
            || meta.uid() != directory_meta.uid()
            || name == "webagent-optimization" && meta.permissions().mode() & 0o111 == 0
        {
            return None;
        }
        assets.push(FileIdentity::from(&meta));
        files.push(file);
    }
    let identity = PackageIdentity {
        path: path.to_owned(),
        directory: FileIdentity::from(&directory_meta),
        assets,
    };
    let mut cached = cache.lock().ok()?;
    if let Some((previous, bytes)) = &*cached {
        if previous == &identity && identities_still_match(path, &directory, &files, &identity) {
            return Some(bytes.clone());
        }
    }
    // Release the old executable allocation before reading a changed package.
    *cached = None;
    let mut hash = Sha256::new();
    let mut executable = Vec::new();
    let mut guidance = String::new();
    for (index, (name, file)) in FILES.iter().zip(files.iter_mut()).enumerate() {
        let limit = if *name == "webagent-optimization" {
            32 * 1024 * 1024
        } else {
            65536
        };
        let mut bytes = Vec::new();
        (&mut *file).take(limit + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() as u64 > limit
            || FileIdentity::from(&file.metadata().ok()?) != identity.assets[index]
        {
            return None;
        }
        if *name == "plugin.json"
            && serde_json::from_slice::<Value>(&bytes).ok()?
                != json!({"version":1,"id":"optimization","executable":"webagent-optimization","protocol":1})
        {
            return None;
        }
        if *name == "optimization.json"
            && serde_json::from_slice::<Value>(&bytes).ok()?
                != serde_json::from_str::<Value>(include_str!(
                    "plugin_packages/optimization/optimization.json"
                ))
                .ok()?
        {
            return None;
        }
        if *name == "optimization.skill.md" {
            guidance = std::str::from_utf8(&bytes).ok()?.to_owned();
            if guidance.trim().is_empty() {
                return None;
            }
        }
        hash.update((name.len() as u64).to_be_bytes());
        hash.update(name.as_bytes());
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(&bytes);
        if *name == "webagent-optimization" {
            executable = bytes;
        }
    }
    if !identities_still_match(path, &directory, &files, &identity) {
        return None;
    }
    let validated = std::sync::Arc::new((hex::encode(hash.finalize()), executable, guidance));
    *cached = Some((identity, validated.clone()));
    Some(validated)
}

/// Detect both in-place writes and path replacement while all original file
/// descriptors stay pinned. Symlink replacement is rejected by metadata type.
fn identities_still_match(
    path: &Path,
    directory: &std::fs::File,
    files: &[std::fs::File],
    identity: &PackageIdentity,
) -> bool {
    use std::os::fd::AsRawFd;
    let pinned = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
    let directory_same = std::fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.is_dir() && FileIdentity::from(&meta) == identity.directory)
        && directory
            .metadata()
            .is_ok_and(|meta| FileIdentity::from(&meta) == identity.directory);
    directory_same
        && FILES
            .iter()
            .zip(files)
            .enumerate()
            .all(|(index, (name, file))| {
                file.metadata()
                    .is_ok_and(|meta| FileIdentity::from(&meta) == identity.assets[index])
                    && std::fs::symlink_metadata(pinned.join(name)).is_ok_and(|meta| {
                        meta.is_file() && FileIdentity::from(&meta) == identity.assets[index]
                    })
            })
}
pub(crate) fn generation() -> Option<String> {
    let root = ROOT.get()?;
    let marker: Value =
        serde_json::from_slice(&std::fs::read(root.join("optimization/plugin.ready")).ok()?)
            .ok()?;
    if crate::now().saturating_sub(marker["updated_at"].as_u64()?) > 6 {
        return None;
    }
    let package = package(&folder())?;
    (marker["generation"].as_str()? == package.0).then(|| package.0.clone())
}
pub(crate) fn available() -> bool {
    generation().is_some()
}
pub(crate) fn guidance() -> Option<String> {
    let before = generation()?;
    let package = package(&folder())?;
    (package.0 == before && generation()? == before).then(|| package.2.clone())
}
async fn ping(root: &Path, generation: &str) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(1), async {
        let mut s = UnixStream::connect(root.join("optimization/plugin.sock")).await?;
        if s.peer_cred()?.uid() != 1100 && std::env::var("V3_PLUGIN_TEST_ROOT").is_err() {
            anyhow::bail!("Plugin peer rejected")
        }
        let bytes =
            serde_json::to_vec(&json!({"version":1,"operation":"probe","generation":generation}))?;
        s.write_u32(bytes.len() as u32).await?;
        s.write_all(&bytes).await?;
        let length = s.read_u32().await?;
        if length > 65536 {
            anyhow::bail!("Oversized response")
        }
        let mut bytes = vec![0; length as usize];
        s.read_exact(&mut bytes).await?;
        let reply: Value = serde_json::from_slice(&bytes)?;
        if reply != json!({"version":1,"status":"ready","generation":generation}) {
            anyhow::bail!("Plugin protocol rejected")
        }
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
/// Atomically restage a private executable after restart. An old mode-0500
/// generation file may remain; replacing its directory entry needs no write
/// permission on that file and never follows an existing target symlink.
async fn stage_executable(state: &Path, digest: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let target = state.join(format!("executable-{digest}"));
    let temporary = state.join(format!(".executable-{digest}-{}.tmp", crate::uuid()));
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .await?;
    let result = async {
        file.write_all(bytes).await?;
        file.sync_all().await?;
        file.set_permissions(std::fs::Permissions::from_mode(0o500))
            .await?;
        drop(file);
        tokio::fs::rename(&temporary, &target).await?;
        Ok::<_, std::io::Error>(target)
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}

pub(crate) async fn init(root: &Path, role: &str) {
    let _ = ROOT.set(root.to_path_buf());
    if role != "api" {
        return;
    }
    let root = root.to_path_buf();
    tokio::spawn(async move {
        use std::os::unix::fs::PermissionsExt;
        let state = root.join("optimization");
        if tokio::fs::create_dir_all(&state).await.is_err() {
            return;
        }
        let _ = tokio::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).await;
        let mut child: Option<Child> = None;
        let mut current: Option<String> = None;
        let mut executable: Option<PathBuf> = None;
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        loop {
            interval.tick().await;
            let f = folder();
            let wanted = tokio::task::spawn_blocking(move || package(&f))
                .await
                .ok()
                .flatten();
            let exited = child
                .as_mut()
                .is_some_and(|c| c.try_wait().ok().flatten().is_some());
            if wanted.as_ref().map(|v| &v.0) != current.as_ref() || exited {
                let _ = tokio::fs::remove_file(state.join("plugin.ready")).await;
                if let Some(mut c) = child.take() {
                    let _ = c.kill().await;
                    let _ = c.wait().await;
                }
                if let Some(p) = executable.take() {
                    let _ = tokio::fs::remove_file(p).await;
                }
                let _ = tokio::fs::remove_file(state.join("plugin.sock")).await;
                current = None;
                if let Some(package) = wanted {
                    let digest = &package.0;
                    if let Ok(p) = stage_executable(&state, digest, &package.1).await {
                        let mut command = Command::new(&p);
                        command
                            .env_clear()
                            .env("V3_PLUGIN_STATE", &root)
                            .env("V3_PLUGIN_PACKAGE", folder())
                            .env("V3_PLUGIN_GENERATION", &digest)
                            .env("PATH", "/usr/bin:/bin")
                            .env("LANG", "C.UTF-8")
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .kill_on_drop(true);
                        if let Ok(c) = command.spawn() {
                            child = Some(c);
                            current = Some(digest.clone());
                            executable = Some(p);
                        }
                    }
                }
            }
            if let Some(g) = current.as_ref() {
                if ping(&root, g).await.is_ok() {
                    let bytes=serde_json::to_vec(&json!({"generation":g,"updated_at":crate::now(),"pid":child.as_ref().and_then(Child::id)})).unwrap();
                    let temp = state.join("plugin.ready.tmp");
                    if tokio::fs::write(&temp, bytes).await.is_ok() {
                        let _ = tokio::fs::rename(temp, state.join("plugin.ready")).await;
                    }
                } else {
                    let _ = tokio::fs::remove_file(state.join("plugin.ready")).await;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "external API/loop lifecycle acceptance helper"]
    async fn probe_loop_generation() {
        let root = PathBuf::from(std::env::var("V3_STATE_ROOT").expect("isolated state root"));
        init(&root, "loop").await;
        let expected = std::env::var("OPTIMIZATION_EXPECTED_AVAILABLE")
            .expect("expected availability")
            == "true";
        assert_eq!(available(), expected);
        let catalog = crate::abilities::Catalog::load(&PathBuf::from(
            std::env::var("V3_ABILITIES_ROOT")
                .unwrap_or_else(|_| format!("{}/plugins/abilities", env!("CARGO_MANIFEST_DIR"))),
        ))
        .unwrap();
        assert_eq!(catalog.known("optimization"), expected);
        let names = catalog.schemas_with(
            &crate::policy_default(),
            &json!([]),
            &json!({"visibility":{"optimization":"visible"}}),
            &json!({"active":[],"suppressed":[]}),
        );
        assert_eq!(
            names.iter().any(|s| s["name"] == "performance_work"),
            expected
        );
        assert_eq!(guidance().is_some(), expected);
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fixture() -> (tempfile::TempDir, PackageCache) {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        for name in FILES {
            let bytes: Vec<u8> = match name {
                "plugin.json" => br#"{"version":1,"id":"optimization","executable":"webagent-optimization","protocol":1}"#.to_vec(),
                "optimization.json" => include_bytes!("plugin_packages/optimization/optimization.json").to_vec(),
                "optimization.skill.md" => b"Measured resource experiments only.\n".to_vec(),
                "webagent-optimization" => vec![b'x'; 19 * 1024 * 1024],
                _ => b"# fixed driver\n".to_vec(),
            };
            let path = root.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(
                path,
                std::fs::Permissions::from_mode(if name == "webagent-optimization" {
                    0o755
                } else {
                    0o644
                }),
            )
            .unwrap();
        }
        (root, std::sync::Mutex::new(None))
    }

    #[test]
    fn unchanged_large_executable_reuses_validated_allocation() {
        let (root, cache) = fixture();
        let start = std::time::Instant::now();
        let first = package_cached(root.path(), &cache).unwrap();
        let validation = start.elapsed();
        let start = std::time::Instant::now();
        for _ in 0..1000 {
            assert!(std::sync::Arc::ptr_eq(
                &first,
                &package_cached(root.path(), &cache).unwrap()
            ));
        }
        eprintln!("19MiB package initial validation={validation:?}; 1000 metadata-validated cache hits={:?}", start.elapsed());
        assert_eq!(first.1.len(), 19 * 1024 * 1024);
    }

    #[test]
    fn every_asset_content_change_invalidates_with_preserved_mtime() {
        let (root, cache) = fixture();
        for name in FILES {
            let before = package_cached(root.path(), &cache).unwrap();
            let path = root.path().join(name);
            let modified = path.metadata().unwrap().modified().unwrap();
            let mut bytes = std::fs::read(&path).unwrap();
            // Manifest whitespace retains compatibility while changing the exact digest.
            bytes.extend_from_slice(b" ");
            std::fs::write(&path, &bytes).unwrap();
            std::fs::File::open(&path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(modified))
                .unwrap();
            let after = package_cached(root.path(), &cache).unwrap();
            assert!(!std::sync::Arc::ptr_eq(&before, &after), "{name}");
            assert_ne!(before.0, after.0, "{name}");
        }
        // Same-size write with restored mtime specifically exercises ctime identity.
        let before = package_cached(root.path(), &cache).unwrap();
        let path = root.path().join("measure.py");
        let meta = path.metadata().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[0] = b'!';
        std::fs::write(&path, bytes).unwrap();
        std::fs::File::open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(meta.modified().unwrap()))
            .unwrap();
        let after = package_cached(root.path(), &cache).unwrap();
        assert!(!std::sync::Arc::ptr_eq(&before, &after));
        assert_ne!(before.0, after.0);
    }

    #[test]
    fn metadata_replacement_permissions_and_symlinks_withdraw_cached_authority() {
        let (root, cache) = fixture();
        let original = package_cached(root.path(), &cache).unwrap();
        let path = root.path().join("measure.py");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let changed = package_cached(root.path(), &cache).unwrap();
        assert!(!std::sync::Arc::ptr_eq(&original, &changed));
        assert_eq!(original.0, changed.0);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(package_cached(root.path(), &cache).is_none());
        assert!(
            cache.lock().unwrap().is_none(),
            "unsafe assets must release the cached executable"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let restored = package_cached(root.path(), &cache).unwrap();
        let replacement = root.path().join("replacement");
        std::fs::write(&replacement, std::fs::read(&path).unwrap()).unwrap();
        std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let replaced = package_cached(root.path(), &cache).unwrap();
        assert!(!std::sync::Arc::ptr_eq(&restored, &replaced));
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(root.path().join("workload.py"), &path).unwrap();
        assert!(package_cached(root.path(), &cache).is_none());
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(package_cached(root.path(), &cache).is_none());
    }

    #[test]
    fn removed_package_releases_the_cached_executable() {
        let (root, cache) = fixture();
        let path = root.path().to_owned();
        assert!(package_cached(&path, &cache).is_some());
        assert!(cache.lock().unwrap().is_some());
        root.close().unwrap();
        assert!(package_cached(&path, &cache).is_none());
        assert!(cache.lock().unwrap().is_none());
    }

    #[test]
    fn fifo_asset_is_rejected_without_waiting_for_a_writer() {
        let (root, cache) = fixture();
        let path = root.path().join("measure.py");
        std::fs::remove_file(&path).unwrap();
        assert!(std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success());
        assert!(package_cached(root.path(), &cache).is_none());
    }

    #[test]
    fn one_cached_path_cannot_supply_another_package_or_invalid_manifest() {
        let (first, cache) = fixture();
        let (second, _) = fixture();
        let a = package_cached(first.path(), &cache).unwrap();
        let b = package_cached(second.path(), &cache).unwrap();
        assert!(!std::sync::Arc::ptr_eq(&a, &b));
        std::fs::write(second.path().join("plugin.json"), b"{}").unwrap();
        assert!(package_cached(second.path(), &cache).is_none());
        std::fs::write(
            second.path().join("plugin.json"),
            std::fs::read(first.path().join("plugin.json")).unwrap(),
        )
        .unwrap();
        assert!(package_cached(second.path(), &cache).is_some());
        std::fs::write(second.path().join("optimization.json"), b"{}").unwrap();
        assert!(package_cached(second.path(), &cache).is_none());
    }
}

#[cfg(test)]
mod staging_tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    #[tokio::test]
    async fn restart_replaces_existing_readonly_generation_with_new_inode() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("executable-generation");
        std::fs::write(&target, b"old executable").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o500)).unwrap();
        let old = target.metadata().unwrap();
        let staged = stage_executable(root.path(), "generation", b"new executable")
            .await
            .unwrap();
        let metadata = staged.metadata().unwrap();
        assert_eq!(std::fs::read(&staged).unwrap(), b"new executable");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o500);
        assert_ne!((old.dev(), old.ino()), (metadata.dev(), metadata.ino()));
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn staging_replaces_target_symlink_without_touching_its_destination() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), b"private existing content").unwrap();
        let target = root.path().join("executable-generation");
        std::os::unix::fs::symlink(outside.path(), &target).unwrap();
        stage_executable(root.path(), "generation", b"plugin executable")
            .await
            .unwrap();
        assert!(std::fs::symlink_metadata(&target).unwrap().is_file());
        assert_eq!(std::fs::read(target).unwrap(), b"plugin executable");
        assert_eq!(
            std::fs::read(outside.path()).unwrap(),
            b"private existing content"
        );
    }

    #[tokio::test]
    async fn failed_atomic_publish_removes_its_temporary_file() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("executable-generation")).unwrap();
        assert!(
            stage_executable(root.path(), "generation", b"plugin executable")
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
