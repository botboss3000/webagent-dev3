//! Remote Git: the host's repository grants, HTTPS transport through the askpass helper,
//! the outgoing-history credential scan, fetch, pull, push and commit-and-push.
use super::{
    command, command_os, exact, generic, guard, head, hex_digest, metadata_bounded, paths,
    read_source, reference, rejected, repository, review_digest, text, unsafe_key, validate,
    without_empty, Failure, Outcome, Run, GIT_SOURCE_LIMIT,
};
use serde_json::{json, Map, Value};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// One granted HTTPS repository: its normalized URL, user name and optional token.
pub struct Connection {
    pub url: String,
    pub username: String,
    pub token: Option<String>,
}

/// The parts of an `https://` URL that `urllib.parse.urlsplit` would report.
pub(super) struct Split<'a> {
    pub(super) scheme: String,
    pub(super) userinfo: Option<&'a str>,
    pub(super) host: &'a str,
    pub(super) port: Option<&'a str>,
    pub(super) path: &'a str,
    pub(super) query: bool,
    pub(super) fragment: bool,
}

pub(super) fn split(raw: &str) -> Option<Split<'_>> {
    let colon = raw.find(':')?;
    let scheme = &raw[..colon];
    if scheme.is_empty()
        || !scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
    {
        return None;
    }
    let rest = raw[colon + 1..].strip_prefix("//")?;
    let netloc_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (netloc, tail) = rest.split_at(netloc_end);
    let (before_fragment, fragment) = match tail.split_once('#') {
        Some((before, after)) => (before, !after.is_empty()),
        None => (tail, false),
    };
    let (path, query) = match before_fragment.split_once('?') {
        Some((path, query)) => (path, !query.is_empty()),
        None => (before_fragment, false),
    };
    let (userinfo, hostinfo) = match netloc.rsplit_once('@') {
        Some((userinfo, hostinfo)) => (Some(userinfo), hostinfo),
        None => (None, netloc),
    };
    let (host, port) = if let Some(bracketed) = hostinfo.strip_prefix('[') {
        let (host, after) = bracketed.split_once(']')?;
        (host, after.strip_prefix(':'))
    } else {
        match hostinfo.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (hostinfo, None),
        }
    };
    Some(Split {
        scheme: scheme.to_ascii_lowercase(),
        userinfo,
        host,
        port,
        path,
        query,
        fragment,
    })
}

/// The canonical form of a configured HTTPS repository URL without credentials.
pub(super) fn normalized(raw: &Value) -> Outcome<String> {
    let invalid = || rejected("Invalid configured HTTPS repository");
    let raw = raw.as_str().ok_or_else(invalid)?;
    if raw.chars().count() > 2048 || raw.trim() != raw || raw.chars().any(|c| (c as u32) < 33) {
        return Err(invalid());
    }
    let required = || rejected("HTTPS repository without embedded credentials required");
    let value = split(raw).ok_or_else(required)?;
    let port = match value.port {
        None | Some("") => None,
        Some(port) => Some(
            port.parse::<u32>()
                .ok()
                .filter(|p| port.bytes().all(|b| b.is_ascii_digit()) && *p <= 65535)
                .ok_or_else(|| rejected("Invalid repository port"))?,
        ),
    };
    let credentials = value.userinfo.is_some_and(|info| {
        let (user, password) = info.split_once(':').unwrap_or((info, ""));
        !user.is_empty() || !password.is_empty()
    });
    if value.scheme != "https"
        || value.host.is_empty()
        || credentials
        || value.query
        || value.fragment
        || value.path.is_empty()
        || value.path == "/"
        || value.path.contains('%')
    {
        return Err(required());
    }
    let mut host = value.host.to_lowercase();
    if host.contains(':') {
        host = format!("[{host}]");
    }
    let authority = match port {
        Some(port) if port != 443 => format!("{host}:{port}"),
        _ => host,
    };
    if value
        .path
        .split('/')
        .any(|part| part == "." || part == "..")
    {
        return Err(rejected("Normalized repository path required"));
    }
    Ok(format!(
        "https://{authority}{}",
        value.path.trim_end_matches('/')
    ))
}

