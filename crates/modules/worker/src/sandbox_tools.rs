//! The Sandbox ability's worker side: `sandbox_shell`, `sandbox_python` and `sandbox_file`
//! in a session's scratch folder, `<workspace>/sandbox`. Unlike Codebase Admin's commands
//! they read only the system folders the shell and Python need, have no network at all,
//! run under resource limits and a folder size cap ([`crate::sandbox::prepare_strict`],
//! [`crate::runner`]), and their file operations never follow a symlink. The app sends the
//! limits as trusted fields; a request without them is refused, and each is clamped to a
//! ceiling no configuration can raise.
use crate::files::{self, Roots};
use rustix::fs::{AtFlags, Mode, OFlags, ResolveFlags};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// The tools this module runs.
pub const TOOLS: [&str; 4] = [
    "sandbox_shell",
    "sandbox_python",
    "sandbox_file",
    "sandbox_element",
];
const MIB: u64 = 1024 * 1024;
/// Folder entries a size count walks before it calls the folder full.
const MAX_ENTRIES: usize = 50_000;
/// What each file or folder counts for besides its bytes, so empty files still fill it.
const ENTRY_BYTES: u64 = 256;
const LIST_LIMIT: usize = 500;
/// The largest `index.html` a proposal reads; the ability's own limit is lower.
const ELEMENT_READ: u64 = 512 * 1024;

/// Whether this module runs `name`.
#[must_use]
pub fn handles(name: &str) -> bool {
    TOOLS.contains(&name)
}

/// The limits the app sends for one call (`trusted.sandbox`), from the ability's
/// `limits.json` and the storage owner's remaining room.
#[derive(Clone, Copy, Debug)]
struct Limits {
    timeout_seconds: u64,
    memory_mb: u64,
    processes: u64,
    file_mb: u64,
    open_files: u64,
    folder_mb: u64,
    read_kb: u64,
    /// Bytes the storage owner may still add: their storage limit less their use.
    room_bytes: u64,
}

impl Limits {
    fn from_request(request: &Value) -> Result<Self, String> {
        let sent = &request["trusted"]["sandbox"];
        let field = |key: &str, low: u64, high: u64| {
            sent[key]
                .as_u64()
                .map(|value| value.clamp(low, high))
                .ok_or_else(|| "Sandbox limits missing".to_owned())
        };
        Ok(Self {
            timeout_seconds: field("timeout_seconds", 1, 600)?,
            memory_mb: field("memory_mb", 64, 4096)?,
            processes: field("processes", 1, 256)?,
            file_mb: field("file_mb", 1, 100)?,
            open_files: field("open_files", 16, 1024)?,
            folder_mb: field("folder_mb", 1, 500)?,
            read_kb: field("read_kb", 1, 1024)?,
            room_bytes: field("room_bytes", 0, u64::MAX)?,
        })
    }

    /// The folder's byte cap for a call that starts at `used`: the folder limit, or less
    /// when the owner's storage room is smaller.
    fn cap(&self, used: u64) -> u64 {
        (self.folder_mb * MIB).min(used.saturating_add(self.room_bytes))
    }
}

/// What the supervisor applies to a sandbox command (`V3_SANDBOX_CONFINEMENT`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Confinement {
    pub cpu_seconds: u64,
    pub memory_bytes: u64,
    /// Processes and threads the command may add to its user's count.
    pub processes: u64,
    pub file_bytes: u64,
    pub open_files: u64,
    /// The command is stopped once the folder holds more than this.
    pub folder_cap_bytes: u64,
}

impl Confinement {
    const KEYS: [&'static str; 6] = [
        "cpu_seconds",
        "memory_bytes",
        "processes",
        "file_bytes",
        "open_files",
        "folder_cap_bytes",
    ];

    fn values(self) -> [u64; 6] {
        [
            self.cpu_seconds,
            self.memory_bytes,
            self.processes,
            self.file_bytes,
            self.open_files,
            self.folder_cap_bytes,
        ]
    }

    #[must_use]
    pub fn to_json(self) -> String {
        Value::Object(
            Self::KEYS
                .iter()
                .zip(self.values())
                .map(|(key, value)| ((*key).to_owned(), json!(value)))
                .collect(),
        )
        .to_string()
    }

