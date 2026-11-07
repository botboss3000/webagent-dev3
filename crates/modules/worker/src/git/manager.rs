//! The File Manager's Git page (`administrator_file_git`), ported from the Git Control
//! `file_manager.py`: typed Git reads and changes on the live checkout, projected into the
//! page's data. Every Git process runs with the checkout as its sandbox root.
use super::remote::fence;
use super::{
    command, generic, guard, head, paths, python_json, read_source, rejected, repository,
    source_path, validate, Failure, Outcome, Run, GIT_SOURCE_LIMIT,
};
use crate::manager::{self, Error, HIDDEN, MAX_ENTRIES, VIRTUAL};
use rustix::fs::{AtFlags, FileType};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

const ACTIONS: [&str; 10] = [
    "git_status",
    "git_graph",
    "git_commit_detail",
    "git_branches",
    "git_tree_status",
    "git_line_stats",
    "git_ignore",
    "git_tool",
    "git_commit",
    "git_settings",
];

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        match error {
            Error::Rejected(message) => rejected(message),
            Error::Io(_) => generic(),
        }
    }
}

/// Whether `name` passes the File Manager's path rules.
fn visible(name: &str) -> bool {
    manager::parts(&json!(name), false).is_ok()
}

fn excludes() -> Vec<String> {
    HIDDEN
        .iter()
        .map(|name| format!(":(exclude){name}"))
        .chain(
            [
                "build-jobs",
                "data",
                "v3/data",
                "v3/target",
                "v3/workspaces",
                "v3/runtime",
                "v3/gate-runtime",
                "v3/host-build-service",
                "v3/rollback",
            ]
            .iter()
            .map(|name| format!(":(exclude){name}")),
        )
        .collect()
}

/// `git ARGV` in the checkout repository; `pathspec` appends `-- . <excludes>`.
async fn git(root: &Path, repo: &Path, argv: &[&str], pathspec: bool) -> Outcome<Vec<u8>> {
    let excludes = excludes();
    let mut full: Vec<&str> = argv.to_vec();
    if pathspec {
        full.extend(["--", "."]);
        full.extend(excludes.iter().map(String::as_str));
    }
    command(root, repo, &full, Run::default()).await
}

async fn mutate(root: &Path, repo: &Path, argv: &[&str]) -> Outcome<Vec<u8>> {
    command(
        root,
        repo,
        argv,
        Run {
            mutable: true,
            ..Run::default()
        },
    )
    .await
}

async fn git_text(root: &Path, repo: &Path, argv: &[&str]) -> Outcome<String> {
    let output = command(root, repo, argv, Run::default()).await?;
    Ok(String::from_utf8_lossy(&output).trim().to_owned())
}

/// As [`git_text`], with a refusal read as empty output.
async fn git_optional(root: &Path, repo: &Path, argv: &[&str]) -> Outcome<String> {
    match git_text(root, repo, argv).await {
        Err(Failure::Rejected(..)) => Ok(String::new()),
        other => other,
    }
}

fn lossy(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).into_owned()
}