/// The agent's granted repositories from the host's trusted call data.
///
/// # Errors
/// When the trusted data is malformed or ambiguous; every Git call then fails.
pub fn connections(trusted: &Value) -> Outcome<Vec<Connection>> {
    let Value::Object(object) = trusted else {
        return Ok(Vec::new());
    };
    let scope = object.get("repository").unwrap_or(trusted);
    let rows = match scope.get("connections") {
        None => return Ok(Vec::new()),
        Some(Value::Array(rows)) if rows.len() <= 8 => rows,
        Some(_) => return Err(rejected("Invalid trusted repository connections")),
    };
    let mut result: Vec<Connection> = Vec::new();
    for row in rows {
        if !row.is_object() || row["revoked"] == json!(true) {
            continue;
        }
        let url = normalized(&row["url"])?;
        let visible = |text: &str| text.chars().all(|c| (33..=126).contains(&(c as u32)));
        let username = row["username"]
            .as_str()
            .filter(|name| {
                (1..=128).contains(&name.chars().count())
                    && visible(name)
                    && !name.contains(['\'', '"', '\\', '@', ':'])
            })
            .ok_or_else(|| rejected("Invalid trusted repository username"))?;
        let token = match &row["token"] {
            Value::Null => None,
            Value::String(token)
                if (8..=512).contains(&token.chars().count()) && visible(token) =>
            {
                Some(token.clone())
            }
            _ => return Err(rejected("Invalid trusted repository credential")),
        };
        if result.iter().any(|previous| previous.url == url) {
            return Err(rejected("Ambiguous repository connection"));
        }
        result.push(Connection {
            url,
            username: username.to_owned(),
            token,
        });
    }
    Ok(result)
}

/// Reviewed, already committed regression-test lines (SHA-256 of the full added diff line)
/// accepted by the outgoing scan in exactly this commit and path. Tool arguments cannot
/// select them.
const REVIEWED_HISTORY_LINES: [(&str, &str, &[&str]); 6] = [
    // Models tests exercise encrypted storage and device-flow fencing with dummy tokens.
    (
        "22934cce71b2157b3e257b02f266256f5e4a1833",
        "crates/modules/models/src/model_io_tests.rs",
        &["9a06dea7e24d76373a11501db5bfd1b881850f95c4cb85231b1b2ac379d4ed74"],
    ),
    (
        "22934cce71b2157b3e257b02f266256f5e4a1833",
        "crates/modules/models/src/runtime.rs",
        &[
            "d8361830b286f433a795dd4c4dde6560da19cfdef8cc6c64cf34d371272ed359",
            "6749b873c877027b761e26054e497da416b04d26f3005cc4b30ca342c672c24d",
        ],
    ),
    // Manager fixture supplies a dummy token only to its temporary loopback mock provider.
    (
        "4f2442caad651fed6d64cb7147f48c512a352246",
        "crates/modules/manager/src/tests.rs",
        &["9232707750cebd746de222b1620b3a04c17b31500b8e0ae973ffb06cb5d8b4b1"],
    ),
    // UI observer fixture uses a public sentinel to prove private arguments stay hidden.
    (
        "f2d332c246ce23dae46b6267b5301a59182df87f",
        "ui/chat/core/chat-notifications.browser.test.cjs",
        &["6ef6c6a7a40bbb800a8a9b78261065a47f9d524d661cfcce61ee8202761596d6"],
    ),
    (
        "0aa6da670fba3fed8018af1c125eaca164f6f3a0",
        "abilities/Administrator/git_control/test_runner.py",
        &[
            "5251d40c76a281311caf4b366d595672db2b280d6c9454d50da72aa03dfc1a5c",
            "d20e527b2b21a4eaeb67130f34d20c2884f3a6680ce6d6296822212b0c333784",
        ],
    ),
    (
        "ec514257598a923a9ef6b55723e2dc1d41cf16cc",
        "abilities/Administrator/git_control/test_runner.py",
        &["1cb448b9870982d4ffc892f4ade5af24c52ac6d4e60eecce42d4c65bb91a507e"],
    ),
];

