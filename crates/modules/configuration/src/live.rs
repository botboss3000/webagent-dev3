//! Live files: configuration listed in the checkout's `live-files.json`, edited in
//! place (by hand or through the broker's `live_file_write`) and checked on every use.
//! The checkout wins; the published release is the fallback, so a container started
//! before promotion still finds the file (tests and local runs, which have no checkout,
//! use this workspace instead). A file that stops validating keeps serving its last
//! good version. One `stat` per use tells whether a file changed: unchanged files are
//! not read again, and `parsed` values are not parsed again. Folder listings are
//! kept the same way: a folder's own version changes whenever an entry is added,
//! removed or renamed. A version modified within the last two seconds is never reused,
//! since a filesystem with coarse timestamps can give two quick changes the same one.
use anyhow::{anyhow, bail, Result};
use std::{
    any::{Any, TypeId},
    collections::{BTreeMap, HashMap},
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

/// Mounted checkout in the API and loop containers.
pub const CHECKOUT: &str = "/live-checkout";

/// A file's version from one `stat`. The kernel sets the change time on every write and
/// no tool can set it back, and a save that replaces the file changes its inode, so a
/// same-size edit with a copied modification time is still seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

/// The bytes last served for a key, where they came from and their version.
struct Seen {
    path: PathBuf,
    stamp: Stamp,
    bytes: Vec<u8>,
}

static LAST_GOOD: Mutex<Option<HashMap<String, Seen>>> = Mutex::new(None);

/// A `parsed` file: its last good value and the version that last failed to parse.
#[derive(Default)]
struct Parsed {
    good: Option<(PathBuf, Stamp, Box<dyn Any + Send + Sync>)>,
    bad: Option<(PathBuf, Stamp)>,
}

/// Keyed by roots and file, value type and parser.
type ParsedKey = (String, TypeId, usize);
static PARSED: Mutex<BTreeMap<ParsedKey, Parsed>> = Mutex::new(BTreeMap::new());

/// `(name, is_folder)` entries last listed for a folder, with that folder's version.
type Listing = (Stamp, Vec<(String, bool)>);
static LISTED: Mutex<BTreeMap<PathBuf, Listing>> = Mutex::new(BTreeMap::new());

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

/// A cache lock; a poisoned one is cleared (the caches only ever lose entries).
fn cache<T: Default>(lock: &'static Mutex<T>) -> MutexGuard<'static, T> {
    lock.lock().unwrap_or_else(|poisoned| {
        lock.clear_poison();
        let mut guard = poisoned.into_inner();
        *guard = T::default();
        guard
    })
}

/// Candidate roots, most live first: the checkout, then the published release; this
/// workspace only where there is no checkout (tests and local runs).
pub fn roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let checkout = Path::new(CHECKOUT).is_dir();
    if checkout {
        roots.push(PathBuf::from(CHECKOUT));
    }
    if let Ok(Some(published)) = crate::prompts::default_source(None) {
        roots.push(published);
    }
    if !checkout {
        roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    }
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

/// The version of `path`, or `None` when it does not exist.
fn stamp(path: &Path, limit: u64) -> Result<Option<Stamp>> {
    let info = match std::fs::symlink_metadata(path) {
        Ok(info) => info,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !info.is_file() || info.len() > limit {
        bail!("Invalid live file {}", path.display());
    }
    Ok(Some(version(&info)))
}

/// Whether `stamp` is old enough to reuse: two changes within one coarse clock tick can
/// share a version, so a recent one is read again. The modification time is used (not
/// the change time) so tests can settle fixtures by backdating it.
fn settled(stamp: &Stamp) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64);
    stamp.modified.0 < now - 1
}

fn version(info: &std::fs::Metadata) -> Stamp {
    Stamp {
        device: info.dev(),
        inode: info.ino(),
        len: info.len(),
        modified: (info.mtime(), info.mtime_nsec()),
        changed: (info.ctime(), info.ctime_nsec()),
    }
}

/// The version and bytes of `path` (stamped before reading, so an edit racing the read
/// is seen as a change next time), or `None` when it does not exist.
fn read_bounded(path: &Path, limit: u64) -> Result<Option<(Stamp, Vec<u8>)>> {
    let Some(stamp) = stamp(path, limit)? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        bail!("Live file exceeds limit");
    }
    Ok(Some((stamp, bytes)))
}