    /// The confinement in `text`, which must name every field.
    ///
    /// # Errors
    /// When a field is missing or not a whole number.
    pub fn parse(text: &str) -> io::Result<Self> {
        let value: Value = serde_json::from_str(text)?;
        let mut fields = [0u64; 6];
        for (slot, key) in fields.iter_mut().zip(Self::KEYS) {
            *slot = value[key].as_u64().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid sandbox confinement")
            })?;
        }
        let [cpu_seconds, memory_bytes, processes, file_bytes, open_files, folder_cap_bytes] =
            fields;
        Ok(Self {
            cpu_seconds,
            memory_bytes,
            processes,
            file_bytes,
            open_files,
            folder_cap_bytes,
        })
    }
}

/// Runs one Sandbox request. Dropping the future stops a running command, as for
/// [`crate::commands::call`].
pub async fn call(roots: &Roots, runner: &[String], request: &Value) -> Value {
    if request["name"] == "sandbox_element" {
        let (roots, request) = (roots.clone(), request.clone());
        return tokio::task::spawn_blocking(move || {
            element(&roots, &request)
                .unwrap_or_else(|output| json!({"status":"failed","output":output}))
        })
        .await
        .unwrap_or_else(|_| json!({"status":"failed","output":"Sandbox element sync failed"}));
    }
    let admitted = {
        let roots = roots.clone();
        let request = request.clone();
        tokio::task::spawn_blocking(move || admit(&roots, &request)).await
    };
    let (scratch, name, args, limits, used) = match admitted {
        Ok(Ok(admitted)) => admitted,
        Ok(Err(output)) => return json!({"status":"failed","output":output}),
        Err(_) => return json!({"status":"failed","output":"Sandbox admission failed"}),
    };
    let mut result = if name == "sandbox_file" {
        let scratch = scratch.clone();
        tokio::task::spawn_blocking(move || {
            file(&scratch, &args, &limits, used)
                .unwrap_or_else(|output| json!({"status":"failed","output":output}))
        })
        .await
        .unwrap_or_else(|_| json!({"status":"failed","output":"Sandbox file operation failed"}))
    } else {
        command(runner, &scratch, &name, &args, &limits, used).await
    };
    let after = {
        let scratch = scratch.clone();
        tokio::task::spawn_blocking(move || folder_bytes(&scratch))
            .await
            .unwrap_or(u64::MAX)
    };
    let session = request["workspace"].as_str().unwrap_or_default().to_owned();
    let usage = roots.port_claims.with_file_name("sandbox-usage.json");
    let _ = tokio::task::spawn_blocking(move || publish_usage(&usage, &session, after)).await;
    result["sandbox_bytes"] = json!(after);
    result["implementation_version"] = json!(files::IMPLEMENTATION_VERSION);
    result["plugin_revision"] = Value::Null;
    result
}

type Admitted = (PathBuf, String, Value, Limits, u64);

/// The read-only copy of the agent's saved UI elements beside the scratch folder:
/// `<workspace>/saved/{mine,shared}/<name>/index.html`. Sandbox commands read it at
/// `../saved` and `sandbox_file` at `saved/...`; only the app writes it, through
/// [`element`].
fn saved_root(scratch: &Path) -> PathBuf {
    scratch.with_file_name("saved")
}

/// `mine/<name>` or `shared/<name>` with a valid element name.
fn mirror_path(path: &str) -> Option<(&str, &str)> {
    let (scope, name) = path.split_once('/')?;
    let named = (1..=40).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'));
    (matches!(scope, "mine" | "shared") && named).then_some((scope, name))
}