fn commit_id(text: &str) -> bool {
    (40..=64).contains(&text.len()) && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Refuses unless HEAD is exactly `expected`.
pub(super) async fn fence(root: &Path, repo: &Path, expected: Option<&Value>) -> Outcome<()> {
    let expected = expected.and_then(Value::as_str).filter(|e| commit_id(e));
    match expected {
        Some(expected) if head(root, repo).await? == expected => Ok(()),
        _ => Err(rejected("HEAD changed; review before retry")),
    }
}

/// `validate` for a workspace-local bare remote: bare configuration, no hooks.
async fn validate_bare(root: &Path, repo: &Path) -> Outcome<()> {
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
        if unsafe_key(&key) || key == "core.bare" && !item.ends_with(b"\ntrue") {
            return Err(rejected("Unsafe Git configuration"));
        }
    }
    metadata_bounded(repo)?;
    let bare = command(
        root,
        repo,
        &["rev-parse", "--is-bare-repository"],
        Run::default(),
    )
    .await?;
    if bare.trim_ascii() != b"true" {
        return Err(rejected("Workspace remote must be bare"));
    }
    if let Ok(hooks) = std::fs::read_dir(repo.join("hooks")) {
        for hook in hooks.flatten() {
            let name = hook.file_name();
            if hook.path().is_file() && !name.to_string_lossy().ends_with(".sample") {
                return Err(rejected("Workspace remote hooks unavailable"));
            }
        }
    }
    Ok(())
}

/// The scheme `urlsplit` would report: letters first, then letters, digits, `+`, `-`, `.`.
fn url_scheme(url: &str) -> Option<String> {
    let colon = url.find(':')?;
    let scheme = &url[..colon];
    let valid = scheme
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphabetic)
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b));
    valid.then(|| scheme.to_ascii_lowercase())
}

/// Where a remote operation goes: a local bare repository in the workspace (no
/// credentials) or a granted HTTPS repository with the askpass transport.
enum Target {
    Local(String),
    Network(Vec<(String, String)>),
}

/// The credential prompt helper: the app executable, in its `V3_GIT_ASKPASS` mode.
fn askpass() -> Outcome<PathBuf> {
    let helper = PathBuf::from(super::askpass::HELPER);
    let meta = std::fs::symlink_metadata(&helper).map_err(|_| generic())?;
    // Git executes GIT_ASKPASS directly as one file; the sandbox allows only this helper.
    if !meta.is_file() || std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 == 0
    {
        return Err(rejected("Trusted Git credential helper is not executable"));
    }
    Ok(helper)
}

