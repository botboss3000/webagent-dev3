//! Typed session-workspace Git (`git_tool`, `resolve_conflict`, `commit_and_push`, the
//! repository watch), ported from the Administrator Git Control adapter. Every Git process
//! runs with fixed safety options, a scrubbed environment and the workspace Landlock
//! boundary; output is capped and passed through the credential guard. The host owns
//! permission and network credentials.
pub mod askpass;
mod conflict;
pub mod guard;
pub mod manager;
mod remote;

use conflict::resolve_conflict;
use remote::{commit_and_push, fence, network};
pub use remote::{connections, Connection};

use crate::sandbox;
use rustix::fs::{Mode, OFlags};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;

const LIMIT: usize = 65536;
/// Git hashes and scans whole files; the conflict reader keeps a smaller cap.
const GIT_SOURCE_LIMIT: u64 = 16 * 1024 * 1024;
const READS: [&str; 7] = [
    "status",
    "diff",
    "log",
    "show",
    "branch_list",
    "tag_list",
    "stash_list",
];

/// Why a Git call did not succeed.
#[derive(Debug)]
pub enum Failure {
    /// Refused before or without an effect: its kind, message and optional diagnostics.
    Rejected(&'static str, String, Option<Value>),
    /// A mutation may have taken effect.
    Uncertain(String),
}

/// What the adapter reported for an unexpected error (a spawn or sandbox failure).
fn generic() -> Failure {
    rejected("Workspace Git rejected")
}

fn rejected(message: impl Into<String>) -> Failure {
    Failure::Rejected("git_rejected", message.into(), None)
}

type Outcome<T> = Result<T, Failure>;

impl Failure {
    /// The tool result for this failure, as the Python adapter reported it.
    #[must_use]
    pub fn result(&self) -> Value {
        match self {
            Self::Uncertain(message) => {
                json!({"status":"error","error_kind":"git_effect_uncertain",
                "uncertain":true,"output":message})
            }
            Self::Rejected(kind, message, diagnostics) => {
                let mut result = json!({"status":"error","error_kind":kind,"output":message});
                if let Some(diagnostics) = diagnostics {
                    result["diagnostics"] = diagnostics.clone();
                }
                result
            }
        }
    }
}

/// Bounded non-empty text without NUL.
fn text(value: Option<&Value>, limit: usize) -> Outcome<&str> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty() && text.len() <= limit && !text.contains('\0'))
        .ok_or_else(|| rejected("Invalid Git text"))
}

fn components(raw: Option<&Value>) -> Outcome<Vec<&str>> {
    let raw = text(raw, 512)?;
    let parts: Vec<&str> = raw.split('/').collect();
    if raw.starts_with('/')
        || raw.contains('\\')
        || parts
            .iter()
            .any(|part| matches!(*part, "" | "." | ".." | ".git"))
    {
        return Err(rejected("Path outside workspace source"));
    }
    Ok(parts)
}

/// An exact regular file path in the repository; with `missing`, it may not exist yet.
fn source_path(base: &Path, raw: &Value, missing: bool) -> Outcome<PathBuf> {
    let mut path = base.to_owned();
    for part in components(Some(raw))? {
        path.push(part);
        if path.is_symlink() {
            return Err(rejected("Symlink paths unavailable"));
        }
    }
    if !missing && !path.exists() {
        return Err(rejected("Source path unavailable"));
    }
    if path.exists() && !path.is_file() {
        return Err(rejected("Exact regular source files required"));
    }
    Ok(path)
}

/// A Git reference name without traversal, reflog or lock forms.
fn reference(value: Option<&Value>) -> Outcome<&str> {
    let value = text(value, 200)?;
    let bytes = value.as_bytes();
    let shaped = bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(b));
    if !shaped
        || ["..", "//", "@{"].iter().any(|bad| value.contains(bad))
        || value.ends_with('/')
        || value.ends_with(".lock")
        || value.ends_with('.')
    {
        return Err(rejected("Invalid Git reference"));
    }
    Ok(value)
}

