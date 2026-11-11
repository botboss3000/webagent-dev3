//! File tools run inside the worker: list, read, search, edit and write, the
//! Administrator's direct live-checkout edit, and directory-decision candidates
//! ([`decision`]). Ported from the Codebase Admin runner
//! (`plugins/abilities/Administrator/codebase_admin/runner.py`), which started a Python
//! process for every call; argument checks, limits, routing refusals and result fields are
//! unchanged. Paths are walked one component at a time from `/` with `O_NOFOLLOW`, so a
//! symlink swapped in after resolution is refused rather than followed.
use rustix::fs::{
    fchmod, flock, fstat, fsync, linkat, mkdirat, openat, renameat, statat, unlinkat, AtFlags, Dir,
    FileType, FlockOperation, Mode, OFlags, Stat, CWD,
};
use rustix::io::Errno;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod decision;

/// The tools this module runs; everything else goes to the Python worker.
pub const TOOLS: [&str; 7] = [
    "decision_candidates",
    "file_list",
    "file_read",
    "file_search",
    "file_edit",
    "file_write",
    "checkout_edit",
];
/// Reported in every result, replacing the Python runner's 1.1.0.
pub(crate) const IMPLEMENTATION_VERSION: &str = "2.0.0";
const LIMIT: usize = 1024 * 1024;
/// Matches the broker read bound so one file_read cannot flood the model context.
const READ_LIMIT: usize = 16000;
/// Runtime state and other writers' material never change through a source edit.
const CHECKOUT_PRIVATE: [&str; 9] = [
    "data",
    "workspaces",
    "target",
    ".git",
    "build-jobs",
    "deployment-secrets",
    "runtime",
    "rollback",
    "gate-runtime",
];
/// Checkout folders holding runtime state, build output or other writers' material; no
/// tool reads them. Git history stays readable.
pub(crate) const CHECKOUT_HIDDEN: [&str; 8] = [
    "data",
    "workspaces",
    "target",
    "build-jobs",
    "deployment-secrets",
    "runtime",
    "rollback",
    "gate-runtime",
];
const SOURCE_EXCLUDED: [&str; 6] = [
    "data",
    "workspaces",
    "target",
    ".git",
    "build-jobs",
    "deployment-secrets",
];
/// Top-level checkout folders a directory search of the whole checkout skips.
const SEARCH_SKIPPED: [&str; 9] = [
    "build-jobs",
    "workspaces",
    "target",
    "v2_ref",
    "data",
    "rollback",
    "runtime",
    "gate-runtime",
    "host-build-service",
];

/// The fixed places the file tools use.
#[derive(Debug, Clone)]
pub struct Roots {
    /// Per-session workspaces, one folder per session UUID.
    pub workspaces: PathBuf,
    /// The live source checkout; relative paths resolve here when it exists.
    pub live_checkout: PathBuf,
    /// `dispatch.lock` and `shutoff`, the global control files.
    pub control: PathBuf,
    /// The live context configuration holding the search output limits.
    pub context_config: PathBuf,
    /// Workspace preview-port claims, shared with the Python worker's terminals.
    pub port_claims: PathBuf,
}

impl Roots {
    /// The container layout; `V3_WORKSPACE_ROOT` overrides the workspace root.
    #[must_use]
    pub fn container() -> Self {
        let live_checkout = PathBuf::from("/code");
        Self {
            workspaces: PathBuf::from(
                std::env::var("V3_WORKSPACE_ROOT").unwrap_or_else(|_| "/workspaces".into()),
            ),
            context_config: live_checkout.join("crates/modules/context/context-compaction.json"),
            live_checkout,
            control: PathBuf::from("/runtime/control"),
            port_claims: PathBuf::from("/runtime/worker/port-claims"),
        }
    }
}

/// Why a call failed: a stable `error_kind` the review log groups on, and the text the
/// tool returns as its output.
#[derive(Debug)]
pub(crate) enum Failure {
    Rejected(&'static str, String),
    Io(io::Error),
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected(_, message) => formatter.write_str(message),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<Errno> for Failure {
    fn from(error: Errno) -> Self {
        Self::Io(error.into())
    }
}

impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        Self::Rejected("invalid_arguments", error.to_string())
    }
}

fn rejected(message: &str) -> Failure {
    Failure::Rejected("invalid_arguments", message.to_owned())
}

/// A rejection whose cause is not an argument mistake; the kind is what review groups on.
fn rejected_kind(kind: &'static str, message: &str) -> Failure {
    Failure::Rejected(kind, message.to_owned())
}

/// The kind for an OS-level failure, so a missing path is not filed as bad arguments.
fn io_kind(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::NotFound => "not_found",
        io::ErrorKind::PermissionDenied => "denied",
        io::ErrorKind::AlreadyExists => "already_exists",
        _ => "io_error",
    }
}

type Outcome<T> = Result<T, Failure>;

/// Whether this module runs `name`.
#[must_use]
pub fn handles(name: &str) -> bool {
    TOOLS.contains(&name)
}