async fn transport(
    root: &Path,
    repo: &Path,
    remote: &str,
    operation: &str,
    granted: &[Connection],
) -> Outcome<Target> {
    let url = command(root, repo, &["remote", "get-url", remote], Run::default()).await?;
    let url = String::from_utf8(url).map_err(|_| generic())?;
    let url = url.trim();
    if guard::secret(url.as_bytes()) || url.chars().any(char::is_whitespace) {
        return Err(rejected("Unsafe configured remote"));
    }
    let scheme = url_scheme(url);
    let scp = url.split_once('@').is_some_and(|(user, rest)| {
        rest.split_once(':').is_some_and(|(host, path)| {
            !user.is_empty()
                && !host.is_empty()
                && !path.is_empty()
                && user
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
                && host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
                && path
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_./-".contains(&b))
        })
    });
    if scheme.is_none() && !scp {
        let target = if url.starts_with('/') {
            PathBuf::from(url)
        } else {
            repo.join(url)
        };
        let target =
            std::fs::canonicalize(&target).map_err(|_| rejected("Remote outside workspace"))?;
        if !target.starts_with(root) || !target.is_dir() {
            return Err(rejected("Remote outside workspace"));
        }
        if target.join("commondir").exists() || target.join("objects/info/alternates").exists() {
            return Err(rejected("Unsafe workspace remote"));
        }
        validate_bare(root, &target).await?;
        return Ok(Target::Local(target.to_string_lossy().into_owned()));
    }
    if !matches!(scheme.as_deref(), Some("https" | "ssh")) && !scp {
        return Err(rejected("Remote transport unavailable"));
    }
    if let Some(parts) = split(url) {
        let (user, password) =
            parts
                .userinfo
                .map_or(("", None), |info| match info.split_once(':') {
                    Some((user, password)) => (user, Some(password)),
                    None => (info, None),
                });
        if password.is_some_and(|p| !p.is_empty()) || parts.scheme == "https" && !user.is_empty() {
            return Err(rejected("Credential-bearing URL refused"));
        }
    }
    // The host's grant: only an exact, normalized HTTPS URL of a connection is served.
    if !matches!(operation, "fetch" | "pull" | "push") {
        return Err(rejected("Unsupported repository transport operation"));
    }
    let exact = normalized(&json!(url))?;
    let row = granted
        .iter()
        .find(|row| row.url == exact)
        .ok_or_else(|| rejected("Remote is not granted by this agent's repository connections"))?;
    let helper = askpass()?;
    Ok(Target::Network(vec![
        ("GIT_ASKPASS".into(), helper.to_string_lossy().into_owned()),
        ("V3_GIT_ASKPASS".into(), "1".into()),
        ("V3_GIT_URL".into(), exact),
        ("V3_GIT_USERNAME".into(), row.username.clone()),
        ("V3_GIT_TOKEN".into(), row.token.clone().unwrap_or_default()),
    ]))
}

/// Changed paths scanned by one `git show` in the outgoing scan: batching keeps the process
/// count bounded while a whole-folder move no longer needs one process per path.
const SCAN_PATHS_PER_COMMAND: usize = 64;
/// A changed-path listing for one commit: a whole-folder move touches thousands of files.
const SCAN_PATH_LIST_LIMIT: usize = 4 * 1024 * 1024;

fn located(commit: &str) -> Value {
    json!({"status":"located","commit":commit,"match_class":"credential_pattern"})
}

/// Scans every outgoing commit's message and each changed path's diff for credentials.
async fn scan_outgoing_history(
    root: &Path,
    repo: &Path,
    remote: &str,
    scan_head: &str,
) -> Outcome<()> {
    let remotes = format!("--remotes={remote}");
    let listed = command(
        root,
        repo,
        &["rev-list", "--max-count=257", scan_head, "--not", &remotes],
        Run::default(),
    )
    .await?;
    let listed = String::from_utf8_lossy(&listed).into_owned();
    let commits: Vec<&str> = listed.lines().collect();
    if commits.len() > 256 {
        return Err(Failure::Rejected(
            "git_credential_guard",
            "Outgoing history exceeds credential scan commit bound".into(),
            Some(json!({"status":"unavailable","reason":"commit_bound"})),
        ));
    }
    let scan = |reviewed| Run {
        scan_only: true,
        reviewed,
        ..Run::default()
    };
    for sha in commits {
        if !commit_id(sha) {
            return Err(Failure::Rejected(
                "git_credential_guard",
                "Invalid outgoing commit".into(),
                None,
            ));
        }
        match command(
            root,
            repo,
            &["show", "--no-patch", "--format=fuller", sha],
            scan(None),
        )
        .await
        {
            Err(Failure::Rejected("git_credential_guard", message, _)) => {
                return Err(Failure::Rejected(
                    "git_credential_guard",
                    message,
                    Some(located(sha)),
                ));
            }
            Err(other) => return Err(other),
            Ok(_) => {}
        }
        let changed = command(
            root,
            repo,
            &[
                "diff-tree",
                "-m",
                "--root",
                "--no-renames",
                "--no-commit-id",
                "--name-only",
                "-r",
                "-z",
                sha,
            ],
            Run {
                output_limit: Some(SCAN_PATH_LIST_LIMIT),
                ..Run::default()
            },
        )
        .await?;
        let paths: Vec<&[u8]> = changed
            .split(|b| *b == 0)
            .filter(|raw| !raw.is_empty())
            .collect();
        for batch in paths.chunks(SCAN_PATHS_PER_COMMAND) {
            if !scan_paths(root, repo, sha, batch).await? {
                continue;
            }
            // A batch is refused only when it carries a credential: rescan its paths one at a
            // time so the failure names the exact path, line and any reviewed-line exception.
            for raw in batch {
                scan_path(root, repo, sha, raw).await?;
            }
        }
    }
    Ok(())
}