fn not_found(relative: &str) -> anyhow::Error {
    std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("Live file {relative} not found"),
    )
    .into()
}

/// Read and validate `relative` from the given roots in order. An unchanged file is not
/// read again; `validate` still runs, since callers pass different checks.
pub fn read_from(
    roots: &[PathBuf],
    relative: &str,
    limit: u64,
    validate: impl Fn(&[u8]) -> Result<()>,
) -> Result<Vec<u8>> {
    checked(relative)?;
    // Keyed by the root set too, so separate fixtures never share a cached file.
    let key = format!("{roots:?}\0{relative}");
    let last_good = || {
        cache(&LAST_GOOD)
            .as_ref()
            .and_then(|seen| seen.get(&key))
            .map(|seen| seen.bytes.clone())
    };
    let mut first_error = None;
    for root in roots {
        let path = root.join(relative);
        let unchanged = stamp(&path, limit).ok().flatten().and_then(|stamp| {
            cache(&LAST_GOOD)
                .as_ref()
                .and_then(|seen| seen.get(&key))
                .filter(|seen| seen.path == path && seen.stamp == stamp && settled(&stamp))
                .map(|seen| seen.bytes.clone())
        });
        if let Some(bytes) = unchanged.filter(|bytes| validate(bytes).is_ok()) {
            return Ok(bytes);
        }
        let attempt = read_bounded(&path, limit).and_then(|read| {
            read.map(|(stamp, bytes)| validate(&bytes).map(|_| (stamp, bytes)))
                .transpose()
        });
        match attempt {
            Ok(None) => continue,
            Ok(Some((stamp, bytes))) => {
                cache(&LAST_GOOD).get_or_insert_with(HashMap::new).insert(
                    key.clone(),
                    Seen {
                        path,
                        stamp,
                        bytes: bytes.clone(),
                    },
                );
                return Ok(bytes);
            }
            Err(error) => {
                if let Some(bytes) = last_good() {
                    eprintln!(
                        "Live file {relative} is invalid; serving its last good version: {error:#}"
                    );
                    return Ok(bytes);
                }
                first_error.get_or_insert(error);
            }
        }
    }
    Err(first_error.unwrap_or_else(|| not_found(relative)))
}

/// The cached value for `key`: the last good one when `current` is `None`, otherwise only
/// one parsed from exactly `current`. `Err(())` means `current` already failed to parse.
fn parsed_hit<T: Clone + 'static>(
    key: &ParsedKey,
    current: Option<(&Path, Stamp)>,
) -> std::result::Result<Option<T>, ()> {
    let cache = cache(&PARSED);
    let Some(entry) = cache.get(key) else {
        return Ok(None);
    };
    let good = entry.good.as_ref();
    let value = |good: Option<&(PathBuf, Stamp, Box<dyn Any + Send + Sync>)>| {
        good.and_then(|(_, _, value)| value.downcast_ref::<T>().cloned())
    };
    let Some((path, stamp)) = current else {
        return Ok(value(good));
    };
    if !settled(&stamp) {
        return Ok(None);
    }
    if let Some(hit) = value(good.filter(|(p, s, _)| p == path && *s == stamp)) {
        return Ok(Some(hit));
    }
    if entry
        .bad
        .as_ref()
        .is_some_and(|(p, s)| p == path && *s == stamp)
    {
        return value(good).map(Some).ok_or(());
    }
    Ok(None)
}