/// The app's sync of a session's element copy (`sandbox_element`, trusted only): `status`
/// lists each copied element's revision, `put` writes one, `remove` drops one.
fn element(roots: &Roots, request: &Value) -> Result<Value, String> {
    if request["trusted"]["element_sync"] != true {
        return Err("Sandbox element sync is the app's own".into());
    }
    let (base, _, args) = files::admit(roots, request).map_err(|f| f.to_string())?;
    let saved = base.join("saved");
    for dir in [saved.clone(), saved.join("mine"), saved.join("shared")] {
        match std::fs::DirBuilder::new().mode(0o750).create(&dir) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            result => result.map_err(|error| error.to_string())?,
        }
        if !std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.is_dir()) {
            return Err("Invalid saved elements folder".into());
        }
    }
    match args["action"].as_str() {
        Some("status") => {
            let mut revisions = serde_json::Map::new();
            for scope in ["mine", "shared"] {
                for entry in std::fs::read_dir(saved.join(scope))
                    .into_iter()
                    .flatten()
                    .flatten()
                {
                    let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                        continue;
                    };
                    let path = format!("{scope}/{name}");
                    if mirror_path(&path).is_none() || !entry.file_type().is_ok_and(|t| t.is_dir())
                    {
                        continue;
                    }
                    let revision = std::fs::read_to_string(entry.path().join(".revision"))
                        .ok()
                        .and_then(|text| text.trim().parse::<u64>().ok());
                    revisions.insert(path, json!(revision));
                }
            }
            Ok(json!({"status":"success","output":{"revisions":revisions}}))
        }
        Some("put") => {
            let path = args["path"]
                .as_str()
                .and_then(mirror_path)
                .ok_or("Invalid element path")?;
            let html = args["html"].as_str().ok_or("html must be text")?;
            let revision = args["revision"]
                .as_u64()
                .ok_or("revision must be a whole number")?;
            let dir = saved.join(path.0).join(path.1);
            let temporary = saved.join(path.0).join(format!(".tmp-{}", path.1));
            let _ = std::fs::remove_dir_all(&temporary);
            std::fs::DirBuilder::new()
                .mode(0o750)
                .create(&temporary)
                .map_err(|error| error.to_string())?;
            std::fs::write(temporary.join("index.html"), html).map_err(|e| e.to_string())?;
            std::fs::write(temporary.join(".revision"), revision.to_string())
                .map_err(|e| e.to_string())?;
            if std::fs::symlink_metadata(&dir).is_ok() {
                std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
            }
            std::fs::rename(&temporary, &dir).map_err(|e| e.to_string())?;
            Ok(json!({"status":"success"}))
        }
        Some("read") => {
            // The scratch folder's `<folder>/index.html` for a proposal, never through a symlink.
            let folder = args["folder"].as_str().unwrap_or_default();
            let mut parts = checked(folder)?;
            parts.push("index.html");
            let scratch = base.join("sandbox");
            let root = rustix::fs::open(
                &scratch,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| "The sandbox folder is empty".to_owned())?;
            let fd = beneath(&root, parts, OFlags::RDONLY)
                .map_err(|_| format!("No index.html in sandbox folder {folder}"))?;
            let mut file = std::fs::File::from(fd);
            if !file.metadata().is_ok_and(|meta| meta.is_file()) {
                return Err(format!("{folder}/index.html is not a file"));
            }
            let mut data = Vec::new();
            (&mut file)
                .take(ELEMENT_READ + 1)
                .read_to_end(&mut data)
                .map_err(|error| error.to_string())?;
            if data.len() as u64 > ELEMENT_READ {
                return Err("index.html is larger than 512 KB".into());
            }
            let html =
                String::from_utf8(data).map_err(|_| "index.html is not UTF-8 text".to_owned())?;
            Ok(json!({"status":"success","output":{"html":html}}))
        }
        Some("remove") => {
            let path = args["path"]
                .as_str()
                .and_then(mirror_path)
                .ok_or("Invalid element path")?;
            let dir = saved.join(path.0).join(path.1);
            if std::fs::symlink_metadata(&dir).is_ok() {
                std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
            }
            Ok(json!({"status":"success"}))
        }
        _ => Err("action must be status, put, read or remove".into()),
    }
}

fn admit(roots: &Roots, request: &Value) -> Result<Admitted, String> {
    let limits = Limits::from_request(request)?;
    let (base, name, args) = files::admit(roots, request).map_err(|f| f.to_string())?;
    let scratch = base.join("sandbox");
    match std::fs::DirBuilder::new().mode(0o770).create(&scratch) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        result => result.map_err(|error| error.to_string())?,
    }
    let meta = std::fs::symlink_metadata(&scratch).map_err(|error| error.to_string())?;
    if !meta.is_dir() {
        return Err("Invalid sandbox folder".into());
    }
    let used = folder_bytes(&scratch);
    Ok((scratch, name.to_owned(), args.clone(), limits, used))
}

