//! The administrator File Manager on the live development checkout
//! (`administrator_file_manager`), ported from the Git Control `file_manager.py`. The host
//! admits only an authenticated administrator; this module confines paths to the checkout,
//! hides protected folders and changes files without following symlinks.
use base64::Engine;
use rustix::fs::{AtFlags, FileType, Mode, OFlags, RenameFlags, Stat};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::path::Path;

/// The Files page's stored repository identifier; it predates the `/code` mount name.
pub(crate) const VIRTUAL: &str = "/live-checkout";
pub(crate) const MAX_FILE: u64 = 1024 * 1024;
pub(crate) const MAX_ENTRIES: usize = 2000;
pub(crate) const HIDDEN: [&str; 10] = [
    ".git",
    "data",
    "target",
    "workspaces",
    "rollback",
    "runtime",
    "gate-runtime",
    "host-build-service",
    "deployment-secrets",
    ".cache",
];

/// Why a File Manager operation failed.
#[derive(Debug)]
pub(crate) enum Error {
    /// Refused with a message the page shows.
    Rejected(String),
    /// A filesystem error; reported generically.
    Io(io::Error),
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rustix::io::Errno> for Error {
    fn from(error: rustix::io::Errno) -> Self {
        Self::Io(error.into())
    }
}

pub(crate) fn rejected(message: impl Into<String>) -> Error {
    Error::Rejected(message.into())
}

pub(crate) type Outcome<T> = Result<T, Error>;

fn hidden(name: &str) -> bool {
    HIDDEN.contains(&name) || name.starts_with(".v3-")
}

/// The checkout-relative components of `raw` (a relative path or one under [`VIRTUAL`]).
pub(crate) fn parts(raw: &Value, directory: bool) -> Outcome<Vec<String>> {
    let raw = raw
        .as_str()
        .filter(|raw| raw.len() <= 1024 && !raw.contains('\0') && !raw.contains('\\'))
        .ok_or_else(|| rejected("Invalid File Manager path"))?;
    let raw = if raw == VIRTUAL {
        ""
    } else {
        raw.strip_prefix(&format!("{VIRTUAL}/")).unwrap_or(raw)
    };
    if raw.is_empty() {
        if directory {
            return Ok(Vec::new());
        }
        return Err(rejected("A file path is required"));
    }
    if raw.starts_with('/') || raw.split('/').any(|item| matches!(item, "" | "." | "..")) {
        return Err(rejected("Path must stay inside the development checkout"));
    }
    let items: Vec<String> = raw.split('/').map(str::to_owned).collect();
    if items.iter().any(|item| hidden(item)) {
        return Err(rejected("Protected checkout path"));
    }
    if items[0] == "v3"
        && items
            .get(1)
            .is_some_and(|item| HIDDEN.contains(&item.as_str()))
    {
        return Err(rejected("Protected V3 runtime path"));
    }
    Ok(items)
}

pub(crate) fn display(items: &[String]) -> String {
    if items.is_empty() {
        VIRTUAL.to_owned()
    } else {
        format!("{VIRTUAL}/{}", items.join("/"))
    }
}

fn root_fd(root: &Path) -> Outcome<OwnedFd> {
    let meta = std::fs::symlink_metadata(root);
    if !meta.is_ok_and(|meta| meta.is_dir()) {
        return Err(rejected("Development checkout unavailable"));
    }
    Ok(rustix::fs::open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}

fn open_directory(parent: &OwnedFd, name: &str) -> Outcome<OwnedFd> {
    Ok(rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}

fn directory_fd(root: &Path, items: &[String]) -> Outcome<OwnedFd> {
    let mut fd = root_fd(root)?;
    for item in items {
        fd = open_directory(&fd, item)?;
    }
    Ok(fd)
}

/// The parent directory of file `raw` and the file's name in it.
pub(crate) fn parent_fd(root: &Path, raw: &Value) -> Outcome<(OwnedFd, String)> {
    let mut items = parts(raw, false)?;
    let name = items.pop().unwrap_or_default();
    Ok((directory_fd(root, &items)?, name))
}

fn identity(stat: &Stat) -> (u64, u64, i64, i64, i64) {
    (
        stat.st_dev,
        stat.st_ino,
        stat.st_size,
        stat.st_mtime,
        stat.st_mtime_nsec as i64,
    )
}

fn regular_bytes(file: OwnedFd) -> Outcome<(Vec<u8>, Stat)> {
    let before = rustix::fs::fstat(&file)?;
    if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile
        || before.st_size as u64 > MAX_FILE
    {
        return Err(rejected("Only bounded regular files are available"));
    }
    let mut data = Vec::new();
    let mut file = std::fs::File::from(file);
    (&mut file).take(MAX_FILE + 1).read_to_end(&mut data)?;
    if data.len() as u64 > MAX_FILE {
        return Err(rejected("File exceeds the one MiB editor limit"));
    }
    let after = rustix::fs::fstat(&file)?;
    if identity(&before) != identity(&after) {
        return Err(rejected("File changed during read"));
    }
    Ok((data, before))
}

fn open_file(parent: &OwnedFd, name: &str) -> rustix::io::Result<OwnedFd> {
    rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
}

pub(crate) fn sha256(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

pub(crate) fn read(root: &Path, raw: &Value) -> Outcome<Value> {
    let (parent, name) = parent_fd(root, raw)?;
    let (content, _) = regular_bytes(open_file(&parent, &name)?)?;
    let (value, binary, encoding) = match std::str::from_utf8(&content) {
        Ok(text) => (text.to_owned(), false, "utf-8"),
        Err(_) => (
            base64::engine::general_purpose::STANDARD.encode(&content),
            true,
            "base64",
        ),
    };
    Ok(
        json!({"path":display(&parts(raw, false)?),"content":value,"encoding":encoding,
        "binary":binary,"size":content.len(),"truncated":false,"sha256":sha256(&content)}),
    )
}

fn tree(root: &Path, raw: &Value, show_hidden: &Value) -> Outcome<Value> {
    let show_hidden = show_hidden
        .as_bool()
        .ok_or_else(|| rejected("Invalid hidden-file setting"))?;
    let items = parts(raw, true)?;
    let fd = directory_fd(root, &items)?;
    let mut entries = Vec::new();
    let mut listing = rustix::fs::Dir::read_from(&fd)?;
    while let Some(entry) = listing.next() {
        let entry = entry?;
        let Ok(name) = entry.file_name().to_str() else {
            continue;
        };
        if matches!(name, "." | "..") {
            continue;
        }
        if entries.len() >= MAX_ENTRIES {
            return Err(rejected("Directory exceeds File Manager limit"));
        }
        if hidden(name) {
            continue;
        }
        if !show_hidden
            && name.starts_with('.')
            && !matches!(name, ".env.example" | ".gitignore" | ".github")
        {
            continue;
        }
        let info = rustix::fs::statat(&fd, name, AtFlags::SYMLINK_NOFOLLOW)?;
        let kind = FileType::from_raw_mode(info.st_mode);
        if !matches!(kind, FileType::RegularFile | FileType::Directory) {
            continue;
        }
        let is_dir = kind == FileType::Directory;
        let mut path = items.clone();
        path.push(name.to_owned());
        entries.push((
            !is_dir,
            name.to_lowercase(),
            name.to_owned(),
            json!({"name":name,"path":display(&path),"is_dir":is_dir,
                "size":if is_dir {0} else {info.st_size}}),
        ));
    }
    entries.sort_by(|a, b| (a.0, &a.1, &a.2).cmp(&(b.0, &b.1, &b.2)));
    let parent = (!items.is_empty()).then(|| display(&items[..items.len() - 1]));
    Ok(
        json!({"path":display(&items),"parent":parent,"project_root":VIRTUAL,
        "entries":entries.into_iter().map(|entry| entry.3).collect::<Vec<_>>()}),
    )
}

fn fsync_directory(fd: &OwnedFd) -> Outcome<()> {
    Ok(rustix::fs::fsync(fd)?)
}

pub(crate) fn write(
    root: &Path,
    raw: &Value,
    content: &Value,
    encoding: &Value,
    expected: &Value,
) -> Outcome<Value> {
    let (Some(content), Some(encoding @ ("utf-8" | "base64"))) =
        (content.as_str(), encoding.as_str())
    else {
        return Err(rejected("Invalid file content"));
    };
    let data = if encoding == "utf-8" {
        content.as_bytes().to_vec()
    } else {
        base64::engine::general_purpose::STANDARD
            .decode(content)
            .map_err(|_| rejected("Invalid file encoding"))?
    };
    if data.len() as u64 > MAX_FILE {
        return Err(rejected("File exceeds the one MiB editor limit"));
    }
    let expected = expected
        .as_str()
        .filter(|expected| {
            expected.is_empty()
                || expected.len() == 64
                    && expected
                        .bytes()
                        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
        .ok_or_else(|| rejected("Expected file digest required"))?;
    let (parent, name) = parent_fd(root, raw)?;
    let temp = format!(".v3-file-{}", uuid::Uuid::new_v4().simple());
    let old = match open_file(&parent, &name) {
        Ok(file) => Some(file),
        Err(rustix::io::Errno::NOENT) => None,
        Err(error) => return Err(error.into()),
    };
    let (mode, old_stat) = match old {
        None if !expected.is_empty() => {
            return Err(rejected("File disappeared; reload before saving"))
        }
        None => (0o660, None),
        Some(file) => {
            let (old, old_stat) = regular_bytes(file)?;
            if sha256(&old) != expected {
                return Err(rejected("File changed; reload before saving"));
            }
            (old_stat.st_mode & 0o7777, Some(old_stat))
        }
    };
    let saved = (|| -> Outcome<()> {
        let file = rustix::fs::openat(
            &parent,
            &temp,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )?;
        let mut output = std::fs::File::from(file);
        output.write_all(&data)?;
        output.flush()?;
        rustix::fs::fchmod(&output, Mode::from_raw_mode(mode))?;
        output.sync_all()?;
        drop(output);
        match old_stat {
            None => rustix::fs::linkat(&parent, &temp, &parent, &name, AtFlags::empty())?,
            Some(old_stat) => {
                let current = rustix::fs::statat(&parent, &name, AtFlags::SYMLINK_NOFOLLOW)?;
                if identity(&current) != identity(&old_stat) {
                    return Err(rejected("File changed during save"));
                }
                rustix::fs::renameat(&parent, &temp, &parent, &name)?;
            }
        }
        fsync_directory(&parent)
    })();
    match rustix::fs::unlinkat(&parent, &temp, AtFlags::empty()) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => {}
        Err(error) if saved.is_ok() => return Err(error.into()),
        Err(_) => {}
    }
    saved?;
    Ok(
        json!({"path":display(&parts(raw, false)?),"size":data.len(),"status":"ok",
        "sha256":sha256(&data)}),
    )
}

fn create(root: &Path, raw: &Value, kind: &Value) -> Outcome<Value> {
    let kind = kind
        .as_str()
        .filter(|kind| matches!(*kind, "file" | "dir"))
        .ok_or_else(|| rejected("Invalid entry kind"))?;
    let (parent, name) = parent_fd(root, raw)?;
    if kind == "dir" {
        rustix::fs::mkdirat(&parent, &name, Mode::from_raw_mode(0o770))?;
    } else {
        drop(rustix::fs::openat(
            &parent,
            &name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o660),
        )?);
    }
    fsync_directory(&parent)?;
    Ok(json!({"path":display(&parts(raw, false)?),"kind":kind,"status":"ok"}))
}

fn rename(root: &Path, raw: &Value, new_raw: &Value) -> Outcome<Value> {
    let (source, old_name) = parent_fd(root, raw)?;
    let (target, new_name) = parent_fd(root, new_raw)?;
    let info = rustix::fs::statat(&source, &old_name, AtFlags::SYMLINK_NOFOLLOW)?;
    if FileType::from_raw_mode(info.st_mode) == FileType::Symlink {
        return Err(rejected("Symlink entries unavailable"));
    }
    // RENAME_NOREPLACE never overwrites a raced destination.
    rustix::fs::renameat_with(
        &source,
        &old_name,
        &target,
        &new_name,
        RenameFlags::NOREPLACE,
    )?;
    fsync_directory(&source)?;
    fsync_directory(&target)?;
    Ok(
        json!({"from":display(&parts(raw, false)?),"to":display(&parts(new_raw, false)?),
        "status":"ok"}),
    )
}

fn children(fd: &OwnedFd) -> Outcome<Vec<String>> {
    let mut names = Vec::new();
    let mut listing = rustix::fs::Dir::read_from(fd)?;
    while let Some(entry) = listing.next() {
        let entry = entry?;
        let name = entry
            .file_name()
            .to_str()
            .map_err(|_| rejected("Directory contains protected or too many entries"))?;
        if !matches!(name, "." | "..") {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

/// Walks `name` under `parent`, refusing protected entries or more than the budget; with
/// `remove`, deletes as it goes (files first, then each emptied directory).
fn walk_delete(parent: &OwnedFd, name: &str, budget: &mut i64, remove: bool) -> Outcome<()> {
    let info = rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
    match FileType::from_raw_mode(info.st_mode) {
        FileType::Symlink if remove => return Err(rejected("Symlink entries unavailable")),
        FileType::RegularFile => {
            if remove {
                rustix::fs::unlinkat(parent, name, AtFlags::empty())?;
            }
            return Ok(());
        }
        FileType::Directory => {}
        _ => {
            return Err(rejected(
                "Only ordinary files and directories can be deleted",
            ))
        }
    }
    let fd = open_directory(parent, name)?;
    for child in children(&fd)? {
        *budget -= 1;
        if *budget < 0 || hidden(&child) {
            return Err(rejected("Directory contains protected or too many entries"));
        }
        walk_delete(&fd, &child, budget, remove)?;
    }
    drop(fd);
    if remove {
        rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR)?;
    }
    Ok(())
}

fn delete(root: &Path, raw: &Value) -> Outcome<Value> {
    let (parent, name) = parent_fd(root, raw)?;
    walk_delete(&parent, &name, &mut (MAX_ENTRIES as i64), false)?;
    walk_delete(&parent, &name, &mut (MAX_ENTRIES as i64), true)?;
    fsync_directory(&parent)?;
    Ok(json!({"path":display(&parts(raw, false)?),"status":"deleted"}))
}

/// Runs one File Manager request on the checkout at `root`.
#[must_use]
pub fn execute(root: &Path, args: &Value) -> Value {
    let Value::Object(args) = args else {
        return json!({"status":"error","output":"Invalid File Manager request"});
    };
    let known: &[&str] = match args.get("action").and_then(Value::as_str) {
        Some("tree") => &["action", "path", "show_hidden"],
        Some("read" | "delete") => &["action", "path"],
        Some("write") => &["action", "path", "content", "encoding", "expected_sha256"],
        Some("create") => &["action", "path", "kind"],
        Some("rename") => &["action", "path", "new_path"],
        _ => return json!({"status":"error","output":"Unsupported File Manager action"}),
    };
    if args.keys().any(|key| !known.contains(&key.as_str())) {
        return json!({"status":"error","output":"Unsupported File Manager action"});
    }
    match run(root, args) {
        Ok(value) => json!({"status":"success","output":value}),
        Err(Error::Rejected(message)) => json!({"status":"error","output":message}),
        Err(Error::Io(_)) => json!({"status":"error","output":"File Manager operation failed"}),
    }
}

fn run(root: &Path, args: &Map<String, Value>) -> Outcome<Value> {
    // A missing required argument fails like any other unusable request.
    let required = |key: &str| {
        args.get(key)
            .ok_or_else(|| Error::Io(io::Error::from(io::ErrorKind::InvalidInput)))
    };
    let empty = json!("");
    match args.get("action").and_then(Value::as_str) {
        Some("tree") => tree(
            root,
            args.get("path").unwrap_or(&empty),
            args.get("show_hidden").unwrap_or(&Value::Bool(false)),
        ),
        Some("read") => read(root, required("path")?),
        Some("write") => write(
            root,
            required("path")?,
            required("content")?,
            required("encoding")?,
            required("expected_sha256")?,
        ),
        Some("create") => create(root, required("path")?, required("kind")?),
        Some("rename") => rename(root, required("path")?, required("new_path")?),
        _ => delete(root, required("path")?),
    }
}

#[cfg(test)]
mod tests {
    use super::execute;
    use serde_json::json;

    fn checkout() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("checkout");
        for (path, text) in [("a.txt", "one\n"), ("b/c.txt", "c\n"), ("data/keep", "x\n")] {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("folder");
            std::fs::write(path, text).expect("file");
        }
        dir
    }

    #[test]
    fn listing_hides_protected_folders_and_reads_confine_paths() {
        let dir = checkout();
        let tree = execute(dir.path(), &json!({"action":"tree"}));
        let names: Vec<_> = tree["output"]["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .map(|entry| entry["name"].clone())
            .collect();
        assert_eq!(names, [json!("b"), json!("a.txt")]);
        let read = execute(
            dir.path(),
            &json!({"action":"read","path":"/live-checkout/a.txt"}),
        );
        assert_eq!(read["output"]["content"], "one\n", "{read}");
        for path in [
            "../x",
            "/etc/passwd",
            "data/keep",
            "b/../a.txt",
            ".v3-file-x",
        ] {
            let refused = execute(dir.path(), &json!({"action":"read","path":path}));
            assert_eq!(refused["status"], "error", "{path}: {refused}");
        }
    }

    #[test]
    fn writes_need_the_current_digest_and_deletes_stop_at_protected_entries() {
        let dir = checkout();
        let read = execute(dir.path(), &json!({"action":"read","path":"a.txt"}));
        let stale = execute(
            dir.path(),
            &json!({"action":"write","path":"a.txt","content":"two\n","encoding":"utf-8",
                "expected_sha256":"0".repeat(64)}),
        );
        assert_eq!(stale["output"], "File changed; reload before saving");
        let saved = execute(
            dir.path(),
            &json!({"action":"write","path":"a.txt","content":"two\n","encoding":"utf-8",
                "expected_sha256":read["output"]["sha256"]}),
        );
        assert_eq!(saved["status"], "success", "{saved}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).expect("file"),
            "two\n"
        );
        std::fs::create_dir_all(dir.path().join("b/data")).expect("folder");
        let refused = execute(dir.path(), &json!({"action":"delete","path":"b"}));
        assert_eq!(
            refused["output"],
            "Directory contains protected or too many entries"
        );
        assert!(dir.path().join("b/c.txt").exists());
    }
}