/// The workspace repository `raw` (`__root__` for the workspace itself): an ordinary
/// repository with no external object stores or worktrees.
fn repository(base: &Path, raw: &Value) -> Outcome<PathBuf> {
    let mut path = std::fs::canonicalize(base)
        .map_err(|_| rejected("Ordinary workspace repository required"))?;
    if raw.as_str() == Some("__root__") {
        if path.is_symlink() {
            return Err(rejected("Symlink repository unavailable"));
        }
    } else {
        for part in components(Some(raw))? {
            path.push(part);
            if path.is_symlink() {
                return Err(rejected("Symlink repository unavailable"));
            }
        }
    }
    let git = path.join(".git");
    if !path.is_dir() || !git.is_dir() || git.is_symlink() {
        return Err(rejected("Ordinary workspace repository required"));
    }
    for name in [
        "commondir",
        "objects/info/alternates",
        "objects/info/http-alternates",
    ] {
        let entry = git.join(name);
        if entry.exists() || entry.is_symlink() {
            return Err(rejected("External Git object/worktree access unavailable"));
        }
    }
    let config = git.join("config");
    let valid = std::fs::symlink_metadata(&config)
        .is_ok_and(|meta| meta.is_file() && meta.len() <= LIMIT as u64);
    if !valid {
        return Err(rejected("Invalid Git configuration"));
    }
    Ok(path)
}

/// How one Git process runs.
#[derive(Default)]
struct Run<'a> {
    mutable: bool,
    /// The trusted HTTPS transport's environment (askpass helper, URL, user, token).
    transport: Option<&'a [(String, String)]>,
    local: bool,
    timeout: Option<u64>,
    /// Scan the output line by line for credentials instead of returning it.
    scan_only: bool,
    /// Hashes of reviewed fixture lines the scan accepts in this exact commit and path.
    reviewed: Option<&'static [&'static str]>,
}

const SCAN_LINE_LIMIT: usize = 1024 * 1024;
const GUARDED: &str = "Outgoing history withheld by credential guard; inspect outgoing commits for credential-shaped content";

/// The outgoing-history scan of one `git show` output: removed lines are blanked, reviewed
/// fixture lines replaced, and every added line checked with the context before it.
struct Scan {
    output: Vec<u8>,
    new_line: Option<i64>,
    reviewed: Option<&'static [&'static str]>,
}

impl Scan {
    fn line(&mut self, mut line: Vec<u8>) -> Outcome<()> {
        if line.len() > SCAN_LINE_LIMIT {
            return Err(rejected("Outgoing history line exceeds scan bound"));
        }
        if let Some(start) = hunk_start(&line) {
            self.new_line = Some(start);
            if let Some(end) = line[2..]
                .windows(2)
                .position(|w| w == b"@@")
                .map(|at| at + 2)
            {
                line.truncate(end + 2);
                line.push(b'\n');
            }
        }
        let added = line.starts_with(b"+") && !line.starts_with(b"+++");
        let location = if added { self.new_line } else { None };
        if location.is_some()
            && self
                .reviewed
                .is_some_and(|reviewed| reviewed.contains(&hex_digest(&line).as_str()))
        {
            line = b"+<reviewed synthetic assertion>\n".to_vec();
        } else if line.starts_with(b"-") && !line.starts_with(b"---") {
            // Removed text is not sent in the new tree; its introducing commit is scanned.
            line = b"-<removed line>\n".to_vec();
        }
        let before = self.output.len().saturating_sub(4160);
        self.output.extend_from_slice(&line);
        // Earlier bytes were already scanned: rescan from the line boundary before the last
        // 4160 bytes of context, which covers any match ending in this line.
        let floor = self.output.len() - line.len();
        let from = self.output[..floor.min(before)]
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |at| at + 1);
        let window = &self.output[from..];
        if guard::secret(window) {
            let diagnostics = location.map(|line| json!({"line":line}));
            return Err(Failure::Rejected(
                "git_credential_guard",
                GUARDED.into(),
                diagnostics,
            ));
        }
        let mut spaces = 0;
        for byte in window {
            spaces = if matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
                spaces + 1
            } else {
                0
            };
            if spaces >= 4096 {
                return Err(rejected("Outgoing history whitespace exceeds scan bound"));
            }
        }
        if self.output.len() > LIMIT {
            let excess = self.output.len() - LIMIT;
            self.output.drain(..excess);
        }
        if let Some(number) = self.new_line.as_mut() {
            if line.starts_with(b"+") && !line.starts_with(b"+++") || line.starts_with(b" ") {
                *number += 1;
            }
        }
        Ok(())
    }
}

/// The new-file start line of a `@@ -A[,B] +C` hunk header.
fn hunk_start(line: &[u8]) -> Option<i64> {
    let rest = line.strip_prefix(b"@@ -")?;
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let mut rest = &rest[digits..];
    if let Some(after) = rest.strip_prefix(b",") {
        let count = after.iter().take_while(|b| b.is_ascii_digit()).count();
        if count == 0 {
            return None;
        }
        rest = &after[count..];
    }
    let rest = rest.strip_prefix(b" +")?;
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    std::str::from_utf8(&rest[..digits]).ok()?.parse().ok()
}

