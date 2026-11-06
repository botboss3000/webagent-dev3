//! `resolve_conflict`: replaces an unresolved file with one side or both sides of every
//! conflict, publishing atomically only if the reviewed content is unchanged.
use super::{
    command, exact, generic, guard, hex_digest, read_at, read_source, rejected, repository,
    source_parent, validate, without_empty, Outcome, Run,
};
use rustix::fs::{Mode, OFlags};
use serde_json::{json, Map, Value};
use std::os::fd::OwnedFd;
use std::path::Path;

/// Atomically replaces conflict file `raw` with `data` if its content still hashes to
/// `expected`, keeping its mode; the parent directory must not be replaced meanwhile.
fn replace_source(
    root: &Path,
    repo: &Path,
    raw: &Value,
    data: &[u8],
    expected: &str,
) -> Outcome<()> {
    let (parent, name) = source_parent(root, repo, raw)?;
    let temporary = format!(".git-resolve-{}", uuid::Uuid::new_v4().simple());
    let result = (|| {
        let (original, mode) = read_at(&parent, &name, false, 1_048_576)?.ok_or_else(generic)?;
        if hex_digest(&original) != expected {
            return Err(rejected("Conflict changed during read"));
        }
        let file = rustix::fs::openat(
            &parent,
            temporary.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(mode),
        )
        .map_err(|_| generic())?;
        rustix::fs::fchmod(&file, Mode::from_raw_mode(mode)).map_err(|_| generic())?;
        let mut file = std::fs::File::from(file);
        std::io::Write::write_all(&mut file, data).map_err(|_| generic())?;
        file.sync_all().map_err(|_| generic())?;
        let (current_parent, _) = source_parent(root, repo, raw)?;
        let identity = |fd: &OwnedFd| rustix::fs::fstat(fd).map(|s| (s.st_dev, s.st_ino));
        if identity(&parent).map_err(|_| generic())?
            != identity(&current_parent).map_err(|_| generic())?
        {
            return Err(rejected("Conflict parent replaced; reload source"));
        }
        let (current, _) = read_at(&parent, &name, false, 1_048_576)?.ok_or_else(generic)?;
        if hex_digest(&current) != expected {
            return Err(rejected("Conflict changed before publication"));
        }
        rustix::fs::renameat(&parent, temporary.as_str(), &parent, name.as_str())
            .map_err(|_| generic())?;
        rustix::fs::fsync(&parent).map_err(|_| generic())
    })();
    let _ = rustix::fs::unlinkat(&parent, temporary.as_str(), rustix::fs::AtFlags::empty());
    result
}

/// `ours`/`theirs` takes that side's stage; `both` keeps both sides of every conflict.
pub(super) async fn resolve_conflict(root: &Path, args: &Map<String, Value>) -> Outcome<Value> {
    let args = without_empty(args, &["repo", "remote"]);
    exact(
        &args,
        &["repo", "path", "strategy", "expected_sha256"],
        &["path", "strategy", "expected_sha256"],
    )?;
    let repo = repository(root, args.get("repo").unwrap_or(&json!("webagent-v3")))?;
    validate(root, &repo).await?;
    let (data, _) =
        read_source(root, &repo, &args["path"], false, 1_048_576)?.ok_or_else(generic)?;
    let expected = args["expected_sha256"].as_str().unwrap_or_default();
    if hex_digest(&data) != expected {
        return Err(rejected("Conflict file changed"));
    }
    let strategy = args["strategy"].as_str().unwrap_or_default();
    if !matches!(strategy, "ours" | "theirs" | "both") {
        return Err(rejected("Invalid conflict strategy"));
    }
    let path = args["path"].as_str().unwrap_or_default();
    let unmerged = command(
        root,
        &repo,
        &["ls-files", "--unmerged", "--", path],
        Run::default(),
    )
    .await?;
    if unmerged.is_empty() {
        return Err(rejected("Path is not unresolved"));
    }
    let result = if strategy == "both" {
        merge_both(&data)?
    } else {
        let stage = format!(":{}:{path}", if strategy == "ours" { 2 } else { 3 });
        command(root, &repo, &["show", &stage], Run::default()).await?
    };
    if guard::secret(&result) {
        return Err(rejected("Conflict content contains possible credentials"));
    }
    replace_source(root, &repo, &args["path"], &result, expected)?;
    Ok(json!({"path":path,"sha256":hex_digest(&result),"staged":false}))
}

/// Lines split like Python's `bytes.splitlines(keepends=True)`: at `\n`, `\r\n` or `\r`.
fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut at = 0;
    while at < data.len() {
        let end = match data[at] {
            b'\n' => Some(at + 1),
            b'\r' if data.get(at + 1) == Some(&b'\n') => Some(at + 2),
            b'\r' => Some(at + 1),
            _ => None,
        };
        if let Some(end) = end {
            lines.push(&data[start..end]);
            start = end;
            at = end;
        } else {
            at += 1;
        }
    }
    if start < data.len() {
        lines.push(&data[start..]);
    }
    lines
}

fn merge_both(data: &[u8]) -> Outcome<Vec<u8>> {
    #[derive(PartialEq)]
    enum Mode {
        Plain,
        Ours,
        Theirs,
    }
    let (mut out, mut left, mut right) = (Vec::new(), Vec::new(), Vec::new());
    let (mut mode, mut found) = (Mode::Plain, false);
    for line in split_lines(data) {
        if line.starts_with(b"<<<<<<< ") && mode == Mode::Plain {
            mode = Mode::Ours;
            left.clear();
            right.clear();
            found = true;
        } else if line.starts_with(b"=======") && mode == Mode::Ours {
            mode = Mode::Theirs;
        } else if line.starts_with(b">>>>>>> ") && mode == Mode::Theirs {
            out.extend_from_slice(&left);
            out.extend_from_slice(&right);
            mode = Mode::Plain;
        } else if line.starts_with(b"<<<<<<< ")
            || line.starts_with(b">>>>>>> ")
            || line.starts_with(b"=======")
        {
            return Err(rejected("Malformed conflict markers"));
        } else if mode == Mode::Ours {
            left.extend_from_slice(line);
        } else if mode == Mode::Theirs {
            right.extend_from_slice(line);
        } else {
            out.extend_from_slice(line);
        }
    }
    if mode != Mode::Plain || !found {
        return Err(rejected("Incomplete conflict markers"));
    }
    Ok(out)
}