async fn command(
    runner: &[String],
    scratch: &Path,
    name: &str,
    args: &Value,
    limits: &Limits,
    used: u64,
) -> Value {
    let (key, prefix): (&str, &[&str]) = if name == "sandbox_shell" {
        ("command", &["/bin/sh", "-c"])
    } else {
        ("code", &["/usr/bin/python3", "-I", "-c"])
    };
    let Some(text) = args.get(key).and_then(Value::as_str) else {
        return json!({"status":"failed","output":format!("{key} must be text")});
    };
    let timeout = match args.get("timeout_seconds") {
        None | Some(Value::Null) => limits.timeout_seconds,
        Some(value) => match value.as_u64() {
            Some(seconds) => seconds.clamp(1, limits.timeout_seconds),
            None => {
                return json!({"status":"failed","output":"timeout_seconds must be a whole number"})
            }
        },
    };
    let cap = limits.cap(used);
    if used >= cap {
        return json!({"status":"failed","output":full_message(used, cap)});
    }
    let confinement = Confinement {
        cpu_seconds: timeout,
        memory_bytes: limits.memory_mb * MIB,
        processes: limits.processes,
        file_bytes: limits.file_mb * MIB,
        open_files: limits.open_files,
        folder_cap_bytes: cap,
    };
    let confinement = confinement.to_json();
    let argv: Vec<String> = prefix
        .iter()
        .map(|part| (*part).to_owned())
        .chain([text.to_owned()])
        .collect();
    crate::commands::run(
        runner,
        &argv,
        scratch,
        &[],
        timeout,
        false,
        Some(&confinement),
    )
    .await
    .unwrap_or_else(|error| json!({"status":"failed","output":error.to_string()}))
}

fn full_message(used: u64, cap: u64) -> String {
    format!(
        "Sandbox folder is full: {} of {} MB used. Delete files with sandbox_file or sandbox_shell first.",
        used.div_ceil(MIB),
        cap / MIB
    )
}

/// The `sandbox_file` actions: `read`, `write`, `list` and `delete`, each resolved beneath
/// the scratch folder without following any symlink.
fn file(scratch: &Path, args: &Value, limits: &Limits, used: u64) -> Result<Value, String> {
    let action = args["action"].as_str().unwrap_or_default();
    let requested = args["path"].as_str().unwrap_or_default();
    // `saved/...` reads the agent's saved elements, which are read-only here.
    let (folder, path) = match requested
        .strip_prefix("saved")
        .filter(|rest| rest.is_empty() || rest.starts_with('/'))
    {
        Some(rest) => {
            if !matches!(action, "read" | "list") {
                return Err("saved/ holds the agent's saved elements and is read-only".into());
            }
            (saved_root(scratch), rest.trim_start_matches('/'))
        }
        None => (scratch.to_path_buf(), requested),
    };
    if !folder.is_dir() {
        return Err("No saved elements yet".into());
    }
    let root = rustix::fs::open(
        &folder,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| error.to_string())?;
    let os = |error: rustix::io::Errno| -> String {
        match error {
            rustix::io::Errno::NOENT => format!("Not found: {path}"),
            rustix::io::Errno::LOOP | rustix::io::Errno::XDEV => {
                format!("Refused: {path} leaves the sandbox or is a symlink")
            }
            other => format!("{path}: {other}"),
        }
    };
    match action {
        "list" => {
            let dir = if path.is_empty() {
                root
            } else {
                beneath(&root, checked(path)?, OFlags::RDONLY | OFlags::DIRECTORY).map_err(os)?
            };
            let mut entries = Vec::new();
            let mut more = false;
            for entry in rustix::fs::Dir::read_from(&dir).map_err(os)? {
                let entry = entry.map_err(os)?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if name == "." || name == ".." {
                    continue;
                }
                if entries.len() == LIST_LIMIT {
                    more = true;
                    break;
                }
                let info = rustix::fs::statat(&dir, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(os)?;
                let kind = match rustix::fs::FileType::from_raw_mode(info.st_mode) {
                    rustix::fs::FileType::Directory => "folder",
                    rustix::fs::FileType::RegularFile => "file",
                    rustix::fs::FileType::Symlink => "symlink",
                    _ => "other",
                };
                entries.push(json!({"name":name,"kind":kind,"bytes":info.st_size}));
            }
            entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            Ok(
                json!({"status":"success","output":{"path":path,"entries":entries,"truncated":more},
                "folder_bytes":used}),
            )
        }
        "read" => {
            let fd = beneath(&root, checked(path)?, OFlags::RDONLY).map_err(os)?;
            let mut file = std::fs::File::from(fd);
            if !file.metadata().map_err(|e| e.to_string())?.is_file() {
                return Err(format!("{path} is not a file"));
            }
            let limit = limits.read_kb * 1024;
            let mut data = Vec::new();
            (&mut file)
                .take(limit + 1)
                .read_to_end(&mut data)
                .map_err(|error| error.to_string())?;
            let truncated = data.len() as u64 > limit;
            data.truncate(limit as usize);
            let text = String::from_utf8(data)
                .map_err(|_| format!("{path} is not UTF-8 text; inspect it with sandbox_shell"))?;
            Ok(json!({"status":"success","output":text,"truncated":truncated}))
        }
        "write" => {
            let content = args["content"]
                .as_str()
                .ok_or_else(|| "content must be text".to_owned())?;
            let parts = checked(path)?;
            let (name, folders) = parts.split_last().ok_or("path required")?;
            let size = content.len() as u64;
            if size > limits.file_mb * MIB {
                return Err(format!(
                    "A sandbox file holds at most {} MB",
                    limits.file_mb
                ));
            }
            let mut dir = root;
            for folder in folders {
                match rustix::fs::mkdirat(&dir, *folder, Mode::from_raw_mode(0o770)) {
                    Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                    Err(error) => return Err(os(error)),
                }
                dir =
                    beneath(&dir, vec![*folder], OFlags::RDONLY | OFlags::DIRECTORY).map_err(os)?;
            }
            let old = match rustix::fs::statat(&dir, *name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(info) if rustix::fs::FileType::from_raw_mode(info.st_mode).is_file() => {
                    info.st_size as u64
                }
                Ok(_) => return Err(format!("{path} exists and is not a file")),
                Err(rustix::io::Errno::NOENT) => 0,
                Err(error) => return Err(os(error)),
            };
            let after = used.saturating_sub(old).saturating_add(size + ENTRY_BYTES);
            if after > limits.cap(used) && size > old {
                return Err(full_message(after, limits.cap(used)));
            }
            let fd = rustix::fs::openat(
                &dir,
                *name,
                OFlags::WRONLY
                    | OFlags::CREATE
                    | OFlags::TRUNC
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o660),
            )
            .map_err(os)?;
            let mut file = std::fs::File::from(fd);
            file.write_all(content.as_bytes())
                .map_err(|error| error.to_string())?;
            Ok(json!({"status":"success","output":format!("Wrote {size} bytes to {path}")}))
        }
        "delete" => {
            let parts = checked(path)?;
            let (name, folders) = parts.split_last().ok_or("path required")?;
            let dir = if folders.is_empty() {
                root
            } else {
                beneath(&root, folders.to_vec(), OFlags::RDONLY | OFlags::DIRECTORY).map_err(os)?
            };
            let info = rustix::fs::statat(&dir, *name, AtFlags::SYMLINK_NOFOLLOW).map_err(os)?;
            let flags = if rustix::fs::FileType::from_raw_mode(info.st_mode).is_dir() {
                AtFlags::REMOVEDIR
            } else {
                AtFlags::empty()
            };
            rustix::fs::unlinkat(&dir, *name, flags).map_err(|error| match error {
                rustix::io::Errno::NOTEMPTY => {
                    format!("{path} is a folder that is not empty; empty it first")
                }
                other => os(other),
            })?;
            Ok(json!({"status":"success","output":format!("Deleted {path}")}))
        }
        _ => Err("action must be read, write, list or delete".into()),
    }
}