async fn command(root: &Path, repo: &Path, argv: &[&str], run: Run<'_>) -> Outcome<Vec<u8>> {
    let argv: Vec<&OsStr> = argv.iter().map(OsStr::new).collect();
    command_os(root, repo, &argv, run).await
}

/// Runs `git ARGV` in `repo` with the fixed safety options, the workspace sandbox and a
/// 64 KiB output cap, and returns its merged output (empty for a scan).
async fn command_os(root: &Path, repo: &Path, argv: &[&OsStr], run: Run<'_>) -> Outcome<Vec<u8>> {
    let safe = format!("safe.directory={}", repo.display());
    let file = format!(
        "protocol.file.allow={}",
        if run.local { "always" } else { "never" }
    );
    let options = [
        "-c",
        &safe,
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "diff.external=",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "tag.gpgsign=false",
        "-c",
        "credential.helper=",
        "-c",
        "credential.useHttpPath=true",
        "-c",
        "http.followRedirects=false",
        "-c",
        "http.sslVerify=true",
        "-c",
        "protocol.ext.allow=never",
        "-c",
        &file,
    ];
    let home = repo.parent().unwrap_or(repo);
    let mut git = tokio::process::Command::new("git");
    git.args(options)
        .args(argv)
        .current_dir(repo)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", ":")
        .env("GIT_SEQUENCE_EDITOR", ":")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut readable: Vec<PathBuf> = Vec::new();
    if let Some(transport) = run.transport {
        for (key, value) in transport {
            git.env(key, value);
            if key == "GIT_ASKPASS" {
                readable.push(PathBuf::from(value));
            }
        }
    }
    let read_paths: Vec<&Path> = readable.iter().map(PathBuf::as_path).collect();
    let boundary = sandbox::prepare(root, &read_paths, &[], false).map_err(|_| generic())?;
    // SAFETY: setsid, dup2 and the prepared Landlock calls are raw system calls on
    // memory prepared before the fork.
    unsafe {
        git.pre_exec(move || {
            // Errors join the output stream, as the adapter merged stderr into stdout.
            libc::dup2(1, 2);
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            boundary.apply()
        });
    }
    let mut child = git.spawn().map_err(|_| generic())?;
    let group = child.id().map_or(0, |pid| pid as i32);
    // A call dropped mid-run (caller hung up, workspace quiesced) ends Git's session, as the
    // adapter's cancellation did.
    let mut armed = Armed(group);
    let mut stdout = child.stdout.take().ok_or_else(generic)?;
    let mut output = Vec::new();
    let mut scan = Scan {
        output: Vec::new(),
        new_line: None,
        reviewed: run.reviewed,
    };
    let read = async {
        let mut chunk = vec![0; 8192];
        let mut pending: Vec<u8> = Vec::new();
        loop {
            let count = stdout.read(&mut chunk).await.map_err(|_| generic())?;
            if count == 0 {
                break;
            }
            if run.scan_only {
                pending.extend_from_slice(&chunk[..count]);
                while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = pending.drain(..=end).collect();
                    scan.line(line)?;
                }
                if pending.len() > SCAN_LINE_LIMIT {
                    return Err(rejected("Outgoing history line exceeds scan bound"));
                }
            } else {
                output.extend_from_slice(&chunk[..count]);
                if output.len() > LIMIT {
                    return Err(rejected("Git output exceeds limit"));
                }
            }
        }
        if run.scan_only && !pending.is_empty() {
            scan.line(pending)?;
        }
        child.wait().await.map_err(|_| generic())
    };
    let status =
        match tokio::time::timeout(Duration::from_secs(run.timeout.unwrap_or(30)), read).await {
            Ok(Ok(status)) => {
                armed.0 = 0;
                status
            }
            failed => {
                armed.0 = 0;
                stop(group).await;
                if run.mutable {
                    return Err(Failure::Uncertain(
                        "Interrupted Git effect uncertain; inspect repository before retry".into(),
                    ));
                }
                if let (true, Ok(Err(failure @ Failure::Rejected(..)))) = (run.scan_only, failed) {
                    return Err(failure);
                }
                return Err(rejected("Git read timed out or exceeded output limit"));
            }
        };
    if run.scan_only {
        output = scan.output;
    }
    let token = run
        .transport
        .and_then(|t| t.iter().find(|(key, _)| key == "V3_GIT_TOKEN"))
        .map(|(_, value)| value.as_bytes())
        .filter(|token| !token.is_empty());
    let leaked = token.is_some_and(|token| output.windows(token.len()).any(|w| w == token));
    if guard::secret(&output) || leaked {
        if run.mutable {
            return Err(Failure::Uncertain(
                "Git output withheld after possible effect; reconcile repository before retry"
                    .into(),
            ));
        }
        return Err(rejected("Git output withheld by credential guard"));
    }
    if !status.success() {
        if run.transport.is_some() && argv.first() == Some(&OsStr::new("push")) {
            let has = |needle: &[u8]| output.windows(needle.len()).any(|w| w == needle);
            if has(b"fatal: Authentication failed for ") {
                return Err(Failure::Rejected("git_auth_failed", "GitHub authentication rejected the saved repository key; update it in the repository configuration or Vault".into(), None));
            }
            if has(b"fatal: unable to access ") && has(b"The requested URL returned error: 403") {
                return Err(Failure::Rejected("git_push_forbidden", "GitHub denied push access; check the saved key's repository access and write permission".into(), None));
            }
            if has(b"remote: Repository not found.") {
                return Err(Failure::Rejected("git_repository_unavailable", "GitHub repository unavailable; check the repository URL and the saved key's access".into(), None));
            }
        }
        if run.mutable {
            return Err(Failure::Uncertain(
                "Git mutation did not finish cleanly; reconcile local and remote state before retry".into(),
            ));
        }
        return Err(rejected("Git operation rejected; inspect workspace status"));
    }
    Ok(if run.scan_only { Vec::new() } else { output })
}