/// Parse live file `relative` from the given roots in order. `parse` sees only the bytes,
/// so its value is kept per file, type and parser and reused while the file is unchanged.
/// A broken edit is parsed once, logged once and then serves the last good value.
pub fn parsed_from<T: Clone + Send + Sync + 'static>(
    roots: &[PathBuf],
    relative: &str,
    limit: u64,
    parse: fn(&[u8]) -> Result<T>,
) -> Result<T> {
    checked(relative)?;
    // A parser compiled twice only costs an extra parse; it never shares a wrong value.
    let key = (
        format!("{roots:?}\0{relative}"),
        TypeId::of::<T>(),
        parse as usize,
    );
    let mut first_error = None;
    for root in roots {
        let path = root.join(relative);
        let current = match stamp(&path, limit) {
            Ok(None) => continue,
            Ok(Some(stamp)) => stamp,
            Err(error) => {
                if let Ok(Some(value)) = parsed_hit::<T>(&key, None) {
                    return Ok(value);
                }
                first_error.get_or_insert(error);
                continue;
            }
        };
        match parsed_hit::<T>(&key, Some((&path, current))) {
            Ok(Some(value)) => return Ok(value),
            Ok(None) => {}
            Err(()) => {
                first_error.get_or_insert_with(|| anyhow!("Live file {relative} is invalid"));
                continue;
            }
        }
        let attempt = read_bounded(&path, limit).and_then(|read| {
            read.map(|(stamp, bytes)| parse(&bytes).map(|value| (stamp, value)))
                .transpose()
        });
        match attempt {
            Ok(None) => continue,
            Ok(Some((stamp, value))) => {
                let mut cache = cache(&PARSED);
                let entry = cache.entry(key).or_default();
                // A good fallback copy leaves a broken checkout version marked as broken.
                if entry.bad.as_ref().is_some_and(|(bad, _)| *bad == path) {
                    entry.bad = None;
                }
                entry.good = Some((path, stamp, Box::new(value.clone())));
                return Ok(value);
            }
            Err(error) => {
                cache(&PARSED).entry(key.clone()).or_default().bad = Some((path, current));
                if let Ok(Some(value)) = parsed_hit::<T>(&key, None) {
                    eprintln!(
                        "Live file {relative} is invalid; serving its last good version: {error:#}"
                    );
                    return Ok(value);
                }
                first_error.get_or_insert(error);
            }
        }
    }
    Err(first_error.unwrap_or_else(|| not_found(relative)))
}

/// Parse a live file from the default roots.
pub fn parsed<T: Clone + Send + Sync + 'static>(
    relative: &str,
    limit: u64,
    parse: fn(&[u8]) -> Result<T>,
) -> Result<T> {
    parsed_from(&roots(), relative, limit, parse)
}

/// `parsed` when `path` is checkout-relative, otherwise that exact file parsed as is
/// (test fixtures keep their behavior: read every time, no last good value).
pub fn parsed_path<T: Clone + Send + Sync + 'static>(
    path: &Path,
    limit: u64,
    parse: fn(&[u8]) -> Result<T>,
) -> Result<T> {
    if path.is_relative() {
        return parsed(
            path.to_str()
                .ok_or_else(|| anyhow!("Invalid live file path"))?,
            limit,
            parse,
        );
    }
    let (_, bytes) = read_bounded(path, limit)?.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "Configuration file not found")
    })?;
    parse(&bytes)
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
    let (_, bytes) = read_bounded(path, limit)?.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "Configuration file not found")
    })?;
    Ok(bytes)
}