/// Runs one worker request for a file tool and returns its result object.
#[must_use]
pub fn call(roots: &Roots, request: &Value) -> Value {
    let (base, name, args) = match admit(roots, request) {
        Ok(admitted) => admitted,
        Err(failure) => return failed(&failure),
    };
    if name == "decision_candidates" {
        // A worker tool of its own, not a Codebase Admin one: no routing or runner fields.
        return match args {
            Value::Object(args) => decision::candidates(roots, &base, args),
            _ => Err(rejected("Unsupported decision arguments")),
        }
        .unwrap_or_else(|failure| failed(&failure));
    }
    let mut result =
        match route(roots, name, args).and_then(|args| operation(roots, &base, &args, name)) {
            Ok(result) => result,
            Err(failure) => failed(&failure),
        };
    result["implementation_version"] = json!(IMPLEMENTATION_VERSION);
    result["plugin_revision"] = Value::Null;
    result
}

pub(crate) fn failed(failure: &Failure) -> Value {
    let text = failure.to_string();
    let cut = text
        .char_indices()
        .nth(4096)
        .map_or(text.len(), |(at, _)| at);
    let kind = match failure {
        Failure::Rejected(kind, _) => *kind,
        Failure::Io(error) => io_kind(error),
    };
    json!({"status":"failed","error_kind":kind,"output":&text[..cut]})
}

/// Holds the dispatch lock while checking the shutoff switch and the workspace, as the
/// Python worker does; the lock is released before any file effect.
pub(crate) fn admit<'a>(
    roots: &Roots,
    request: &'a Value,
) -> Outcome<(PathBuf, &'a str, &'a Value)> {
    let lock = File::open(roots.control.join("dispatch.lock"))?;
    flock(&lock, FlockOperation::LockExclusive)?;
    if std::fs::read_to_string(roots.control.join("shutoff"))?.trim() != "0" {
        return Err(rejected("execution disabled"));
    }
    let workspace = request
        .get("workspace")
        .ok_or_else(|| rejected("'workspace'"))?;
    let base = workspace_path(roots, workspace)?;
    let name = request["name"].as_str().unwrap_or_default();
    // An absent argument object is an empty one.
    let args = request.get("arguments").unwrap_or(&EMPTY_OBJECT);
    drop(lock);
    Ok((base, name, args))
}

static EMPTY_OBJECT: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| json!({}));

fn workspace_path(roots: &Roots, session: &Value) -> Outcome<PathBuf> {
    let session = session
        .as_str()
        .ok_or_else(|| rejected("invalid workspace identifier"))?;
    let parsed =
        uuid_text(session).ok_or_else(|| rejected("badly formed hexadecimal UUID string"))?;
    if parsed != session {
        return Err(rejected("invalid workspace identifier"));
    }
    let root = std::fs::canonicalize(&roots.workspaces)?;
    let path = root.join(session);
    match std::fs::DirBuilder::new().mode(0o770).create(&path) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => {}
        result => result?,
    }
    let meta = std::fs::symlink_metadata(&path)?;
    if meta.file_type().is_symlink() || std::fs::canonicalize(&path)?.parent() != Some(&root) {
        return Err(rejected("invalid workspace"));
    }
    Ok(path)
}