/// Ends a still-running Git process group when dropped: SIGTERM, then SIGKILL two
/// seconds later. Set to 0 once Git has finished or been stopped.
struct Armed(i32);

impl Drop for Armed {
    fn drop(&mut self) {
        let group = self.0;
        if group > 0 {
            std::thread::spawn(move || {
                // SAFETY: killpg takes plain integers; a vanished group is ignored.
                unsafe {
                    libc::killpg(group, libc::SIGTERM);
                }
                std::thread::sleep(Duration::from_secs(2));
                // SAFETY: as above.
                unsafe {
                    libc::killpg(group, libc::SIGKILL);
                }
            });
        }
    }
}

/// Ends a Git process group: SIGTERM, then SIGKILL after two seconds.
async fn stop(group: i32) {
    // SAFETY: killpg takes plain integers.
    unsafe {
        libc::killpg(group, libc::SIGTERM);
    }
    for _ in 0..20 {
        // SAFETY: signal 0 only checks the group still exists.
        if unsafe { libc::killpg(group, 0) } != 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // SAFETY: as above.
    unsafe {
        libc::killpg(group, libc::SIGKILL);
    }
}

/// Whether a configuration key could run programs, reach other repositories or change
/// transport (as the adapter's prefix list and `remote\..*\.NAME$`/`merge\..*\.driver$`;
/// the middle may be empty).
fn unsafe_key(key: &str) -> bool {
    let prefixes = [
        "include.",
        "includeif.",
        "filter.",
        "credential.",
        "url.",
        "http.",
        "https.",
    ];
    let named = [
        "core.sshcommand",
        "core.gitproxy",
        "core.worktree",
        "extensions.worktreeconfig",
        "extensions.partialclone",
    ];
    let remote_key = key.strip_prefix("remote.").is_some_and(|rest| {
        [
            ".uploadpack",
            ".receivepack",
            ".vcs",
            ".promisor",
            ".proxy",
            ".pushurl",
        ]
        .iter()
        .any(|suffix| rest.ends_with(suffix))
    });
    let merge_driver = key
        .strip_prefix("merge.")
        .is_some_and(|rest| rest.ends_with(".driver"));
    prefixes.iter().any(|prefix| key.starts_with(prefix))
        || named.contains(&key)
        || remote_key
        || merge_driver
}

/// Refuses linked or oversized (over 10,001 entries) Git metadata under `metadata`.
fn metadata_bounded(metadata: &Path) -> Outcome<()> {
    let refused = || rejected("Unsafe or oversized Git metadata");
    let mut count = 0usize;
    let mut stack = vec![metadata.to_owned()];
    while let Some(folder) = stack.pop() {
        for entry in std::fs::read_dir(&folder).map_err(|_| refused())?.flatten() {
            count += 1;
            let kind = entry.file_type().map_err(|_| refused())?;
            if count > 10_001 || kind.is_symlink() {
                return Err(refused());
            }
            if kind.is_dir() {
                stack.push(entry.path());
            }
        }
    }
    Ok(())
}

/// Refuses repository configuration that could run programs, reach other repositories or
/// change transport, and oversized or linked metadata.
async fn validate(root: &Path, repo: &Path) -> Outcome<()> {
    let listed = command(
        root,
        repo,
        &["config", "--local", "--null", "--list"],
        Run::default(),
    )
    .await?;
    for item in listed.split(|b| *b == 0) {
        let key = String::from_utf8_lossy(item.split(|b| *b == b'\n').next().unwrap_or_default())
            .to_lowercase();
        if key.is_empty() {
            continue;
        }
        if unsafe_key(&key) || key == "core.bare" && !item.ends_with(b"\nfalse") {
            return Err(rejected("Unsafe Git configuration"));
        }
    }
    metadata_bounded(&repo.join(".git"))
}

async fn head(root: &Path, repo: &Path) -> Outcome<String> {
    let output = command(
        root,
        repo,
        &["rev-parse", "--verify", "HEAD"],
        Run::default(),
    )
    .await?;
    Ok(String::from_utf8_lossy(&output).trim().to_owned())
}

fn hex_digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// `json.dumps(value, sort_keys=True)` with Python's default separators and ASCII escapes,
/// so digests match the adapter's.
fn python_json(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => {
            let mut out = String::from('"');
            for unit in text.encode_utf16() {
                match unit {
                    0x22 => out.push_str("\\\""),
                    0x5c => out.push_str("\\\\"),
                    0x0a => out.push_str("\\n"),
                    0x0d => out.push_str("\\r"),
                    0x09 => out.push_str("\\t"),
                    0x08 => out.push_str("\\b"),
                    0x0c => out.push_str("\\f"),
                    0x20..=0x7e => out.push(char::from(unit as u8)),
                    _ => out.push_str(&format!("\\u{unit:04x}")),
                }
            }
            out.push('"');
            out
        }
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(python_json).collect::<Vec<_>>().join(", ")
        ),
        // serde_json maps are sorted by key, as `sort_keys=True` sorts them.
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(key, value)| format!("{}: {}", python_json(&json!(key)), python_json(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Opens `component` under `parent` as a directory without following links.
fn open_directory(parent: &OwnedFd, component: &str, failure: &str) -> Outcome<OwnedFd> {
    rustix::fs::openat(
        parent,
        component,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| rejected(failure))
}

/// The directory holding repository file `raw`, reached from the workspace root one
/// component at a time without following links, and the file's name.
fn source_parent(root: &Path, repo: &Path, raw: &Value) -> Outcome<(OwnedFd, String)> {
    let parts = components(Some(raw))?;
    let relative = repo
        .strip_prefix(root)
        .map_err(|_| rejected("Repository escaped current workspace"))?;
    let mut directory = rustix::fs::open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| rejected("Workspace directory replaced"))?;
    let relative: Vec<String> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    for component in relative
        .iter()
        .map(String::as_str)
        .chain(parts[..parts.len() - 1].iter().copied())
    {
        if matches!(component, "" | "." | "..") || component.contains('/') {
            return Err(rejected("Invalid source directory"));
        }
        directory = open_directory(
            &directory,
            component,
            "Source directory unavailable or replaced",
        )?;
    }
    Ok((directory, parts[parts.len() - 1].to_owned()))
}

/// Reads one regular file under `parent` whose identity does not change during the read.
fn read_at(
    parent: &OwnedFd,
    name: &str,
    missing: bool,
    max_bytes: u64,
) -> Outcome<Option<(Vec<u8>, u32)>> {
    let file = match rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(file) => file,
        Err(rustix::io::Errno::NOENT) if missing => return Ok(None),
        Err(rustix::io::Errno::NOENT) => return Err(rejected("Source file missing")),
        Err(_) => return Err(rejected("Source file replaced or unavailable")),
    };
    let before = rustix::fs::fstat(&file).map_err(|_| generic())?;
    if rustix::fs::FileType::from_raw_mode(before.st_mode) != rustix::fs::FileType::RegularFile {
        return Err(rejected("Regular source file required"));
    }
    if before.st_size as u64 > max_bytes {
        return Err(rejected(format!(
            "Source file exceeds {} MiB read limit",
            max_bytes / 1_048_576
        )));
    }
    let mut data = Vec::new();
    let mut file = std::fs::File::from(file);
    (&mut file)
        .take(max_bytes + 1)
        .read_to_end(&mut data)
        .map_err(|_| generic())?;
    if data.len() as u64 > max_bytes {
        return Err(rejected("Source file exceeds read bound"));
    }
    let after = rustix::fs::fstat(&file).map_err(|_| generic())?;
    let identity = |s: &rustix::fs::Stat| {
        (
            s.st_ino,
            s.st_size,
            s.st_mtime,
            s.st_mtime_nsec,
            s.st_ctime,
            s.st_ctime_nsec,
        )
    };
    if identity(&before) != identity(&after) {
        return Err(rejected("Source changed during read"));
    }
    Ok(Some((data, before.st_mode & 0o777)))
}

/// Reads repository file `raw`; refusals name the path, as the adapter's did.
fn read_source(
    root: &Path,
    repo: &Path,
    raw: &Value,
    missing: bool,
    max_bytes: u64,
) -> Outcome<Option<(Vec<u8>, u32)>> {
    let (parent, name) = source_parent(root, repo, raw)?;
    read_at(&parent, &name, missing, max_bytes).map_err(|failure| match failure {
        Failure::Rejected(kind, message, diagnostics) => Failure::Rejected(
            kind,
            format!("{}: {message}", raw.as_str().unwrap_or_default()),
            diagnostics,
        ),
        other => other,
    })
}

/// The digest a commit must be reviewed against: HEAD, porcelain status, the diff against
/// HEAD and every untracked file's content.
async fn review_digest(root: &Path, repo: &Path) -> Outcome<String> {
    let reads = |argv: &'static [&'static str]| command(root, repo, argv, Run::default());
    let mut parts = vec![
        json!(head(root, repo).await?),
        json!(hex::encode(
            reads(&["status", "--porcelain=v1", "-z"]).await?
        )),
        json!(hex::encode(
            reads(&["diff", "--no-ext-diff", "--no-textconv", "HEAD", "--"]).await?
        )),
    ];
    let untracked = reads(&["ls-files", "--others", "--exclude-standard", "-z"]).await?;
    let mut names: Vec<&[u8]> = untracked
        .split(|b| *b == 0)
        .filter(|n| !n.is_empty())
        .collect();
    names.sort_unstable();
    for raw in names {
        let name = std::str::from_utf8(raw).map_err(|_| generic())?;
        let (data, _) =
            read_source(root, repo, &json!(name), false, GIT_SOURCE_LIMIT)?.ok_or_else(generic)?;
        parts.push(json!([name, hex_digest(&data)]));
    }
    Ok(hex_digest(python_json(&Value::Array(parts)).as_bytes()))
}

