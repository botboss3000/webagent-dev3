//! Live files: configuration listed in the checkout's `live-files.json`, edited in
//! place (by hand or through the broker's `live_file_write`) and read on every use.
//! The checkout wins; the published release and this workspace are fallbacks, so a
//! container started before promotion, a test or a local run still finds the file.
//! A file that stops validating keeps serving its last good bytes.
use anyhow::{anyhow, bail, Result};
use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

/// Mounted checkout in the API and loop containers.
pub const CHECKOUT: &str = "/live-checkout";

static LAST_GOOD: Mutex<Option<HashMap<String, Vec<u8>>>> = Mutex::new(None);

/// Replaces a live file if it still holds `expected`; installed once by the app process.
type Writer = dyn Fn(&str, &[u8], &[u8]) -> Result<()> + Send + Sync;
static WRITER: OnceLock<Box<Writer>> = OnceLock::new();

/// Install this process's live-file writer (the app saves through the broker).
pub fn install_writer(writer: impl Fn(&str, &[u8], &[u8]) -> Result<()> + Send + Sync + 'static) {
    // A second install is ignored: the first process-wide writer stays authoritative.
    let _ = WRITER.set(Box::new(writer));
}

/// Whether this process saves live files (false in tests, which use fixture files).
pub fn writable() -> bool {
    WRITER.get().is_some()
}

/// Replace `relative` with `body` only while it still holds `expected` (the bytes read).
pub fn write(relative: &str, body: &[u8], expected: &[u8]) -> Result<()> {
    checked(relative)?;
    let writer = WRITER
        .get()
        .ok_or_else(|| anyhow!("Live files are read-only in this process"))?;
    writer(relative, body, expected)
}

/// The last-good cache; a poisoned lock is cleared (the cache only ever loses entries).
fn last_good() -> std::sync::MutexGuard<'static, Option<HashMap<String, Vec<u8>>>> {
    LAST_GOOD.lock().unwrap_or_else(|poisoned| {
        LAST_GOOD.clear_poison();
        let mut guard = poisoned.into_inner();
        *guard = None;
        guard
    })
}

/// Candidate roots, most live first.
pub fn roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if Path::new(CHECKOUT).is_dir() {
        roots.push(PathBuf::from(CHECKOUT));
    }
    if let Ok(Some(published)) = crate::prompts::default_source(None) {
        roots.push(published);
    }
    roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    roots
}

fn checked(relative: &str) -> Result<()> {
    if relative.is_empty()
        || relative
            .split('/')
            .any(|part| matches!(part, "" | "." | ".."))
    {
        bail!("Invalid live file path");
    }
    Ok(())
}

fn read_bounded(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    let info = match std::fs::symlink_metadata(path) {
        Ok(info) => info,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !info.is_file() || info.len() > limit {
        bail!("Invalid live file {}", path.display());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        bail!("Live file exceeds limit");
    }
    Ok(Some(bytes))
}

/// Read and validate `relative` from the given roots in order.
pub fn read_from(
    roots: &[PathBuf],
    relative: &str,
    limit: u64,
    validate: impl Fn(&[u8]) -> Result<()>,
) -> Result<Vec<u8>> {
    checked(relative)?;
    // Keyed by the root set too, so separate fixtures never share a cached file.
    let key = format!("{roots:?}\0{relative}");
    let mut first_error = None;
    for root in roots {
        let attempt = read_bounded(&root.join(relative), limit).and_then(|bytes| {
            bytes
                .map(|bytes| validate(&bytes).map(|_| bytes))
                .transpose()
        });
        match attempt {
            Ok(None) => continue,
            Ok(Some(bytes)) => {
                last_good()
                    .get_or_insert_with(HashMap::new)
                    .insert(key.clone(), bytes.clone());
                return Ok(bytes);
            }
            Err(error) => {
                if let Some(bytes) = last_good().as_ref().and_then(|cache| cache.get(&key)) {
                    eprintln!(
                        "Live file {relative} is invalid; serving its last good version: {error:#}"
                    );
                    return Ok(bytes.clone());
                }
                first_error.get_or_insert(error);
            }
        }
    }
    Err(first_error.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Live file {relative} not found"),
        )
        .into()
    }))
}

/// Read and validate a live file from the default roots.
pub fn read(relative: &str, limit: u64, validate: impl Fn(&[u8]) -> Result<()>) -> Result<Vec<u8>> {
    read_from(&roots(), relative, limit, validate)
}