/// Runs one File Manager Git request on the checkout at `root`.
pub async fn action(root: &Path, args: &Value, trusted: &Value) -> Value {
    let Some(map) = args.as_object().filter(|map| {
        map.get("action")
            .and_then(Value::as_str)
            .is_some_and(|a| ACTIONS.contains(&a))
    }) else {
        return json!({"status":"error","output":"Unsupported File Manager Git action"});
    };
    let action = map["action"].as_str().unwrap_or_default();
    let repo_name = map
        .get("repo")
        .cloned()
        .unwrap_or_else(|| json!("__root__"));
    let valid = repo_name.as_str().is_some_and(|name| {
        name == "__root__"
            || !(name.starts_with('/') || name.split('/').any(|p| p == "..") || name.contains('\\'))
    });
    if !valid {
        return json!({"status":"error","output":"Invalid repository selection"});
    }
    if repo_name != "__root__" && manager::parts(&repo_name, false).is_err() {
        return json!({"status":"error","output":"Protected repository selection"});
    }
    let Ok(root) = std::fs::canonicalize(root) else {
        return Failure::from(manager::rejected("Development checkout unavailable")).result();
    };
    if action == "git_tool" {
        let mut values: Map<String, Value> = map
            .iter()
            .filter(|(key, _)| *key != "action")
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        values.insert("repo".into(), repo_name);
        return super::execute(&root, "git_tool", &Value::Object(values), trusted).await;
    }
    let result = async {
        let repo = repository(&root, &repo_name)?;
        validate(&root, &repo).await?;
        match action {
            "git_status" => status(&root, &repo, &repo_name).await,
            "git_tree_status" => tree_status(&root, &repo).await,
            "git_line_stats" => line_stats(&root, &repo).await,
            "git_ignore" => ignore(&root, &repo, &repo_name, map).await,
            "git_graph" => {
                let limit = map.get("limit").map_or(Some(80), Value::as_i64);
                match limit.filter(|limit| (1..=100).contains(limit)) {
                    Some(limit) if map.get("limit").is_none_or(Value::is_i64) => {
                        graph(&root, &repo, limit).await
                    }
                    _ => Err(rejected("Invalid graph limit")),
                }
            }
            "git_commit_detail" => detail(&root, &repo, map.get("hash")).await,
            "git_branches" => branches(&root, &repo).await,
            "git_settings" => settings(&root, &repo, map, trusted).await,
            _ => commit(&root, &repo, map).await,
        }
    }
    .await;
    match result {
        Ok(output) => json!({"status":"success","output":output}),
        Err(failure) => failure.result(),
    }
}

/// HEAD, porcelain status and the review digest a commit is checked against.
async fn live_snapshot(
    root: &Path,
    repo: &Path,
    repo_name: &Value,
) -> Outcome<(String, String, String)> {
    let head = head(root, repo).await?;
    let pathspec = repo_name == "__root__";
    let status = git(
        root,
        repo,
        &["status", "--porcelain=v1", "--untracked-files=all"],
        pathspec,
    )
    .await?;
    let status = String::from_utf8(status).map_err(|_| generic())?;
    let status = status.trim_end_matches('\n').to_owned();
    let raw = git(root, repo, &["diff", "--raw", "HEAD"], pathspec).await?;
    let mut untracked = Vec::new();
    for name in status.lines().filter_map(|line| line.strip_prefix("?? ")) {
        let digest = if visible(name) {
            match read_source(root, repo, &json!(name), false, GIT_SOURCE_LIMIT) {
                Ok(Some((data, _))) => manager::sha256(&data),
                Ok(None) => return Err(generic()),
                Err(Failure::Rejected(..)) => "unavailable".to_owned(),
                Err(other) => return Err(other),
            }
        } else {
            "unavailable".to_owned()
        };
        untracked.push(json!([name, digest]));
    }
    let review = manager::sha256(
        python_json(&json!([head, status, hex::encode(&raw), untracked])).as_bytes(),
    );
    Ok((head, status, review))
}