/// HEAD, porcelain status, review digest and how far HEAD is ahead of its upstream (or of
/// every remote when there is none).
async fn snapshot(root: &Path, repo_name: &Value) -> Outcome<Value> {
    let repo = repository(root, repo_name)?;
    validate(root, &repo).await?;
    let head = head(root, &repo).await?;
    let status = command(root, &repo, &["status", "--porcelain=v1"], Run::default()).await?;
    let status = String::from_utf8(status).map_err(|_| generic())?;
    let review = review_digest(root, &repo).await?;
    let count = |output: Vec<u8>| -> Outcome<i64> {
        String::from_utf8_lossy(&output)
            .trim()
            .parse()
            .map_err(|_| generic())
    };
    let (ahead, upstream) = match command(
        root,
        &repo,
        &["rev-list", "--count", "@{upstream}..HEAD"],
        Run::default(),
    )
    .await
    {
        Ok(output) => (count(output)?, true),
        Err(Failure::Rejected(..)) => {
            let output = command(
                root,
                &repo,
                &["rev-list", "--count", "HEAD", "--not", "--remotes"],
                Run::default(),
            )
            .await?;
            (count(output)?, false)
        }
        Err(other) => return Err(other),
    };
    Ok(
        json!({"head":head,"status":status,"review_sha256":review,"ahead":ahead,"has_upstream":upstream}),
    )
}

