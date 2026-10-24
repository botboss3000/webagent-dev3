//! Bounded metadata cache for a validated Automation package, never an authority cache.
use super::*;
use std::{collections::VecDeque, os::unix::fs::MetadataExt, sync::{Arc, Mutex, OnceLock}};

#[derive(Clone, PartialEq, Eq)]
struct Identity {
    path: PathBuf,
    resolved: PathBuf,
    metadata: [u64; 10],
}
impl Identity {
    fn read(path: &FsPath) -> Result<Self> {
        let m = std::fs::symlink_metadata(path)?;
        if m.file_type().is_symlink() || !(m.is_file() || m.is_dir()) {
            anyhow::bail!("Unsafe Automation cache input");
        }
        Ok(Self { path: path.to_owned(), resolved: path.canonicalize()?, metadata: [m.dev(), m.ino(), m.len(), m.mtime() as u64, m.mtime_nsec() as u64, m.ctime() as u64, m.ctime_nsec() as u64, m.mode() as u64, m.uid() as u64, m.gid() as u64] })
    }
    fn current(&self) -> bool { Self::read(&self.path).is_ok_and(|v| v == *self) }
}
struct Entry {
    root: PathBuf,
    artifacts: PathBuf,
    identities: Vec<Identity>,
    service: Arc<AutomationService>,
}
#[derive(Default)]
struct Cache(VecDeque<Entry>);
impl Cache {
    fn get(&mut self, root: &FsPath, artifacts: &FsPath) -> Result<Arc<AutomationService>> {
        if let Some(index) = self.0.iter().position(|e| e.root == root && e.artifacts == artifacts) {
            let entry = self.0.remove(index).unwrap();
            if entry.identities.iter().all(Identity::current) {
                let result = entry.service.clone();
                self.0.push_back(entry);
                return Ok(result);
            }
            // Invalidation removes the old value before loading; errors never restore stale code.
        }
        let manifest: Value = serde_json::from_slice(&read(&root.join("plugin.json"), 64 * 1024)?)?;
        let mut paths = std::collections::BTreeSet::from([root.to_owned(), artifacts.to_owned(), root.join("plugin.json"), root.join("automation.json")]);
        for kind in ["inputs", "assets"] {
            let names = manifest["build"][kind].as_array().ok_or_else(|| anyhow!("Missing Automation inputs"))?;
            if names.len() > 1024 { anyhow::bail!("Automation input bound"); }
            for name in names {
                let path = relative(root, name)?;
                let mut parent = path.parent();
                while let Some(p) = parent.filter(|p| p.starts_with(root) && *p != root) {
                    paths.insert(p.to_owned()); parent = p.parent();
                }
                paths.insert(path);
            }
        }
        let key = build_key(root, &manifest, false)?;
        paths.insert(artifacts.join(&key));
        paths.insert(artifacts.join(&key).join("webagent_automation"));
        let identities: Vec<_> = paths.iter().map(|p| Identity::read(p)).collect::<Result<_>>()?;
        let service = Arc::new(super::load_automation_service_at(root, artifacts)?);
        if service.executable != artifacts.join(key).join("webagent_automation") || serde_json::from_slice::<Value>(&read(&root.join("plugin.json"), 64 * 1024)?)? != manifest || !identities.iter().all(Identity::current) {
            anyhow::bail!("Automation inputs changed during validation");
        }
        if self.0.len() == 16 { self.0.pop_front(); }
        self.0.push_back(Entry { root: root.to_owned(), artifacts: artifacts.to_owned(), identities, service: service.clone() });
        Ok(service)
    }
}
pub(super) fn get(root: &FsPath, artifacts: &FsPath) -> Result<AutomationService> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Mutex::default).lock().unwrap_or_else(|e| e.into_inner());
    Ok(cache.get(root, artifacts)?.as_ref().clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(feature = "test-support")]
    fn cache_reuses_validation_and_invalidates_every_declared_asset() {
        use std::os::unix::fs::PermissionsExt;
        let (root, manifest, _) = super::super::registered_tests::native_automation_contract_tests::fixture_data();
        let artifacts = tempfile::tempdir().unwrap();
        let key = build_key(root.path(), &manifest, false).unwrap();
        let dir = artifacts.path().join(key);std::fs::create_dir(&dir).unwrap();
        let executable = dir.join("webagent_automation");
        std::fs::write(&executable, b"synthetic artifact").unwrap();
        std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o555)).unwrap();
        let mut cache = Cache::default();
        let first = cache.get(root.path(),artifacts.path()).unwrap();
        assert!(Arc::ptr_eq(&first,&cache.get(root.path(),artifacts.path()).unwrap()));
        std::fs::write(root.path().join("service/page.js"),b"updated asset").unwrap();
        let next = cache.get(root.path(),artifacts.path()).unwrap();
        assert!(!Arc::ptr_eq(&first,&next));assert_ne!(first.implementation,next.implementation);
        std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o775)).unwrap();
        assert!(cache.get(root.path(),artifacts.path()).is_err());assert!(cache.0.is_empty());
        std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o555)).unwrap();
        cache.get(root.path(),artifacts.path()).unwrap();
        let input=root.path().join("service/engine.rs");std::fs::remove_file(&input).unwrap();
        std::os::unix::fs::symlink("/etc/passwd",&input).unwrap();
        assert!(cache.get(root.path(),artifacts.path()).is_err());
    }
    #[test]
    fn metadata_detects_replacement_permissions_and_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap(); let p = root.path().join("input");
        std::fs::write(&p, b"same").unwrap(); let original = Identity::read(&p).unwrap();
        assert!(original.current());
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        let private = Identity::read(&p).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o666)).unwrap(); assert!(!private.current());
        std::fs::remove_file(&p).unwrap(); std::fs::write(&p, b"same").unwrap(); assert!(!original.current());
        std::fs::remove_file(&p).unwrap(); std::os::unix::fs::symlink("/etc/passwd", &p).unwrap(); assert!(Identity::read(&p).is_err());
    }
}