async fn status(root: &Path, repo: &Path, repo_name: &Value) -> Outcome<Value> {
    let (head, snapshot, review) = live_snapshot(root, repo, repo_name).await?;
    let branch = git_text(root, repo, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await?;
    let remote = git_optional(root, repo, &["remote", "get-url", "origin"]).await?;
    let upstream = format!("HEAD...refs/remotes/origin/{branch}");
    let counts = git_optional(
        root,
        repo,
        &["rev-list", "--left-right", "--count", &upstream],
    )
    .await?;
    let counts: Vec<&str> = counts.split_whitespace().collect();
    let (ahead, behind) = match counts[..] {
        [ahead, behind] => (
            ahead.parse::<i64>().map_err(|_| generic())?,
            behind.parse::<i64>().map_err(|_| generic())?,
        ),
        _ => (0, 0),
    };
    let author = git_optional(root, repo, &["config", "--local", "user.name"]).await?;
    let email = git_optional(root, repo, &["config", "--local", "user.email"]).await?;
    let (mut staged, mut unstaged, mut untracked) = (Vec::new(), Vec::new(), Vec::new());
    for line in snapshot.lines() {
        let mut chars = line.chars();
        let (Some(x), Some(y)) = (chars.next(), chars.next()) else {
            continue;
        };
        if line.chars().count() < 4 {
            continue;
        }
        let name: String = line.chars().skip(3).collect();
        if !visible(&name) {
            continue;
        }
        // Gitlinks and symlinks stay visible, but cannot enter a regular-file commit.
        let (committable, reason) = match source_path(repo, &json!(name), true) {
            Ok(source) => match std::fs::metadata(&source) {
                Ok(meta) if meta.len() > GIT_SOURCE_LIMIT => (
                    false,
                    format!(
                        "File exceeds {} MiB Git scan limit",
                        GIT_SOURCE_LIMIT / (1024 * 1024)
                    ),
                ),
                _ => (true, String::new()),
            },
            Err(_) => (
                false,
                "Submodule, directory or symlink: commit separately".to_owned(),
            ),
        };
        let entry = |flag: char| {
            json!({"path":name,"committable":committable,"skip_reason":reason,
                "flag":flag.to_string()})
        };
        if x == '?' && y == '?' {
            let mut row = entry('?');
            row["flag"] = json!("??");
            untracked.push(row);
        } else {
            if x != ' ' {
                staged.push(entry(x));
            }
            if y != ' ' {
                unstaged.push(entry(y));
            }
        }
    }
    Ok(
        json!({"branch":branch,"remote_url":remote,"has_remote":!remote.is_empty(),
        "git_user_name":author,"git_user_email":email,"ahead":ahead,"behind":behind,
        "file_count":staged.len()+unstaged.len()+untracked.len(),
        "staged":staged,"unstaged":unstaged,"untracked":untracked,
        "head":head,"review_sha256":review}),
    )
}

/// Visible names from NUL-separated Git output, at most 10000.
fn available(raw: &[u8]) -> Outcome<Vec<String>> {
    let mut values = Vec::new();
    for item in raw.split(|b| *b == 0).filter(|item| !item.is_empty()) {
        let name = lossy(item).trim_end_matches('/').to_owned();
        if !visible(&name) {
            continue;
        }
        values.push(name);
        if values.len() > 10000 {
            return Err(rejected("Git tracking view exceeds limit"));
        }
    }
    Ok(values)
}

async fn tree_status(root: &Path, repo: &Path) -> Outcome<Value> {
    let tracked = git(root, repo, &["ls-files", "--cached", "-z"], true).await?;
    let ignored = git(
        root,
        repo,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            "-z",
        ],
        true,
    )
    .await?;
    Ok(
        json!({"repo_root":VIRTUAL,"is_repo":true,"head":head(root, repo).await?,
        "tracked":available(&tracked)?,"ignored":available(&ignored)?}),
    )
}

/// Python's `bytes.splitlines()` count: lines end at `\n`, `\r` or `\r\n`.
fn line_count(data: &[u8]) -> usize {
    let mut count = 0;
    let mut at = 0;
    while at < data.len() {
        count += 1;
        match data[at..].iter().position(|b| *b == b'\n' || *b == b'\r') {
            None => break,
            Some(end) => {
                at += end + 1;
                if data[at - 1] == b'\r' && data.get(at) == Some(&b'\n') {
                    at += 1;
                }
            }
        }
    }
    count
}