/// The repository watch's notice: a digest of the snapshot, and whether it newly shows
/// uncommitted or unpushed work.
#[must_use]
pub fn notice(value: &Value, previous: Option<&str>) -> Value {
    let digest = hex_digest(python_json(value).as_bytes());
    let dirty = value["status"].as_str().is_some_and(|s| !s.is_empty());
    let ahead = value["ahead"].as_i64().unwrap_or(0) != 0;
    json!({"snapshot_sha256":digest,"notify":Some(digest.as_str()) != previous && (dirty || ahead),
        "dirty":dirty,"head":value["head"],"ahead":value.get("ahead")})
}

const GIT_TOOL_KEYS: [&str; 10] = [
    "operation",
    "repo",
    "paths",
    "ref",
    "name",
    "message",
    "remote",
    "limit",
    "cached",
    "expected_head",
];

/// The tools this module runs.
pub const TOOLS: [&str; 4] = [
    "git_tool",
    "resolve_conflict",
    "commit_and_push",
    "administrator_git_watch",
];

/// Whether this module runs `name`.
#[must_use]
pub fn handles(name: &str) -> bool {
    TOOLS.contains(&name)
}

/// The arguments that apply to `operation`; empty defaults of the others are dropped and
/// any other value is refused.
fn operation_arguments(args: &Map<String, Value>, operation: &str) -> Outcome<Map<String, Value>> {
    let applicable: &[&str] = match operation {
        "diff" => &["paths", "cached"],
        "log" => &["paths", "ref", "limit"],
        "show" | "checkout" => &["paths", "ref"],
        "branch_create" | "tag_create" => &["name", "ref"],
        "branch_delete" | "tag_delete" => &["name"],
        "merge" => &["ref"],
        "stash_push" => &["message", "paths"],
        "add" | "reset" => &["paths"],
        "fetch" | "pull" | "push" => &["remote"],
        _ => &[],
    };
    let allowed = |key: &str| {
        ["operation", "repo", "expected_head"].contains(&key) || applicable.contains(&key)
    };
    let mut result = Map::new();
    for (key, value) in args {
        let inert = matches!(value, Value::Null)
            || value.as_str() == Some("")
            || key == "paths" && value.as_array().is_some_and(Vec::is_empty)
            || key == "cached" && value == &json!(false)
            || key == "limit"
                && value
                    .as_i64()
                    .is_some_and(|limit| (1..=100).contains(&limit));
        if !allowed(key) {
            if !inert {
                return Err(rejected("Arguments do not apply to this Git operation"));
            }
            continue;
        }
        result.insert(key.clone(), value.clone());
    }
    for key in ["repo", "remote"] {
        if result
            .get(key)
            .is_some_and(|v| v.is_null() || v.as_str() == Some(""))
        {
            result.remove(key);
        }
    }
    let empty = |v: &Value| v.is_null() || v.as_str() == Some("");
    if matches!(operation, "log" | "show" | "branch_create" | "tag_create")
        && result.get("ref").is_some_and(empty)
    {
        result.remove("ref");
    }
    if operation == "stash_push" && result.get("message").is_some_and(empty) {
        result.remove("message");
    }
    Ok(result)
}