/// A relative path's parts: no absolute paths, empty, `.` or `..` parts, at most 512 bytes.
fn checked(path: &str) -> Result<Vec<&str>, String> {
    let parts: Vec<&str> = path.split('/').collect();
    if path.is_empty()
        || path.len() > 512
        || path.contains('\0')
        || parts.iter().any(|part| matches!(*part, "" | "." | ".."))
    {
        return Err("path must be relative to the sandbox, without . or .. parts".into());
    }
    Ok(parts)
}

/// Opens `parts` beneath `dir`; the kernel refuses any symlink, magic link or escape.
fn beneath(dir: &OwnedFd, parts: Vec<&str>, flags: OFlags) -> rustix::io::Result<OwnedFd> {
    rustix::fs::openat2(
        dir,
        parts.join("/"),
        flags | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
    )
}

/// The folder's size for its cap and the owner's storage: each entry's bytes plus
/// [`ENTRY_BYTES`], never following a symlink. A folder past [`MAX_ENTRIES`] counts as
/// full.
#[must_use]
pub fn folder_bytes(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut seen = 0usize;
    let mut pending = vec![root.to_path_buf()];
    while let Some(folder) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_ENTRIES {
                return u64::MAX;
            }
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            total = total
                .saturating_add(meta.size())
                .saturating_add(ENTRY_BYTES);
            if meta.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    total
}

/// Bytes per session's sandbox, as last measured; loaded from the file on first use.
static USAGE: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);