/// The canonical hyphenated lowercase form of a UUID, as Python's `str(uuid.UUID(text))`.
pub(crate) fn uuid_text(text: &str) -> Option<String> {
    let mut hex: String = text
        .trim_start_matches("urn:")
        .trim_start_matches("uuid:")
        .chars()
        .filter(|c| !matches!(c, '{' | '}' | '-'))
        .collect();
    if hex.len() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    hex.make_ascii_lowercase();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

fn is_given(args: &Map<String, Value>, key: &str) -> bool {
    args.get(key).is_some_and(|value| !value.is_null())
}

/// The runner's routing: refuses the cases that belong to the broker or are malformed,
/// and returns the arguments the file effect runs with.
fn route(roots: &Roots, name: &str, args: &Value) -> Outcome<Map<String, Value>> {
    let Value::Object(args) = args else {
        return Err(rejected("Unsupported Codebase Admin contract"));
    };
    let broker = || rejected("Only file effects execute in the plugin process");
    if is_given(args, "task_id") {
        return Err(if name == "checkout_edit" {
            rejected("Task scope unsupported")
        } else {
            broker()
        });
    }
    let present: Map<String, Value> = args
        .iter()
        .filter(|(_, value)| !value.is_null())
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let live_prefix = format!("{}/", roots.live_checkout.display());
    if name == "checkout_edit" {
        let raw = args
            .get("path")
            .and_then(Value::as_str)
            .filter(|raw| !raw.is_empty())
            .ok_or_else(|| rejected("path is required"))?;
        let relative = raw.strip_prefix(&live_prefix).unwrap_or(raw);
        let first = relative.split('/').next().unwrap_or_default();
        if relative.starts_with('/')
            || relative
                .split('/')
                .any(|part| matches!(part, "" | "." | ".."))
            || CHECKOUT_PRIVATE.contains(&first)
        {
            return Err(rejected_kind(
                "denied_path",
                "Path must be a live-checkout source path outside runtime state",
            ));
        }
        if args.contains_key("content") == args.contains_key("old_text") {
            return Err(rejected(
                "Pass content to write a file, or old_text and new_text to edit one",
            ));
        }
        let mut routed = present;
        routed.insert(
            "path".into(),
            json!(roots.live_checkout.join(relative).to_string_lossy()),
        );
        return Ok(routed);
    }
    if matches!(name, "file_edit" | "file_write") {
        if let Some(raw) = args.get("path").and_then(Value::as_str) {
            if raw.starts_with(&live_prefix) || (!raw.is_empty() && !raw.starts_with('/')) {
                let relative = raw.strip_prefix(&live_prefix).unwrap_or(raw);
                let first = relative.split('/').next().unwrap_or_default();
                if relative
                    .split('/')
                    .any(|part| matches!(part, "" | "." | ".."))
                    || SOURCE_EXCLUDED.contains(&first)
                {
                    return Err(rejected_kind(
                        "denied_path",
                        "Source path excluded; use the owning runtime/workspace API",
                    ));
                }
                return Err(broker());
            }
        }
    }
    let mut live = present;
    live.remove("task_id");
    if live.contains_key("request_id") || live.contains_key("expected_inputs_sha256") {
        return Err(rejected_kind(
            "denied_path",
            "Task write arguments require task_id; live fallback denied",
        ));
    }
    Ok(live)
}

/// `os.path.realpath(path, strict=False)`: follows the symlinks of components that exist
/// and keeps the missing tail as written, with `..` applied after resolution.
fn resolve(path: &Path) -> PathBuf {
    let mut resolved = PathBuf::from("/");
    let mut pending: Vec<OsString> = path
        .components()
        .rev()
        .map(|part| part.as_os_str().to_owned())
        .collect();
    let mut links = 0;
    while let Some(part) = pending.pop() {
        match part.as_bytes() {
            b"/" | b"" | b"." => continue,
            b".." => {
                resolved.pop();
                continue;
            }
            _ => {}
        }
        let candidate = resolved.join(&part);
        let link =
            std::fs::symlink_metadata(&candidate).is_ok_and(|meta| meta.file_type().is_symlink());
        if !link || links >= 40 {
            resolved = candidate;
            continue;
        }
        links += 1;
        match std::fs::read_link(&candidate) {
            Ok(target) => {
                if target.is_absolute() {
                    resolved = PathBuf::from("/");
                }
                pending.extend(
                    target
                        .components()
                        .rev()
                        .map(|part| part.as_os_str().to_owned()),
                );
            }
            Err(_) => resolved = candidate,
        }
    }
    resolved
}

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// An OS error worded as Python reports it, naming the path component, so agents see the
/// same text as before.
fn os_error(error: Errno, name: &OsStr) -> Failure {
    let error = io::Error::from(error);
    let kind = io_kind(&error);
    let text = error.to_string();
    let description = text.split(" (os error").next().unwrap_or_default();
    Failure::Rejected(
        kind,
        format!(
            "[Errno {}] {description}: '{}'",
            error.raw_os_error().unwrap_or_default(),
            name.to_string_lossy()
        ),
    )
}

/// `openat` relative to `directory`, with Python's error wording.
fn open_at(directory: &OwnedFd, name: &OsStr, flags: OFlags) -> Outcome<OwnedFd> {
    openat(directory, name, flags | OFlags::CLOEXEC, Mode::empty())
        .map_err(|error| os_error(error, name))
}

fn regular(stat: &Stat) -> bool {
    FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
}

fn permission_bits(stat: &Stat) -> u32 {
    stat.st_mode & 0o7777
}

/// Device, inode, modification time and size: a file is unchanged when all four match.
fn identity(stat: &Stat) -> (u64, u64, i64, i64, i64) {
    (
        stat.st_dev,
        stat.st_ino,
        stat.st_mtime,
        stat.st_mtime_nsec as i64,
        stat.st_size,
    )
}

fn read_up_to(file: &mut File, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(limit as u64).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn int_argument(args: &Map<String, Value>, key: &str, default: i64) -> Option<i64> {
    match args.get(key) {
        None => Some(default),
        Some(value) => value.as_i64(),
    }
}

/// Removes a temporary file on every exit path.
struct Temporary<'a> {
    directory: &'a OwnedFd,
    name: String,
}

impl Drop for Temporary<'_> {
    fn drop(&mut self) {
        // Already published or never created: nothing to remove.
        let _ = unlinkat(self.directory, self.name.as_str(), AtFlags::empty());
    }
}

fn temporary_name(kind: &str) -> String {
    format!(".v3-{kind}-{}", uuid::Uuid::new_v4().simple())
}