/// 1 to `max` unique exact paths in the repository.
fn paths(repo: &Path, values: &Value, max: usize) -> Outcome<Vec<String>> {
    let refused = || rejected(format!("Specify 1 to {max} unique exact paths"));
    let values = values
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= max)
        .ok_or_else(refused)?;
    let mut out: Vec<String> = Vec::new();
    for value in values {
        let text = value.as_str().ok_or_else(refused)?;
        if out.iter().any(|seen| seen == text) {
            return Err(refused());
        }
        out.push(text.to_owned());
    }
    for value in values {
        source_path(repo, value, true)?;
    }
    Ok(out)
}

const OPERATIONS: [&str; 20] = [
    "status",
    "diff",
    "log",
    "show",
    "branch_list",
    "tag_list",
    "stash_list",
    "branch_create",
    "branch_delete",
    "tag_create",
    "tag_delete",
    "checkout",
    "merge",
    "stash_push",
    "stash_pop",
    "add",
    "reset",
    "fetch",
    "pull",
    "push",
];

async fn git_tool(root: &Path, args: &Value, granted: &[Connection]) -> Outcome<Value> {
    let Value::Object(args) = args else {
        return Err(rejected("Invalid typed Git arguments"));
    };
    exact(args, &GIT_TOOL_KEYS, &["operation"])?;
    let operation = args["operation"].as_str().unwrap_or_default().to_owned();
    if !OPERATIONS.contains(&operation.as_str()) {
        return Err(rejected("Unsupported typed Git operation"));
    }
    let read_only = READS.contains(&operation.as_str());
    let args = operation_arguments(args, &operation)?;
    let repo_name = args.get("repo").cloned().unwrap_or(json!("webagent-v3"));
    let repo = repository(root, &repo_name)?;
    validate(root, &repo).await?;
    if !read_only {
        fence(root, &repo, args.get("expected_head")).await?;
    }
    let listed = match args.get("paths") {
        Some(value) if !value.is_null() && value.as_array().is_none_or(|v| !v.is_empty()) => {
            paths(&repo, value, 64)?
        }
        _ => Vec::new(),
    };
    let count = args
        .get("limit")
        .map_or(Some(20), Value::as_i64)
        .filter(|c| (1..=100).contains(c));
    let cached = match args.get("cached") {
        None => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return Err(rejected("Invalid Git option")),
    };
    let Some(count) = count else {
        return Err(rejected("Invalid Git option"));
    };
    if operation == "status" {
        let mut value = snapshot(root, &repo_name).await?;
        value["notice"] = notice(&value, None);
        return Ok(value);
    }
    if matches!(operation.as_str(), "fetch" | "pull" | "push") {
        let remote = reference(Some(args.get("remote").unwrap_or(&json!("origin"))))?.to_owned();
        let output = network(root, &repo, &remote, &operation, granted).await?;
        return Ok(json!({"text":String::from_utf8(output).map_err(|_| generic())?}));
    }
    let name = if matches!(
        operation.as_str(),
        "branch_create" | "branch_delete" | "tag_create" | "tag_delete"
    ) {
        reference(args.get("name"))?.to_owned()
    } else {
        "main".to_owned()
    };
    let head_ref = json!("HEAD");
    let target = if matches!(operation.as_str(), "checkout" | "merge") {
        reference(args.get("ref"))?
    } else {
        reference(Some(args.get("ref").unwrap_or(&head_ref)))?
    }
    .to_owned();
    let message = text(
        Some(
            args.get("message")
                .unwrap_or(&json!("Workspace checkpoint")),
        ),
        2000,
    )?
    .to_owned();
    let limit = format!("-{count}");
    let mut argv: Vec<&str> = match operation.as_str() {
        "diff" if cached => vec!["diff", "--no-ext-diff", "--no-textconv", "--cached", "--"],
        "diff" => vec!["diff", "--no-ext-diff", "--no-textconv", "--"],
        "log" => vec![
            "log",
            "--no-show-signature",
            &limit,
            "--format=%H %s",
            &target,
            "--",
        ],
        "show" => vec![
            "show",
            "--no-ext-diff",
            "--no-textconv",
            "--no-show-signature",
            &target,
            "--",
        ],
        "branch_list" => vec!["branch", "--list"],
        "branch_create" => vec!["branch", &name, &target],
        "branch_delete" => vec!["branch", "-d", &name],
        "tag_list" => vec!["tag", "--list"],
        "tag_create" => vec!["tag", &name, &target],
        "tag_delete" => vec!["tag", "-d", &name],
        "checkout" if listed.is_empty() => vec!["checkout", &target],
        "checkout" => vec!["checkout", &target, "--"],
        "merge" => vec!["merge", "--no-edit", "--no-ff", &target],
        "stash_list" => vec!["stash", "list"],
        "stash_push" => vec!["stash", "push", "-m", &message, "--"],
        "stash_pop" => vec!["stash", "pop"],
        "add" => vec!["add", "--"],
        _ => vec!["reset", "HEAD", "--"],
    };
    if argv.last() == Some(&"--") {
        argv.extend(listed.iter().map(String::as_str));
    }
    if matches!(operation.as_str(), "add" | "reset") && listed.is_empty() {
        return Err(rejected("Exact paths required"));
    }
    let run = Run {
        mutable: !read_only,
        ..Run::default()
    };
    let output = command(root, &repo, &argv, run).await?;
    Ok(json!({"text":String::from_utf8(output).map_err(|_| generic())?}))
}