/// Writes `sandbox-usage.json` beside the port claims (group-readable by the API, which
/// counts it toward the owner's storage), replacing it atomically: `{"<session id>": bytes}`.
fn publish_usage(path: &Path, session: &str, bytes: u64) -> io::Result<()> {
    change_usage(path, |usage| {
        usage.insert(session.to_owned(), bytes);
    })
}

/// Drops the usage of every session `gone` names, for a deleted session or the startup
/// sweep of orphaned workspaces.
///
/// # Errors
/// When the file cannot be replaced.
pub fn forget_usage(path: &Path, gone: impl Fn(&str) -> bool) -> io::Result<()> {
    change_usage(path, |usage| usage.retain(|session, _| !gone(session)))
}

/// Applies `change` to the usage map and rewrites the file, holding the lock throughout so
/// concurrent calls never interleave their writes.
fn change_usage(path: &Path, change: impl FnOnce(&mut HashMap<String, u64>)) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut usage = USAGE.lock().unwrap_or_else(PoisonError::into_inner);
    let usage = usage.get_or_insert_with(|| {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    });
    change(usage);
    let body = serde_json::to_vec(&*usage)?;
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&temporary);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o640)
        .open(&temporary)?;
    file.write_all(&body)?;
    std::fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits {
        Limits::from_request(&json!({"trusted":{"sandbox":{"timeout_seconds":9999,
            "memory_mb":1,"processes":64,"file_mb":10,"open_files":256,"folder_mb":50,
            "read_kb":64,"room_bytes":1024}}}))
        .expect("limits")
    }

    #[test]
    fn limits_are_required_and_clamped_to_their_ceilings() {
        assert!(Limits::from_request(&json!({})).is_err());
        assert!(
            Limits::from_request(&json!({"trusted":{"sandbox":{"timeout_seconds":1}}})).is_err()
        );
        let limits = limits();
        assert_eq!(limits.timeout_seconds, 600);
        assert_eq!(limits.memory_mb, 64);
        // The owner's room, not the folder limit, caps this call.
        assert_eq!(limits.cap(10), 1034);
    }

    #[test]
    fn paths_stay_relative() {
        assert!(checked("a/b.html").is_ok());
        for bad in ["", "/etc/passwd", "../x", "a/../b", "a//b", "./a", "a/"] {
            assert!(checked(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn file_actions_never_follow_symlinks_out_of_the_sandbox() {
        let dir = tempfile::tempdir().expect("dir");
        let scratch = dir.path().join("sandbox");
        std::fs::create_dir(&scratch).expect("scratch");
        std::fs::write(dir.path().join("secret"), "s").expect("secret");
        std::os::unix::fs::symlink(dir.path().join("secret"), scratch.join("link")).expect("link");
        std::os::unix::fs::symlink(dir.path(), scratch.join("up")).expect("up");
        let limits = Limits {
            room_bytes: u64::MAX,
            ..limits()
        };
        let run = |args: Value| file(&scratch, &args, &limits, 0);
        assert!(run(json!({"action":"read","path":"link"})).is_err());
        assert!(run(json!({"action":"read","path":"up/secret"})).is_err());
        assert!(run(json!({"action":"write","path":"up/new","content":"x"})).is_err());
        assert!(run(json!({"action":"write","path":"link","content":"x"})).is_err());
        assert!(!dir.path().join("new").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("secret")).expect("kept"),
            "s"
        );
        assert!(
            run(json!({"action":"write","path":"site/index.html","content":"<p>hi</p>"})).is_ok()
        );
        assert_eq!(
            run(json!({"action":"read","path":"site/index.html"})).expect("read")["output"],
            "<p>hi</p>"
        );
        let listed = run(json!({"action":"list","path":""})).expect("list");
        assert_eq!(
            listed["output"]["entries"]
                .as_array()
                .expect("entries")
                .len(),
            3
        );
        // Deleting a symlink removes the link, never its target.
        assert!(run(json!({"action":"delete","path":"up"})).is_ok());
        assert!(dir.path().join("secret").exists());
        assert!(run(json!({"action":"delete","path":"site"})).is_err());
    }

    #[test]
    fn writes_stop_at_the_owners_room() {
        let dir = tempfile::tempdir().expect("dir");
        let limits = limits();
        let big = "x".repeat(2048);
        assert!(file(
            dir.path(),
            &json!({"action":"write","path":"a","content":big}),
            &limits,
            0
        )
        .expect_err("over the room")
        .starts_with("Sandbox folder is full"));
        assert!(file(
            dir.path(),
            &json!({"action":"write","path":"a","content":"ok"}),
            &limits,
            0
        )
        .is_ok());
    }
}