fn operation(roots: &Roots, base: &Path, args: &Map<String, Value>, name: &str) -> Outcome<Value> {
    // checkout_edit is the one direct live-checkout writer; policy can deny it alone.
    let checkout = name == "checkout_edit";
    let name = match (checkout, args.contains_key("content")) {
        (true, true) => "file_write",
        (true, false) => "file_edit",
        _ => name,
    };
    let raw = match args.get("path") {
        None => ".",
        Some(Value::String(raw)) => raw.as_str(),
        Some(_) => return Err(rejected("path must be text")),
    };
    let live = roots.live_checkout.is_dir();
    let default_root = if live { &roots.live_checkout } else { base };
    let target = resolve(&default_root.join(raw));
    let writes = matches!(name, "file_write" | "file_edit");
    if writes && live && !checkout {
        let checkout_root = std::fs::canonicalize(&roots.live_checkout)?;
        let checkout_meta = std::fs::metadata(&checkout_root)?;
        let same = |path: &Path| {
            std::fs::metadata(path).is_ok_and(|meta| {
                (meta.dev(), meta.ino()) == (checkout_meta.dev(), checkout_meta.ino())
            })
        };
        if target.starts_with(&checkout_root) || target.ancestors().skip(1).any(same) {
            return Err(rejected(
                "Source writes require broker-owned task routing; direct source effects denied",
            ));
        }
    }
    let checkout_top = confine(roots, base, &target, checkout || !writes)?;
    let parts: Vec<&OsStr> = target
        .components()
        .filter_map(|part| match part {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect();
    let mut directory = openat(CWD, "/", DIRECTORY, Mode::empty())?;
    let (leaf, folders) = match parts.split_last() {
        Some((leaf, folders)) => (*leaf, folders),
        None => (OsStr::new("."), &[][..]),
    };
    for part in folders {
        if name == "file_write" {
            match mkdirat(
                &directory,
                *part,
                Mode::from_raw_mode(if checkout { 0o2775 } else { 0o770 }),
            ) {
                Ok(()) => {
                    if checkout {
                        rustix::fs::chmodat(
                            &directory,
                            *part,
                            // Group-writable and setgid: new checkout files stay in
                            // `webagent-source` for the owner and the broker.
                            Mode::from_raw_mode(0o2775),
                            AtFlags::empty(),
                        )?;
                    }
                    fsync(&directory)?;
                }
                Err(Errno::EXIST) => {}
                Err(error) => return Err(error.into()),
            }
        }
        directory = open_at(&directory, part, DIRECTORY)?;
    }
    let mut scan_limited = false;
    let output = match name {
        "file_edit" => return edit(&directory, leaf, args),
        "file_write" => write(&directory, leaf, args, checkout)?,
        "file_read" => return read(&directory, leaf, args),
        "file_list" => return list(&directory, leaf, args),
        "file_search" => search(
            roots,
            &directory,
            leaf,
            args,
            raw,
            checkout_top,
            &mut scan_limited,
        )?,
        _ => return Err(rejected("unknown tool")),
    };
    let output = match output {
        Written::Uncertain => {
            return Ok(json!({"status":"uncertain",
                "output":"File published; durability confirmation failed"}))
        }
        Written::Text(text) => text,
    };
    let encoded = output.as_bytes();
    let cut = &encoded[..encoded.len().min(LIMIT)];
    Ok(
        json!({"status":"success","output":String::from_utf8_lossy(cut),
        "truncated":encoded.len() > LIMIT || scan_limited}),
    )
}

/// File tools reach only this session's workspace and the checkout's source files;
/// runtime state, other sessions' workspaces and the rest of the container stay out.
/// `source` allows the checkout (reads and checkout_edit). Returns whether `target` is the
/// checkout root, whose runtime folders a walk skips.
pub(super) fn confine(roots: &Roots, base: &Path, target: &Path, source: bool) -> Outcome<bool> {
    let checkout = if source && roots.live_checkout.is_dir() {
        let root = std::fs::canonicalize(&roots.live_checkout)?;
        target.strip_prefix(&root).ok().map(Path::to_path_buf)
    } else {
        None
    };
    let in_source = checkout.as_ref().is_some_and(|rest| {
        rest.components().next().is_none_or(|first| {
            !CHECKOUT_HIDDEN
                .iter()
                .any(|name| first.as_os_str() == *name)
        })
    });
    if !in_source && !target.starts_with(base) {
        return Err(rejected_kind(
            "denied_path",
            &format!(
                "Path {} is outside the readable roots: this session's workspace {} and the source checkout {}",
                target.display(),
                base.display(),
                roots.live_checkout.display()
            ),
        ));
    }
    Ok(in_source && checkout.is_some_and(|rest| rest.as_os_str().is_empty()))
}

enum Written {
    Text(String),
    Uncertain,
}

fn hex_digest(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

fn edit(directory: &OwnedFd, leaf: &OsStr, args: &Map<String, Value>) -> Outcome<Value> {
    if leaf == "." {
        return Err(rejected("path must name a file"));
    }
    let old_text = args.get("old_text").ok_or_else(|| rejected("'old_text'"))?;
    let new_text = args.get("new_text").ok_or_else(|| rejected("'new_text'"))?;
    let old_text = old_text
        .as_str()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| rejected("old_text must be nonempty text"))?;
    let new_text = new_text
        .as_str()
        .ok_or_else(|| rejected("new_text must be text"))?;
    let expected = match args.get("expected_sha256") {
        None | Some(Value::Null) => None,
        Some(Value::String(hash)) if hex_digest(hash) => Some(hash.as_str()),
        Some(_) => return Err(rejected("invalid expected_sha256")),
    };
    let mut source = File::from(open_at(
        directory,
        leaf,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK,
    )?);
    let original_stat = fstat(&source)?;
    if !regular(&original_stat) {
        return Err(rejected_kind(
            "unsupported",
            "only regular files are supported",
        ));
    }
    let original = read_up_to(&mut source, LIMIT + 1)?;
    drop(source);
    if original.len() > LIMIT {
        return Err(rejected_kind("too_large", "file content exceeds limit"));
    }
    let digest = hex::encode(Sha256::digest(&original));
    if expected.is_some_and(|hash| hash != digest) {
        return Err(rejected_kind(
            "changed_since_read",
            "file changed since inspection",
        ));
    }
    let before = old_text.as_bytes();
    let Some(at) = find(&original, before) else {
        return Err(rejected_kind(
            "ambiguous_match",
            "old_text must match exactly once",
        ));
    };
    if find(&original[at + before.len()..], before).is_some() {
        return Err(rejected_kind(
            "ambiguous_match",
            "old_text must match exactly once",
        ));
    }
    let mut updated = Vec::with_capacity(original.len() + new_text.len());
    updated.extend_from_slice(&original[..at]);
    updated.extend_from_slice(new_text.as_bytes());
    updated.extend_from_slice(&original[at + before.len()..]);
    if updated.len() > LIMIT {
        return Err(rejected_kind("too_large", "file content exceeds limit"));
    }
    let mode = permission_bits(&original_stat);
    let temporary = Temporary {
        directory,
        name: temporary_name("edit"),
    };
    let mut file = File::from(openat(
        directory,
        temporary.name.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(mode),
    )?);
    file.write_all(&updated)?;
    file.flush()?;
    fsync(&file)?;
    fchmod(&file, Mode::from_raw_mode(mode))?;
    let current = statat(directory, leaf, AtFlags::SYMLINK_NOFOLLOW)?;
    if !regular(&current) || identity(&current) != identity(&original_stat) {
        return Err(rejected_kind(
            "changed_since_read",
            "file changed since inspection",
        ));
    }
    renameat(directory, temporary.name.as_str(), directory, leaf)?;
    fsync(directory)?;
    Ok(
        json!({"status":"success","output":"edited","previous_sha256":digest,
        "sha256":hex::encode(Sha256::digest(&updated))}),
    )
}

/// The first position of `needle` in `haystack`; matches never overlap a previous one.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn write(
    directory: &OwnedFd,
    leaf: &OsStr,
    args: &Map<String, Value>,
    checkout: bool,
) -> Outcome<Written> {
    if is_given(args, "mode") {
        return Err(rejected(
            "mode applies only to broker task files; omit it to create or replace a workspace file, or use file_edit for a small in-place change",
        ));
    }
    if leaf == "." {
        return Err(rejected("path must name a file"));
    }
    let content = match (is_given(args, "delete"), args.get("content")) {
        (false, Some(Value::String(content))) => content.as_bytes(),
        _ => {
            return Err(rejected(
                "file_write needs content here; delete applies only to broker task files",
            ))
        }
    };
    if content.len() > LIMIT {
        return Err(rejected_kind("too_large", "file content exceeds limit"));
    }
    let previous = match statat(directory, leaf, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => Some(stat),
        Err(Errno::NOENT) => None,
        Err(error) => return Err(os_error(error, leaf)),
    };
    if previous.as_ref().is_some_and(|stat| !regular(stat)) {
        return Err(rejected_kind(
            "unsupported",
            "only regular files are supported",
        ));
    }
    let mode = previous
        .as_ref()
        .map_or(if checkout { 0o664 } else { 0o660 }, permission_bits);
    let temporary = Temporary {
        directory,
        name: temporary_name("write"),
    };
    let mut file = File::from(openat(
        directory,
        temporary.name.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )?);
    file.write_all(content)?;
    file.flush()?;
    fchmod(&file, Mode::from_raw_mode(mode))?;
    fsync(&file)?;
    match &previous {
        // linkat publishes atomically and refuses a concurrently created path.
        None => linkat(
            directory,
            temporary.name.as_str(),
            directory,
            leaf,
            AtFlags::empty(),
        )?,
        Some(previous) => {
            let current = statat(directory, leaf, AtFlags::SYMLINK_NOFOLLOW)?;
            if !regular(&current) || identity(&current) != identity(previous) {
                return Err(rejected_kind(
                    "changed_since_read",
                    "file changed during write",
                ));
            }
            renameat(directory, temporary.name.as_str(), directory, leaf)?;
        }
    }
    if fsync(directory).is_err() {
        return Ok(Written::Uncertain);
    }
    Ok(Written::Text("written".into()))
}

fn read(directory: &OwnedFd, leaf: &OsStr, args: &Map<String, Value>) -> Outcome<Value> {
    let offset = int_argument(args, "offset", 1)
        .filter(|offset| (1..=10_000_000).contains(offset))
        .ok_or_else(|| rejected("offset must be a positive line number"))?;
    let limit = int_argument(args, "limit", 2000)
        .filter(|limit| (1..=2000).contains(limit))
        .ok_or_else(|| rejected("limit must be between 1 and 2000 lines"))?;
    let file = File::from(open_at(
        directory,
        leaf,
        OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::RDONLY,
    )?);
    let stat = fstat(&file)?;
    if !regular(&stat) {
        return Err(rejected_kind(
            "unsupported",
            "only regular files are supported",
        ));
    }
    if stat.st_size > (32 * LIMIT) as i64 {
        return Err(rejected("file exceeds inspection limit"));
    }
    let mut reader = BufReader::new(file);
    let (mut output, mut output_size, mut total, mut size) = (Vec::new(), 0, 0_i64, 0);
    let (mut next_offset, mut truncated, mut cut_line) = (None, false, None);
    loop {
        let mut line = Vec::new();
        (&mut reader)
            .take(LIMIT as u64 + 1)
            .read_until(b'\n', &mut line)?;
        if line.is_empty() {
            break;
        }
        size += line.len();
        if size > 32 * LIMIT || line.len() > LIMIT {
            return Err(rejected("file exceeds inspection limit"));
        }
        total += 1;
        if total < offset || next_offset.is_some() {
            continue;
        }
        if total >= offset + limit || output_size + line.len() > READ_LIMIT {
            truncated = true;
            if !output.is_empty() {
                next_offset = Some(total);
                continue;
            }
            // A single line over the budget (minified JSON) is cut, not returned whole.
            line.truncate(READ_LIMIT);
            cut_line = Some(total);
        }
        output_size += line.len();
        output.extend_from_slice(&line);
    }
    let mut result = json!({"status":"success","output":String::from_utf8_lossy(&output),
        "offset":offset,"limit":limit,"total_lines":total,"next_offset":next_offset,
        "truncated":truncated,"returned_bytes":output_size,"full_bytes":size});
    if let Some(line) = cut_line {
        result["hint"] = json!(format!(
            "Line {line} was cut at {READ_LIMIT} bytes; for single-line files use file_search or python to extract fields."
        ));
    }
    Ok(result)
}

/// Entry names of an open directory, without `.` and `..`, stopping at `cap`.
fn entries(directory: &OwnedFd, cap: usize) -> Outcome<(Vec<Vec<u8>>, bool)> {
    let mut names = Vec::new();
    for entry in Dir::read_from(directory)? {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if names.len() >= cap {
            return Ok((names, true));
        }
        names.push(name.to_vec());
    }
    Ok((names, false))
}

fn list(directory: &OwnedFd, leaf: &OsStr, args: &Map<String, Value>) -> Outcome<Value> {
    let folder = open_at(directory, leaf, DIRECTORY)?;
    let (mut names, limited) = entries(&folder, 10_000)?;
    let offset = int_argument(args, "offset", 0).filter(|offset| *offset >= 0);
    let limit = int_argument(args, "limit", 100).filter(|limit| (1..=100).contains(limit));
    let (Some(offset), Some(limit)) = (offset, limit) else {
        return Err(rejected("bounded directory page required"));
    };
    names.sort();
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(names.len());
    let end = start.saturating_add(limit as usize).min(names.len());
    let selected: Vec<String> = names[start..end]
        .iter()
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect();
    let next = offset + selected.len() as i64;
    Ok(
        json!({"status":"success","output":selected.join("\n"),"offset":offset,
        "next_offset":next,"has_more":end < names.len(),"truncated":limited}),
    )
}

/// `search_output_bytes` and `search_line_bytes` from the live context configuration,
/// read on every search so a change applies without a restart.
fn search_limits(path: &Path) -> Outcome<(usize, usize)> {
    let config: Value = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => json!({}),
        Err(error) => return Err(error.into()),
    };
    let number = |key: &str, default: i64| match config.get(key) {
        None => Some(default),
        Some(value) => value.as_i64(),
    };
    match (
        number("search_output_bytes", 32768),
        number("search_line_bytes", 2048),
    ) {
        (Some(output), Some(line))
            if (4096..=262_144).contains(&output)
                && (128..=8192).contains(&line)
                && line <= output =>
        {
            Ok((output as usize, line as usize))
        }
        _ => Err(rejected("invalid context search limits")),
    }
}