async fn line_stats(root: &Path, repo: &Path) -> Outcome<Value> {
    let raw = git(
        root,
        repo,
        &["diff", "--numstat", "--no-renames", "-z", "HEAD"],
        true,
    )
    .await?;
    let mut stats = Map::new();
    for row in raw.split(|b| *b == 0) {
        let row = lossy(row);
        let columns: Vec<&str> = row.splitn(3, '\t').collect();
        let [added, removed, name] = columns[..] else {
            continue;
        };
        let digits = |text: &str| !text.is_empty() && text.chars().all(|c| c.is_ascii_digit());
        if !digits(added) || !digits(removed) || !visible(name) {
            continue;
        }
        stats.insert(
            name.to_owned(),
            json!({"added":added.parse::<u64>().map_err(|_| generic())?,
                "removed":removed.parse::<u64>().map_err(|_| generic())?}),
        );
        if stats.len() > MAX_ENTRIES {
            return Err(rejected("Git line statistics exceed limit"));
        }
    }
    let untracked = git(
        root,
        repo,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        true,
    )
    .await?;
    for item in untracked.split(|b| *b == 0).filter(|item| !item.is_empty()) {
        let name = lossy(item);
        if !visible(&name) {
            continue;
        }
        let data = match read_source(root, repo, &json!(name), false, 1024 * 1024) {
            Ok(Some((data, _))) => data,
            Err(Failure::Rejected(..)) => continue,
            Ok(None) => return Err(generic()),
            Err(other) => return Err(other),
        };
        if !data.contains(&0) {
            stats.insert(name, json!({"added":line_count(&data),"removed":0}));
        }
        if stats.len() > MAX_ENTRIES {
            return Err(rejected("Git line statistics exceed limit"));
        }
    }
    Ok(json!({"project_root":VIRTUAL,"stats":stats}))
}