/// Read a live JSON object, validated by `validate`.
pub fn json(
    relative: &str,
    limit: u64,
    validate: impl Fn(&serde_json::Value) -> Result<()>,
) -> Result<serde_json::Value> {
    let bytes = read(relative, limit, |bytes| {
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        if !value.is_object() {
            bail!("Live JSON file must be an object");
        }
        validate(&value)
    })?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Bytes of a live file when `path` is checkout-relative (validated, with last-good
/// fallback), otherwise that exact file read as is (test fixtures keep their behavior).
pub fn read_path(
    path: &Path,
    limit: u64,
    validate: impl Fn(&[u8]) -> Result<()>,
) -> Result<Vec<u8>> {
    if path.is_relative() {
        return read(
            path.to_str()
                .ok_or_else(|| anyhow!("Invalid live file path"))?,
            limit,
            validate,
        );
    }
    Ok(read_bounded(path, limit)?.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "Configuration file not found")
    })?)
}

/// True when `error` means the file does not exist anywhere.
pub fn missing(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

/// Read a live UTF-8 text file such as a Markdown prompt.
pub fn text(relative: &str, limit: u64) -> Result<String> {
    let bytes = read(relative, limit, |bytes| {
        std::str::from_utf8(bytes)
            .map(|_| ())
            .map_err(|_| anyhow!("Live text file is not UTF-8"))
    })?;
    Ok(String::from_utf8(bytes)?)
}

/// `(name, is_folder)` entries of `relative` from the first of `roots` that has that folder.
pub fn entries_from(roots: &[PathBuf], relative: &str) -> Result<Vec<(String, bool)>> {
    checked(relative)?;
    for root in roots {
        let folder = root.join(relative);
        if folder.is_dir() && !folder.is_symlink() {
            // A folder this process cannot list yet (permissions not exposed) falls through.
            let Ok(items) = std::fs::read_dir(folder) else {
                continue;
            };
            let mut names = Vec::new();
            for item in items {
                let item = item?;
                let kind = item.file_type()?;
                if let Some(name) = item
                    .file_name()
                    .to_str()
                    .filter(|name| !name.starts_with('.'))
                {
                    if kind.is_file() || kind.is_dir() {
                        names.push((name.to_owned(), kind.is_dir()));
                    }
                }
            }
            names.sort();
            return Ok(names);
        }
    }
    Ok(Vec::new())
}

/// File names in `relative` from the first default root that has that folder.
pub fn list(relative: &str) -> Result<Vec<String>> {
    Ok(entries_from(&roots(), relative)?
        .into_iter()
        .filter(|(_, folder)| !folder)
        .map(|(name, _)| name)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checkout_wins_then_fallback_then_last_good() {
        let (live, published) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let roots = [live.path().to_owned(), published.path().to_owned()];
        let valid = |bytes: &[u8]| {
            serde_json::from_slice::<serde_json::Value>(bytes)
                .map(|_| ())
                .map_err(Into::into)
        };
        std::fs::write(published.path().join("a.json"), b"{\"v\":0}").unwrap();
        assert_eq!(
            read_from(&roots, "a.json", 64, valid).unwrap(),
            b"{\"v\":0}"
        );
        std::fs::write(live.path().join("a.json"), b"{\"v\":1}").unwrap();
        assert_eq!(
            read_from(&roots, "a.json", 64, valid).unwrap(),
            b"{\"v\":1}"
        );
        // A broken edit keeps the last good bytes rather than taking the app down.
        std::fs::write(live.path().join("a.json"), b"{broken").unwrap();
        assert_eq!(
            read_from(&roots, "a.json", 64, valid).unwrap(),
            b"{\"v\":1}"
        );
        // With nothing cached, an invalid checkout copy falls back to the release.
        std::fs::write(live.path().join("b.json"), b"{broken").unwrap();
        std::fs::write(published.path().join("b.json"), b"{}").unwrap();
        assert_eq!(read_from(&roots, "b.json", 64, valid).unwrap(), b"{}");
        let missing = read_from(&roots, "c.json", 64, valid).unwrap_err();
        assert_eq!(
            missing.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::NotFound
        );
        assert!(read_from(&roots, "../a.json", 64, valid).is_err());
        std::fs::write(live.path().join("d.json"), vec![b' '; 65]).unwrap();
        assert!(read_from(&roots[..1], "d.json", 64, |_| Ok(())).is_err());
    }
}