/// One file_search call's budgets and findings.
struct Search<'a> {
    query: &'a str,
    regex: bool,
    comments_only: bool,
    file_glob: &'a str,
    output_bytes: usize,
    line_bytes: usize,
    found: Vec<String>,
    size: usize,
    entries_left: i64,
    bytes_left: i64,
    deadline: Instant,
    limited: bool,
    /// The search starts at the checkout root, so runtime folders there are skipped.
    checkout_top: bool,
}

fn comment_markers(label: &str) -> &'static [&'static str] {
    let suffix = Path::new(label)
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase());
    match suffix.as_deref() {
        Some("py" | "sh" | "toml" | "yaml" | "yml") => &["#"],
        Some("rs" | "js" | "ts" | "c" | "h") => &["//", "/*", "*", "*/"],
        Some("css") => &["/*", "*", "*/"],
        _ => &[],
    }
}

/// Python's `str.splitlines()` line boundaries.
fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let boundary = matches!(
            c,
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if boundary {
            lines.push(&text[start..at]);
            let mut end = at + c.len_utf8();
            if c == '\r' && chars.peek().is_some_and(|(_, next)| *next == '\n') {
                chars.next();
                end += 1;
            }
            start = end;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// Python's `fnmatch.fnmatchcase`: `*`, `?`, `[seq]` and `[!seq]`, case-sensitive, where
/// `*` also matches `/`.
fn glob_matches(name: &str, pattern: &str) -> bool {
    let name: Vec<char> = name.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    let (mut n, mut p) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pattern.len() {
            match pattern[p] {
                '*' => {
                    star = Some((p, n));
                    p += 1;
                    continue;
                }
                '?' => {
                    n += 1;
                    p += 1;
                    continue;
                }
                '[' => {
                    if let Some((matched, next)) = class_matches(&pattern, p, name[n]) {
                        if matched {
                            n += 1;
                            p = next;
                            continue;
                        }
                    } else if name[n] == '[' {
                        n += 1;
                        p += 1;
                        continue;
                    }
                }
                literal if literal == name[n] => {
                    n += 1;
                    p += 1;
                    continue;
                }
                _ => {}
            }
        }
        match star {
            Some((star_p, star_n)) => {
                p = star_p + 1;
                n = star_n + 1;
                star = Some((star_p, star_n + 1));
            }
            None => return false,
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

/// Matches `c` against the `[...]` class starting at `start`; `None` when the bracket is
/// unclosed and therefore literal, as in `fnmatch`.
fn class_matches(pattern: &[char], start: usize, c: char) -> Option<(bool, usize)> {
    let mut i = start + 1;
    let negate = pattern.get(i) == Some(&'!');
    if negate {
        i += 1;
    }
    let first = i;
    let mut matched = false;
    while i < pattern.len() && (pattern[i] != ']' || i == first) {
        if pattern.get(i + 1) == Some(&'-') && pattern.get(i + 2).is_some_and(|end| *end != ']') {
            if pattern[i] <= c && c <= pattern[i + 2] {
                matched = true;
            }
            i += 3;
        } else {
            if pattern[i] == c {
                matched = true;
            }
            i += 1;
        }
    }
    if i >= pattern.len() {
        return None;
    }
    Some((matched != negate, i + 1))
}

/// Line numbers `rg` reports for `query` in `data`, bounded by the search deadline; `None`
/// when the deadline passed first.
fn regex_lines(query: &str, data: &[u8], deadline: Instant) -> Outcome<Option<Vec<usize>>> {
    let mut child = Command::new("rg")
        .args([
            "--line-number",
            "--no-heading",
            "--color",
            "never",
            "--max-count",
            "1001",
            "--max-columns",
            "1",
            "--max-columns-preview",
            "-e",
            query,
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| rejected("rg input unavailable"))?;
    let input = data.to_vec();
    let feeder = std::thread::spawn(move || {
        // rg may stop reading early; a broken pipe is expected then.
        let _ = stdin.write_all(&input);
    });
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| rejected("rg output unavailable"))?;
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = stdout.read_to_end(&mut output);
        output
    });
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline.max(Instant::now() + Duration::from_millis(10)) {
            break None;
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        let _ = feeder.join();
        let _ = reader.join();
        return Ok(None);
    };
    let _ = feeder.join();
    let output = reader.join().unwrap_or_default();
    if !matches!(status.code(), Some(0 | 1)) {
        return Err(rejected("invalid search regex"));
    }
    Ok(Some(
        output
            .split(|byte| *byte == b'\n')
            .filter_map(|row| {
                let number = row.split(|byte| *byte == b':').next()?;
                std::str::from_utf8(number).ok()?.parse().ok()
            })
            .collect(),
    ))
}

impl Search<'_> {
    fn scan_file(&mut self, file: &OwnedFd, label: &str) -> Outcome<()> {
        if !self.file_glob.is_empty() {
            let name = Path::new(label)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !glob_matches(label, self.file_glob) && !glob_matches(&name, self.file_glob) {
                return Ok(());
            }
        }
        if self.bytes_left <= 0 {
            self.limited = true;
            return Ok(());
        }
        let mut reader = File::from(file.try_clone()?);
        let want = (LIMIT + 1).min(self.bytes_left as usize);
        let mut data = read_up_to(&mut reader, want)?;
        self.bytes_left -= data.len() as i64;
        if data.len() > LIMIT {
            self.limited = true;
            data.truncate(LIMIT);
        }
        let text = String::from_utf8_lossy(&data);
        let hits = if self.regex && !self.query.is_empty() {
            match regex_lines(self.query, &data, self.deadline)? {
                None => {
                    self.limited = true;
                    return Ok(());
                }
                Some(hits) => {
                    if hits.len() > 1000 {
                        self.limited = true;
                    }
                    Some(hits)
                }
            }
        } else {
            None
        };
        let markers = comment_markers(label);
        for (index, line) in split_lines(&text).into_iter().enumerate() {
            let index = index + 1;
            if Instant::now() >= self.deadline {
                self.limited = true;
                return Ok(());
            }
            if self.comments_only && !markers.iter().any(|m| line.trim_start().starts_with(m)) {
                continue;
            }
            if !self.query.is_empty() {
                let hit = match &hits {
                    Some(hits) => hits.contains(&index),
                    None => line.contains(self.query),
                };
                if !hit {
                    continue;
                }
            }
            let mut shown = line.to_owned();
            if line.len() > self.line_bytes {
                let start = if !self.query.is_empty() && !self.regex {
                    line.find(self.query)
                        .map_or(0, |at| line[..at].chars().count())
                        .saturating_sub(self.line_bytes / 3)
                } else {
                    0
                };
                shown = line.chars().skip(start).take(self.line_bytes).collect();
                shown.push_str(" …[line clipped]");
                self.limited = true;
            }
            let result = format!("{label}:{index}:{shown}");
            let encoded = result.len() + usize::from(!self.found.is_empty());
            if self.size + encoded > self.output_bytes || self.found.len() >= 200 {
                self.limited = true;
                return Ok(());
            }
            self.found.push(result);
            self.size += encoded;
        }
        Ok(())
    }

    fn scan_dir(&mut self, folder: &OwnedFd, prefix: &str, depth: usize) -> Outcome<()> {
        if depth > 32 {
            self.limited = true;
            return Ok(());
        }
        let cap = usize::try_from(self.entries_left.max(0)).unwrap_or(0);
        let (mut names, limited) = entries(folder, cap)?;
        self.limited |= limited;
        names.sort();
        for name in names {
            let entry = String::from_utf8_lossy(&name).into_owned();
            if depth == 0
                && self.checkout_top
                && (SEARCH_SKIPPED.contains(&entry.as_str())
                    || CHECKOUT_HIDDEN.contains(&entry.as_str()))
            {
                continue;
            }
            if self.entries_left <= 0
                || self.bytes_left <= 0
                || Instant::now() >= self.deadline
                || self.size >= self.output_bytes
            {
                self.limited = true;
                return Ok(());
            }
            self.entries_left -= 1;
            let Ok(child) = openat(
                folder,
                OsStr::from_bytes(&name),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            ) else {
                continue;
            };
            let label = if prefix.is_empty() {
                entry
            } else {
                Path::new(prefix)
                    .join(&entry)
                    .to_string_lossy()
                    .into_owned()
            };
            let stat = fstat(&child)?;
            match FileType::from_raw_mode(stat.st_mode) {
                FileType::Directory => self.scan_dir(&child, &label, depth + 1)?,
                FileType::RegularFile => self.scan_file(&child, &label)?,
                _ => {}
            }
        }
        Ok(())
    }
}

fn search(
    roots: &Roots,
    directory: &OwnedFd,
    leaf: &OsStr,
    args: &Map<String, Value>,
    raw: &str,
    checkout_top: bool,
    scan_limited: &mut bool,
) -> Outcome<Written> {
    let text = |key: &str| match args.get(key) {
        None => Some(""),
        Some(value) => value.as_str().filter(|text| text.chars().count() <= 200),
    };
    let flag = |key: &str| match args.get(key) {
        None => Some(false),
        Some(value) => value.as_bool(),
    };
    let (Some(query), Some(regex), Some(comments_only), Some(file_glob)) = (
        text("query"),
        flag("regex"),
        flag("comments_only"),
        text("file_glob"),
    ) else {
        return Err(rejected("invalid search options"));
    };
    // Agents write `a|b` meaning alternation and forget `regex`; a literal pipe search is rare.
    let regex = regex || query.contains('|');
    if query.is_empty() && !comments_only {
        return Err(rejected("query must not be empty"));
    }
    if file_glob.contains(['\\', '\0']) {
        return Err(rejected("invalid file glob"));
    }
    let target = open_at(
        directory,
        leaf,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK,
    )?;
    let (output_bytes, line_bytes) = search_limits(&roots.context_config)?;
    let mut search = Search {
        query,
        regex,
        comments_only,
        file_glob,
        output_bytes,
        line_bytes,
        found: Vec::new(),
        size: 0,
        entries_left: 10_000,
        bytes_left: (32 * LIMIT) as i64,
        deadline: Instant::now() + Duration::from_secs(2),
        limited: false,
        checkout_top,
    };
    match FileType::from_raw_mode(fstat(&target)?.st_mode) {
        FileType::RegularFile => search.scan_file(&target, raw)?,
        FileType::Directory => {
            if query.is_empty() {
                return Err(rejected("a query is required for directory search"));
            }
            let prefix = if raw == "." { "" } else { raw };
            search.scan_dir(&target, prefix, 0)?;
        }
        _ => return Err(rejected("search path must be a regular file or directory")),
    }
    *scan_limited = search.limited;
    if search.found.is_empty() && search.limited {
        // An empty page alone reads as "no matches"; say the scan stopped before covering the path.
        return Ok(Written::Text(format!(
            "No matches in the part scanned: the search stopped at its time or size budget \
             before covering {raw}. Search a narrower path (for example ui/, src/ or one crate)."
        )));
    }
    Ok(Written::Text(search.found.join("\n")))
}

#[cfg(test)]
mod tests;