/// Runs one Git Control tool in the admitted, canonical session workspace `root`, with the
/// host's trusted repository connections, checked first as the Python transport did.
pub async fn execute(root: &Path, name: &str, args: &Value, trusted: &Value) -> Value {
    let granted = match connections(trusted) {
        Ok(granted) => granted,
        Err(failure) => return failure.result(),
    };
    let outcome = match (name, args) {
        ("git_tool", _) => git_tool(root, args, &granted).await,
        ("resolve_conflict", Value::Object(args)) => resolve_conflict(root, args).await,
        ("commit_and_push", Value::Object(args)) => commit_and_push(root, args, &granted).await,
        ("resolve_conflict" | "commit_and_push", _) => Err(rejected("Invalid typed Git arguments")),
        _ => Err(rejected("Unknown Git Control tool")),
    };
    match outcome {
        Ok(output) => json!({"status":"success","output":output}),
        Err(failure) => failure.result(),
    }
}

/// The repository watch: the default repository's notice, or the plain failure text
/// (the worker reported the watch's errors without the tool envelope).
pub async fn watch(root: &Path) -> Value {
    match snapshot(root, &json!("webagent-v3")).await {
        Ok(value) => json!({"status":"success","output":notice(&value, None)}),
        Err(Failure::Rejected(_, message, _) | Failure::Uncertain(message)) => {
            json!({"status":"failed","output":message})
        }
    }
}

/// Requires `args` to use only `keys` and to include `required`.
fn exact(args: &Map<String, Value>, keys: &[&str], required: &[&str]) -> Outcome<()> {
    if args.keys().any(|key| !keys.contains(&key.as_str()))
        || required.iter().any(|key| !args.contains_key(*key))
    {
        return Err(rejected("Invalid typed Git arguments"));
    }
    Ok(())
}

/// `args` without the named keys whose value is empty text or null.
fn without_empty(args: &Map<String, Value>, keys: &[&str]) -> Map<String, Value> {
    let mut args = args.clone();
    for key in keys {
        if args
            .get(*key)
            .is_some_and(|v| v.is_null() || v.as_str() == Some(""))
        {
            args.remove(*key);
        }
    }
    args
}