/// Python's `str.splitlines(keepends=True)`.
fn split_lines_keep(text: &str) -> Vec<&str> {
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
            let mut end = at + c.len_utf8();
            if c == '\r' {
                if let Some((next, '\n')) = chars.peek() {
                    end = next + 1;
                    chars.next();
                }
            }
            lines.push(&text[start..end]);
            start = end;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

fn bare_line(line: &str) -> &str {
    line.trim_end_matches(['\r', '\n'])
}

fn simple_component(item: &str) -> bool {
    !item.is_empty()
        && item
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// A regular file or directory at checkout path `name`; `None` when it is something else.
fn kind_at(root: &Path, name: &str) -> Outcome<Option<bool>> {
    let (parent, leaf) = manager::parent_fd(root, &json!(name))?;
    let info = rustix::fs::statat(&parent, leaf.as_str(), AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| generic())?;
    Ok(match FileType::from_raw_mode(info.st_mode) {
        FileType::RegularFile => Some(false),
        FileType::Directory => Some(true),
        _ => None,
    })
}

async fn ignore(
    root: &Path,
    repo: &Path,
    repo_name: &Value,
    args: &Map<String, Value>,
) -> Outcome<Value> {
    let keys: BTreeSet<&str> = args.keys().map(String::as_str).collect();
    let wanted: BTreeSet<&str> = ["action", "repo", "path", "tracked", "expected_head"].into();
    if repo_name != "__root__" || keys != wanted {
        return Err(rejected(
            "Git tracking changes require the checkout root and current HEAD",
        ));
    }
    let (Some(name), Some(tracked)) = (args["path"].as_str(), args["tracked"].as_bool()) else {
        return Err(rejected("Invalid Git tracking request"));
    };
    manager::parts(&json!(name), false)?;
    if name == ".gitignore" || !name.split('/').all(simple_component) {
        return Err(rejected("Git tracking toggle requires a simple file path"));
    }
    fence(root, repo, args.get("expected_head")).await?;
    let is_dir = kind_at(root, name)?
        .ok_or_else(|| rejected("Only regular files and directories can change tracking"))?;
    let pattern = format!("/{name}{}", if is_dir { "/" } else { "" });
    let (current, digest) = match manager::read(root, &json!(".gitignore")) {
        Ok(old) if old["binary"] == true => {
            return Err(rejected("Git ignore file is not UTF-8 text"))
        }
        Ok(old) => (
            old["content"].as_str().unwrap_or_default().to_owned(),
            old["sha256"].as_str().unwrap_or_default().to_owned(),
        ),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            (String::new(), String::new())
        }
        Err(error) => return Err(error.into()),
    };
    let updated = if tracked {
        split_lines_keep(&current)
            .into_iter()
            .filter(|line| bare_line(line) != pattern)
            .collect::<String>()
    } else if !split_lines_keep(&current)
        .iter()
        .any(|line| bare_line(line) == pattern)
    {
        let separator = if current.is_empty() || current.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        format!("{current}{separator}{pattern}\n")
    } else {
        current.clone()
    };
    let selected: BTreeSet<Vec<u8>> = if tracked {
        let mut selected: BTreeSet<Vec<u8>> = BTreeSet::new();
        for argv in [
            &[
                "ls-files",
                "--others",
                "--exclude-standard",
                "-z",
                "--",
                name,
            ][..],
            &[
                "ls-files",
                "--others",
                "--ignored",
                "--exclude-standard",
                "-z",
                "--",
                name,
            ][..],
        ] {
            let output = command(root, repo, argv, Run::default()).await?;
            selected.extend(output.split(|b| *b == 0).map(<[u8]>::to_vec));
        }
        selected
    } else {
        let output = command(
            root,
            repo,
            &["ls-files", "--cached", "-z", "--", name],
            Run::default(),
        )
        .await?;
        output.split(|b| *b == 0).map(<[u8]>::to_vec).collect()
    };
    let mut safe: Vec<String> = Vec::new();
    for item in selected.iter().filter(|item| !item.is_empty()) {
        let relative = lossy(item);
        if relative != name && !(is_dir && relative.starts_with(&format!("{name}/"))) {
            continue;
        }
        if !matches!(kind_at(root, &relative), Ok(Some(false))) {
            continue;
        }
        safe.push(relative);
        if safe.len() > MAX_ENTRIES {
            return Err(rejected("Git tracking selection exceeds limit"));
        }
    }
    let changed = async {
        if updated != current {
            manager::write(
                root,
                &json!(".gitignore"),
                &json!(updated),
                &json!("utf-8"),
                &json!(digest),
            )?;
        }
        for batch in safe.chunks(64) {
            let mut argv: Vec<&str> = if tracked {
                vec!["add", "-f", "--"]
            } else {
                vec!["rm", "--cached", "--ignore-unmatch", "--"]
            };
            argv.extend(batch.iter().map(String::as_str));
            mutate(root, repo, &argv).await?;
        }
        tree_status(root, repo).await
    }
    .await;
    // A refused step may follow one that took effect; an uncertain one stays uncertain.
    changed.map_err(|failure| match failure {
        Failure::Rejected(..) => {
            Failure::Uncertain("Git tracking may have changed; refresh before retry".into())
        }
        uncertain => uncertain,
    })
}

fn short(hash: &str) -> String {
    hash.chars().take(7).collect()
}

async fn graph(root: &Path, repo: &Path, limit: i64) -> Outcome<Value> {
    let count = format!("-{limit}");
    let raw = git_text(
        root,
        repo,
        &[
            "log",
            "--all",
            "--date-order",
            "--format=%H%x1f%P%x1f%D%x1f%an%x1f%ar%x1f%aI%x1f%s",
            &count,
        ],
    )
    .await?;
    let head = head(root, repo).await?;
    let branch = git_text(root, repo, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await?;
    let upstream = format!("refs/remotes/origin/{branch}");
    let not_upstream = format!("^{upstream}");
    let set = |text: String| text.lines().map(str::to_owned).collect::<HashSet<String>>();
    let pulled = set(git_optional(root, repo, &["rev-list", "-300", "HEAD"]).await?);
    let pullable = set(git_optional(root, repo, &["rev-list", "-300", &upstream, "^HEAD"]).await?);
    let unpushed =
        set(git_optional(root, repo, &["rev-list", "-300", "HEAD", &not_upstream]).await?);
    let mut lanes: Vec<Option<String>> = Vec::new();
    let mut result = Vec::new();
    let mut maximum = 0;
    let position = |lanes: &[Option<String>], value: &str| {
        lanes.iter().position(|lane| lane.as_deref() == Some(value))
    };
    let empty = |lanes: &mut Vec<Option<String>>| match lanes.iter().position(Option::is_none) {
        Some(slot) => slot,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    };
    for line in raw.lines() {
        let fields: Vec<&str> = line.split('\x1f').collect();
        let [full, parents_text, refs, author, relative, iso, subject] = fields[..] else {
            continue;
        };
        if !matches!(full.len(), 40 | 64) {
            continue;
        }
        let parents: Vec<&str> = parents_text.split_whitespace().collect();
        let incoming = lanes.clone();
        let lane = match position(&lanes, full) {
            Some(lane) => lane,
            None => empty(&mut lanes),
        };
        for slot in lanes.iter_mut() {
            if slot.as_deref() == Some(full) {
                *slot = None;
            }
        }
        let mut parent_lanes = Vec::new();
        for (index, parent) in parents.iter().enumerate() {
            let existing = position(&lanes, parent);
            if index == 0 && lanes[lane].is_none() && existing.is_none_or(|e| e > lane) {
                lanes[lane] = Some((*parent).to_owned());
                parent_lanes.push(lane);
            } else if let Some(existing) = existing {
                parent_lanes.push(existing);
            } else {
                let slot = empty(&mut lanes);
                lanes[slot] = Some((*parent).to_owned());
                parent_lanes.push(slot);
            }
        }
        let outgoing = lanes.clone();
        maximum = maximum
            .max(lane + 1)
            .max(incoming.len())
            .max(outgoing.len());
        let shorten = |values: &[Option<String>]| -> Vec<Value> {
            values
                .iter()
                .map(|value| value.as_deref().map_or(Value::Null, |v| json!(short(v))))
                .collect()
        };
        let merge_in: Vec<usize> = incoming
            .iter()
            .enumerate()
            .filter(|(index, value)| value.as_deref() == Some(full) && *index != lane)
            .map(|(index, _)| index)
            .collect();
        result.push(json!({"hash":short(full),"full_hash":full,
            "parents":parents.iter().map(|p| short(p)).collect::<Vec<_>>(),
            "refs":refs,"author":author,"date_relative":relative,"date_iso":iso,
            "message":subject,"lane":lane,"parent_lanes":parent_lanes,
            "lanes_in":shorten(&incoming),"lanes_out":shorten(&outgoing),
            "merge_in_lanes":merge_in,"is_head":full == head,"is_pulled":pulled.contains(full),
            "is_pullable":pullable.contains(full),"is_unpushed":unpushed.contains(full)}));
    }
    let refs = git_optional(
        root,
        repo,
        &[
            "for-each-ref",
            "--format=%(refname:short)%x1f%(objectname)",
            "refs/heads",
            "refs/remotes",
        ],
    )
    .await?;
    let mut branches = Map::new();
    for line in refs.lines() {
        if let [name, object] = line.split('\x1f').collect::<Vec<_>>()[..] {
            branches.insert(name.to_owned(), json!(short(object)));
        }
    }
    Ok(
        json!({"commits":result,"max_lane":maximum,"branches":branches,
        "current_branch":branch,"head_hash":head}),
    )
}

async fn detail(root: &Path, repo: &Path, hash: Option<&Value>) -> Outcome<Value> {
    let hash = hash
        .and_then(Value::as_str)
        .filter(|hash| {
            (40..=64).contains(&hash.len())
                && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
        .ok_or_else(|| rejected("Full commit hash required"))?;
    let raw = command(
        root,
        repo,
        &[
            "show",
            "-s",
            "--no-show-signature",
            "--format=%H%x1f%h%x1f%an%x1f%ae%x1f%aI%x1f%cn%x1f%ce%x1f%cI%x1f%P%x1f%s%x1f%b",
            hash,
        ],
        Run::default(),
    )
    .await?;
    let raw = lossy(&raw);
    let raw = raw.trim_end_matches('\n');
    let fields: Vec<&str> = raw.splitn(11, '\x1f').collect();
    if fields.len() != 11 {
        return Err(rejected("Commit detail unavailable"));
    }
    let names = [
        "full_hash",
        "hash",
        "author",
        "author_email",
        "author_date",
        "committer",
        "committer_email",
        "commit_date",
        "parents",
        "subject",
        "body",
    ];
    let mut detail: Map<String, Value> = names
        .iter()
        .zip(&fields)
        .map(|(name, value)| ((*name).to_owned(), json!(value)))
        .collect();
    detail.insert(
        "parents".into(),
        json!(fields[8].split_whitespace().collect::<Vec<_>>()),
    );
    let files = git_text(
        root,
        repo,
        &[
            "show",
            "--numstat",
            "--format=",
            "--no-ext-diff",
            "--no-textconv",
            hash,
        ],
    )
    .await?;
    let number = |text: &str| {
        if !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) {
            text.parse::<u64>().unwrap_or(0)
        } else {
            0
        }
    };
    let files: Vec<Value> = files
        .lines()
        .filter_map(|line| match line.splitn(3, '\t').collect::<Vec<_>>()[..] {
            [added, removed, name] => Some(json!({"path":name,"added":number(added),
                "removed":number(removed),"binary":added == "-" && removed == "-"})),
            _ => None,
        })
        .collect();
    detail.insert("files".into(), json!(files));
    Ok(Value::Object(detail))
}

async fn branches(root: &Path, repo: &Path) -> Outcome<Value> {
    let listing = |prefix: &'static str| async move {
        let raw = git_optional(
            root,
            repo,
            &[
                "for-each-ref",
                "--format=%(refname:short)%x1f%(objectname)",
                prefix,
            ],
        )
        .await?;
        Ok::<_, Failure>(
            raw.lines()
                .filter_map(|line| line.split_once('\x1f'))
                .map(|(name, object)| (name.to_owned(), object.to_owned()))
                .collect::<HashMap<String, String>>(),
        )
    };
    let local = listing("refs/heads").await?;
    let remote: HashMap<String, String> = listing("refs/remotes/origin")
        .await?
        .into_iter()
        .filter(|(name, _)| name != "origin/HEAD")
        .map(|(name, object)| {
            (
                name.strip_prefix("origin/").unwrap_or(&name).to_owned(),
                object,
            )
        })
        .collect();
    let current = git_text(root, repo, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await?;
    let names: BTreeSet<&String> = local.keys().chain(remote.keys()).collect();
    let branches: Vec<Value> = names
        .into_iter()
        .map(|name| {
            let object = local
                .get(name)
                .filter(|object| !object.is_empty())
                .or_else(|| remote.get(name))
                .map_or(String::new(), |object| short(object));
            json!({"name":name,"hash":object,"local":local.contains_key(name),
                "remote":remote.contains_key(name)})
        })
        .collect();
    Ok(json!({"current":current,"branches":branches}))
}

async fn commit(root: &Path, repo: &Path, args: &Map<String, Value>) -> Outcome<Value> {
    // The admin page commits a reviewed change set; model tools keep their 64-path bound.
    let paths = paths(repo, args.get("paths").unwrap_or(&Value::Null), MAX_ENTRIES)?;
    let message = args
        .get("message")
        .and_then(Value::as_str)
        .filter(|m| !m.trim().is_empty() && m.len() <= 2000)
        .ok_or_else(|| rejected("Commit message required"))?;
    if guard::secret(message.as_bytes()) {
        return Err(rejected("Commit message contains possible credentials"));
    }
    fence(root, repo, args.get("expected_head")).await?;
    let repo_name = args
        .get("repo")
        .cloned()
        .unwrap_or_else(|| json!("__root__"));
    let (_, _, review) = live_snapshot(root, repo, &repo_name).await?;
    if args.get("expected_review_sha256").and_then(Value::as_str) != Some(review.as_str()) {
        return Err(rejected("Review changed; refresh before committing"));
    }
    for name in &paths {
        manager::parts(&json!(name), false)?;
        let content = read_source(root, repo, &json!(name), true, GIT_SOURCE_LIMIT)?;
        if content.is_some_and(|(content, _)| guard::secret(&content)) {
            return Err(rejected("Source contains possible credentials"));
        }
    }
    let staged = command(
        root,
        repo,
        &["diff", "--cached", "--name-only", "-z"],
        Run::default(),
    )
    .await?;
    for name in staged.split(|b| *b == 0).filter(|name| !name.is_empty()) {
        let name = std::str::from_utf8(name).map_err(|_| generic())?;
        if !paths.iter().any(|path| path == name) {
            return Err(rejected("Unselected staged paths present"));
        }
    }
    let author = git_optional(root, repo, &["config", "--local", "user.name"]).await?;
    let email = git_optional(root, repo, &["config", "--local", "user.email"]).await?;
    let author = format!(
        "user.name={}",
        if author.is_empty() {
            "WebAgent v3"
        } else {
            &author
        }
    );
    let email = format!(
        "user.email={}",
        if email.is_empty() {
            "webagent-v3@localhost"
        } else {
            &email
        }
    );
    let mut add = vec!["add", "--"];
    add.extend(paths.iter().map(String::as_str));
    mutate(root, repo, &add).await?;
    let mut argv = vec![
        "-c",
        &author,
        "-c",
        &email,
        "commit",
        "--no-verify",
        "-m",
        message,
        "--",
    ];
    argv.extend(paths.iter().map(String::as_str));
    mutate(root, repo, &argv).await?;
    let title = message.lines().next().unwrap_or_default();
    Ok(
        json!({"status":"committed","title":title,"hash":short(&head(root, repo).await?),
        "push":{"attempted":false}}),
    )
}

async fn settings(
    root: &Path,
    repo: &Path,
    args: &Map<String, Value>,
    trusted: &Value,
) -> Outcome<Value> {
    const ALLOWED: [&str; 6] = [
        "action",
        "repo",
        "expected_head",
        "remote_url",
        "git_user_name",
        "git_user_email",
    ];
    if args.keys().any(|key| !ALLOWED.contains(&key.as_str())) {
        return Err(rejected("Invalid repository settings"));
    }
    fence(root, repo, args.get("expected_head")).await?;
    if let Some(requested) = args.get("remote_url").filter(|value| !value.is_null()) {
        let requested = requested
            .as_str()
            .ok_or_else(|| rejected("Invalid remote URL"))?;
        let old = git_optional(root, repo, &["remote", "get-url", "origin"]).await?;
        if !requested.is_empty() {
            let value = super::remote::normalized(&json!(requested))?;
            let granted = super::remote::connections(trusted)?;
            if !granted.iter().any(|row| row.url == value) {
                return Err(rejected(
                    "Remote URL is not granted by V3 repository connections",
                ));
            }
            if old.is_empty() {
                mutate(root, repo, &["remote", "add", "origin", &value]).await?;
            } else if old != value {
                mutate(root, repo, &["remote", "set-url", "origin", &value]).await?;
            }
        } else if !old.is_empty() {
            mutate(root, repo, &["remote", "remove", "origin"]).await?;
        }
    }
    for (key, config) in [
        ("git_user_name", "user.name"),
        ("git_user_email", "user.email"),
    ] {
        let value = match args.get(key) {
            None | Some(Value::Null) => continue,
            Some(Value::String(value)) if value.is_empty() => continue,
            Some(value) => value,
        };
        let value = value
            .as_str()
            .filter(|v| v.len() <= 128 && !v.chars().any(|c| (c as u32) < 32 || c as u32 == 127))
            .ok_or_else(|| rejected("Invalid Git author identity"))?;
        mutate(root, repo, &["config", "--local", config, value]).await?;
    }
    Ok(json!({"status":"ok","message":"Repository settings saved"}))
}

#[cfg(test)]
mod tests {
    use super::{line_count, split_lines_keep};

    #[test]
    fn line_counts_follow_python_splitlines() {
        assert_eq!(line_count(b""), 0);
        assert_eq!(line_count(b"a"), 1);
        assert_eq!(line_count(b"a\n"), 1);
        assert_eq!(line_count(b"a\r\nb\rc\n\n"), 4);
    }

    #[test]
    fn kept_line_ends_follow_python_splitlines() {
        assert_eq!(split_lines_keep("a\r\nb\n\nc"), ["a\r\n", "b\n", "\n", "c"]);
        assert_eq!(split_lines_keep("x\u{2028}y"), ["x\u{2028}", "y"]);
        assert!(split_lines_keep("").is_empty());
    }
}