/// True when `error` means the file does not exist anywhere.
pub fn missing(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

/// Parse a UTF-8 text file such as a Markdown prompt.
pub fn utf8(bytes: &[u8]) -> Result<String> {
    String::from_utf8(bytes.to_vec()).map_err(|_| anyhow!("Live text file is not UTF-8"))
}

/// Read a live UTF-8 text file such as a Markdown prompt.
pub fn text(relative: &str, limit: u64) -> Result<String> {
    parsed(relative, limit, utf8)
}

/// `(name, is_folder)` entries of `relative` from the first of `roots` that has that
/// folder. An unchanged folder is not listed again.
pub fn entries_from(roots: &[PathBuf], relative: &str) -> Result<Vec<(String, bool)>> {
    checked(relative)?;
    for root in roots {
        let folder = root.join(relative);
        // Missing, unreadable or symlinked folders fall through to the next root.
        let Ok(info) = std::fs::symlink_metadata(&folder) else {
            continue;
        };
        if !info.is_dir() {
            continue;
        }
        // Stamped before listing, so an entry added during the listing is a change next time.
        let stamp = version(&info);
        if let Some((seen, names)) = cache(&LISTED).get(&folder) {
            if *seen == stamp && settled(&stamp) {
                return Ok(names.clone());
            }
        }
        // A folder this process cannot list yet (permissions not exposed) falls through.
        let Ok(items) = std::fs::read_dir(&folder) else {
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
        cache(&LISTED).insert(folder, (stamp, names.clone()));
        return Ok(names);
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

    static PARSES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    fn counted(bytes: &[u8]) -> Result<u64> {
        PARSES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        value["v"].as_u64().ok_or_else(|| anyhow!("No version"))
    }
    fn parses() -> usize {
        PARSES.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[test]
    fn listings_follow_added_removed_and_replaced_entries() {
        let (live, published) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let roots = [live.path().to_path_buf(), published.path().to_path_buf()];
        std::fs::create_dir(published.path().join("set")).unwrap();
        std::fs::write(published.path().join("set/a.json"), "{}").unwrap();
        let list = || entries_from(&roots, "set").unwrap();
        assert_eq!(list(), [("a.json".to_owned(), false)]);
        assert_eq!(list(), [("a.json".to_owned(), false)]);
        std::fs::write(published.path().join("set/b.json"), "{}").unwrap();
        assert_eq!(list().len(), 2);
        std::fs::remove_file(published.path().join("set/b.json")).unwrap();
        std::fs::create_dir(published.path().join("set/b.json")).unwrap();
        assert_eq!(list()[1], ("b.json".to_owned(), true));
        // The checkout's folder wins once it appears.
        std::fs::create_dir(live.path().join("set")).unwrap();
        assert!(list().is_empty());
        std::fs::write(live.path().join("set/c.md"), "c").unwrap();
        assert_eq!(list(), [("c.md".to_owned(), false)]);
    }

    /// Write a fixture and backdate it (uniquely) so it counts as settled.
    fn put(path: impl AsRef<Path>, bytes: &[u8]) {
        static AGE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::fs::write(&path, bytes).unwrap();
        let age = 1000 - AGE.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let when = SystemTime::now() - std::time::Duration::from_secs(age);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    #[test]
    fn parsed_values_are_reused_until_the_file_changes() {
        let (live, published) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let roots = [live.path().to_owned(), published.path().to_owned()];
        let file = live.path().join("a.json");
        let read = || parsed_from(&roots, "a.json", 64, counted);
        put(&file, b"{\"v\":1}");
        let start = parses();
        assert_eq!(read().unwrap(), 1);
        assert_eq!(read().unwrap(), 1);
        assert_eq!(parses() - start, 1, "an unchanged file is parsed once");
        // Same size, written in place straight after a read: still seen as a change.
        put(&file, b"{\"v\":2}");
        assert_eq!(read().unwrap(), 2);
        assert_eq!(parses() - start, 2);
        // A broken edit is parsed once and serves the last good value until fixed.
        put(&file, b"{broken");
        assert_eq!(read().unwrap(), 2);
        assert_eq!(read().unwrap(), 2);
        assert_eq!(parses() - start, 3, "a broken version is parsed once");
        put(&file, b"{\"v\":3}");
        assert_eq!(read().unwrap(), 3);
        // A missing checkout file uses the release; a broken one with no good value too.
        std::fs::remove_file(&file).unwrap();
        put(published.path().join("a.json"), b"{\"v\":9}");
        assert_eq!(read().unwrap(), 9);
        put(live.path().join("b.json"), b"{broken");
        put(published.path().join("b.json"), b"{\"v\":5}");
        let before = parses();
        for _ in 0..3 {
            assert_eq!(parsed_from(&roots, "b.json", 64, counted).unwrap(), 5);
        }
        assert_eq!(
            parses() - before,
            2,
            "neither copy is parsed again while unchanged"
        );
        assert!(missing(
            &parsed_from(&roots, "c.json", 64, counted).unwrap_err()
        ));
        assert!(parsed_from(&roots, "../a.json", 64, counted).is_err());
    }
}