/// Scans one batch of a commit's changed paths; `true` when the batch was refused, so the
/// caller rescans its paths individually to name the offending one.
async fn scan_paths(root: &Path, repo: &Path, sha: &str, paths: &[&[u8]]) -> Outcome<bool> {
    let mut argv: Vec<&OsStr> = [
        "show",
        "-m",
        "--format=",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--unified=0",
        "-p",
        sha,
        "--",
    ]
    .into_iter()
    .map(OsStr::new)
    .collect();
    argv.extend(paths.iter().map(|raw| OsStr::from_bytes(raw)));
    match command_os(
        root,
        repo,
        &argv,
        Run {
            scan_only: true,
            ..Run::default()
        },
    )
    .await
    {
        Err(Failure::Rejected("git_credential_guard", _, _)) => Ok(true),
        Err(other) => Err(other),
        Ok(_) => Ok(false),
    }
}

/// Scans one changed path, reporting the credential's exact commit, path and line.
async fn scan_path(root: &Path, repo: &Path, sha: &str, raw: &[u8]) -> Outcome<()> {
    let text = String::from_utf8_lossy(raw);
    let safe = (1..=240).contains(&raw.len())
        && raw
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(b))
        && !guard::secret(raw);
    let reviewed = REVIEWED_HISTORY_LINES
        .iter()
        .find(|(commit, path, _)| *commit == sha && *path == text)
        .map(|(_, _, lines)| *lines);
    let argv: Vec<&OsStr> = [
        "show",
        "-m",
        "--format=",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--unified=0",
        "-p",
        sha,
        "--",
    ]
    .into_iter()
    .map(OsStr::new)
    .chain([OsStr::from_bytes(raw)])
    .collect();
    match command_os(
        root,
        repo,
        &argv,
        Run {
            scan_only: true,
            reviewed,
            ..Run::default()
        },
    )
    .await
    {
        Err(Failure::Rejected("git_credential_guard", message, diagnostics)) => {
            let mut location = located(sha);
            if safe {
                location["path"] = json!(text);
            } else {
                location["path_sha256"] = json!(hex_digest(raw));
                location["path_withheld"] = json!(true);
            }
            if let Some(line) = diagnostics.as_ref().and_then(|d| d["line"].as_i64()) {
                location["line"] = json!(line);
            }
            Err(Failure::Rejected(
                "git_credential_guard",
                message,
                Some(location),
            ))
        }
        Err(other) => Err(other),
        Ok(_) => Ok(()),
    }
}

/// Fetch, pull (fast-forward only) or push the current branch; a push first scans every
/// outgoing commit and fences HEAD at the scanned commit.
pub(super) async fn network(
    root: &Path,
    repo: &Path,
    remote: &str,
    operation: &str,
    granted: &[Connection],
) -> Outcome<Vec<u8>> {
    let target = transport(root, repo, remote, operation, granted).await?;
    if operation == "push" {
        let scan_head = head(root, repo).await?;
        scan_outgoing_history(root, repo, remote, &scan_head).await?;
        fence(root, repo, Some(&json!(scan_head))).await?;
    }
    let branch = if operation == "fetch" {
        String::new()
    } else {
        let current = command(
            root,
            repo,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            Run::default(),
        )
        .await?;
        reference(Some(&json!(String::from_utf8_lossy(&current).trim())))?.to_owned()
    };
    let (destination, environment, local) = match &target {
        Target::Local(path) => (path.as_str(), None, true),
        Target::Network(environment) => (remote, Some(environment.as_slice()), false),
    };
    let refspec = format!("HEAD:refs/heads/{branch}");
    let argv: Vec<&str> = match operation {
        "fetch" => vec!["fetch", "--no-tags", "--prune", remote],
        "pull" => vec!["pull", "--ff-only", "--no-rebase", destination, &branch],
        _ => vec!["push", "--no-verify", destination, &refspec],
    };
    let run = Run {
        mutable: true,
        transport: environment,
        local,
        timeout: Some(120),
        ..Run::default()
    };
    let output = command(root, repo, &argv, run).await?;
    if local && operation == "push" {
        let tracking = format!("refs/remotes/{remote}/{branch}");
        let current = head(root, repo).await?;
        command(
            root,
            repo,
            &["update-ref", &tracking, &current],
            Run {
                mutable: true,
                ..Run::default()
            },
        )
        .await?;
    }
    Ok(output)
}

