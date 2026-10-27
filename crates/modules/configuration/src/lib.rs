//! Bounded JSON configuration reads and cooperating, revision-fenced publication.
use anyhow::{anyhow, bail, Result};
use fs2::FileExt;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{fs::{File, OpenOptions}, io::Write, path::Path};

/// Read an object from a regular, non-symlink file no larger than four MiB.
pub fn read(path: &Path) -> Result<Value> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 4 * 1024 * 1024 {
        bail!("Invalid configuration file");
    }
    let value: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    if !value.is_object() { bail!("Configuration must be an object"); }
    Ok(value)
}

/// Hash the existing JSON representation without allocating a serialized copy.
pub fn digest<T: serde::Serialize + ?Sized>(value: &T) -> String {
    struct HashWriter(Sha256);
    impl Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, value).expect("JSON digest serialization");
    hex::encode(writer.0.finalize())
}

fn read_or_default(path: &Path, missing: Option<&Value>) -> Result<Value> {
    match read(path) {
        Ok(value) => Ok(value),
        Err(error) if error.downcast_ref::<std::io::Error>().is_some_and(|e|e.kind()==std::io::ErrorKind::NotFound) => {
            missing.cloned().ok_or(error)
        }
        Err(error) => Err(error),
    }
}

/// Hold the owner's cross-process lock through read, validated update and atomic replacement.
/// A stale digest returns None without invoking the update. Only callers with an absent-file
/// default may create a missing file; domain validation remains in the caller's update.
pub fn update_if_current(
    path: &Path, lock_path: &Path, expected_digest: &str, missing: Option<&Value>,
    update: impl FnOnce(&Value) -> Result<Value>,
) -> Result<Option<Value>> {
    let lock = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(lock_path)?;
    lock.lock_exclusive()?;
    let old = read_or_default(path, missing)?;
    if digest(&old) != expected_digest { return Ok(None); }
    let value = update(&old)?;
    if !value.is_object() { bail!("Configuration must be an object"); }
    let parent = path.parent().ok_or_else(||anyhow!("Missing config directory"))?;
    let mut output = tempfile::NamedTempFile::new_in(parent)?;
    match std::fs::metadata(path) {
        Ok(metadata) => output.as_file().set_permissions(metadata.permissions())?,
        Err(error) if missing.is_some() && error.kind()==std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(error.into()),
    }
    serde_json::to_writer_pretty(&mut output, &value)?;
    output.write_all(b"\n")?;
    output.as_file().sync_all()?;
    if read_or_default(path, missing)? != old { return Ok(None); }
    output.persist(path).map_err(anyhow::Error::from)?;
    File::open(parent)?.sync_all()?;
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::{fs::{symlink, PermissionsExt}};

    #[test]
    fn stale_and_invalid_updates_preserve_saved_bytes() {
        let root=tempfile::tempdir().unwrap();let path=root.path().join("settings.json");let lock=root.path().join("settings.lock");
        let original=b"{\"extension\":true,\"value\":1}\n";std::fs::write(&path,original).unwrap();
        assert!(update_if_current(&path,&lock,"stale",None,|_|panic!("stale update invoked")).unwrap().is_none());
        assert!(update_if_current(&path,&lock,&digest(&read(&path).unwrap()),None,|_|bail!("Invalid policy")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(),original);
    }

    #[test]
    fn publication_preserves_extensions_permissions_and_json_format() {
        let root=tempfile::tempdir().unwrap();let path=root.path().join("settings.json");let lock=root.path().join("settings.lock");
        std::fs::write(&path,b"{\"extension\":{\"retained\":true},\"value\":1}").unwrap();
        std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o640)).unwrap();
        let old=read(&path).unwrap();
        let saved=update_if_current(&path,&lock,&digest(&old),None,|old|{let mut next=old.clone();next["value"]=json!(2);Ok(next)}).unwrap().unwrap();
        assert_eq!(saved["extension"],old["extension"]);assert_eq!(read(&path).unwrap(),saved);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode()&0o777,0o640);
        let mut expected=serde_json::to_vec_pretty(&saved).unwrap();expected.push(b'\n');assert_eq!(std::fs::read(&path).unwrap(),expected);
    }

    #[test]
    fn read_rejects_nonobjects_symlinks_directories_and_oversize_files() {
        let root=tempfile::tempdir().unwrap();let path=root.path().join("settings.json");
        for bytes in [b"[]".as_slice(),b"null",b"invalid"] {std::fs::write(&path,bytes).unwrap();assert!(read(&path).is_err());}
        std::fs::write(&path,b"{}").unwrap();let link=root.path().join("link");symlink(&path,&link).unwrap();assert!(read(&link).is_err());
        assert!(read(root.path()).is_err());
        File::create(&path).unwrap().set_len(4*1024*1024+1).unwrap();assert!(read(&path).is_err());
    }

    #[test]
    fn absent_file_requires_explicit_default() {
        let root=tempfile::tempdir().unwrap();let path=root.path().join("settings.json");let lock=root.path().join("settings.lock");let empty=json!({});
        assert!(update_if_current(&path,&lock,&digest(&empty),None,|_|Ok(json!({"enabled":true}))).is_err());
        assert!(!path.exists());
        let saved=update_if_current(&path,&lock,&digest(&empty),Some(&empty),|_|Ok(json!({"enabled":true}))).unwrap().unwrap();
        assert_eq!(read(&path).unwrap(),saved);
    }

    #[test]
    fn cooperating_writers_with_same_revision_cannot_both_publish() {
        use std::sync::{Arc,Barrier};
        let root=tempfile::tempdir().unwrap();let path=root.path().join("settings.json");let lock=root.path().join("settings.lock");
        std::fs::write(&path,b"{\"value\":0}").unwrap();let expected=digest(&read(&path).unwrap());let barrier=Arc::new(Barrier::new(2));
        let handles:Vec<_>=(1..=2).map(|n|{let (path,lock,expected,barrier)=(path.clone(),lock.clone(),expected.clone(),barrier.clone());
            std::thread::spawn(move||{barrier.wait();update_if_current(&path,&lock,&expected,None,|_|Ok(json!({"value":n}))).unwrap().is_some()})
        }).collect();
        assert_eq!(handles.into_iter().map(|h|usize::from(h.join().unwrap())).sum::<usize>(),1);
        assert!([json!(1),json!(2)].contains(&read(&path).unwrap()["value"]));
    }
}

pub mod live;
pub mod prompts;