/// Commits exactly the reviewed paths and pushes them to a granted remote.
pub(super) async fn commit_and_push(
    root: &Path,
    args: &Map<String, Value>,
    granted: &[Connection],
) -> Outcome<Value> {
    let args = without_empty(args, &["repo", "remote"]);
    exact(
        &args,
        &[
            "repo",
            "paths",
            "message",
            "expected_head",
            "expected_review_sha256",
            "remote",
        ],
        &[
            "paths",
            "message",
            "expected_head",
            "expected_review_sha256",
        ],
    )?;
    let repo = repository(root, args.get("repo").unwrap_or(&json!("webagent-v3")))?;
    validate(root, &repo).await?;
    fence(root, &repo, args.get("expected_head")).await?;
    let review = args["expected_review_sha256"].as_str();
    if Some(review_digest(root, &repo).await?.as_str()) != review {
        return Err(rejected("Review changed"));
    }
    let selected = paths(&repo, &args["paths"], 64)?;
    let message = text(args.get("message"), 2000)?;
    if guard::secret(message.as_bytes()) {
        return Err(rejected("Commit message contains possible credentials"));
    }
    for name in &selected {
        if let Some((data, _)) = read_source(root, &repo, &json!(name), true, GIT_SOURCE_LIMIT)? {
            if guard::secret(&data) {
                return Err(rejected("Source contains possible credentials"));
            }
        }
    }
    let remote = reference(Some(args.get("remote").unwrap_or(&json!("origin"))))?.to_owned();
    transport(root, &repo, &remote, "push", granted).await?;
    fence(root, &repo, args.get("expected_head")).await?;
    if Some(review_digest(root, &repo).await?.as_str()) != review {
        return Err(rejected("Review changed during remote authorization"));
    }
    let staged = command(
        root,
        &repo,
        &["diff", "--cached", "--name-only", "-z"],
        Run::default(),
    )
    .await?;
    for name in staged.split(|b| *b == 0).filter(|n| !n.is_empty()) {
        let name = std::str::from_utf8(name).map_err(|_| generic())?;
        if !selected.iter().any(|path| path == name) {
            return Err(rejected("Unselected staged paths present"));
        }
    }
    let mutation = || Run {
        mutable: true,
        ..Run::default()
    };
    let mut add = vec!["add", "--"];
    add.extend(selected.iter().map(String::as_str));
    command(root, &repo, &add, mutation()).await?;
    let mut commit = vec![
        "-c",
        "user.name=WebAgent",
        "-c",
        "user.email=webagent@localhost",
        "commit",
        "--no-verify",
        "-m",
        message,
        "--",
    ];
    commit.extend(selected.iter().map(String::as_str));
    command(root, &repo, &commit, mutation()).await?;
    let committed = head(root, &repo).await?;
    match network(root, &repo, &remote, "push", granted).await {
        Ok(output) => {
            Ok(json!({"commit":committed,"text":String::from_utf8(output).map_err(|_| generic())?}))
        }
        Err(_) => Err(Failure::Uncertain(format!(
            "Commit created ({committed}); reconcile remote before retry"
        ))),
    }
}
