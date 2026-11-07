//! `git_tool` reads and the repository watch on fixture repositories, through the module
//! the worker dispatches to. Skipped where the kernel lacks Landlock ABI 6.
mod harness;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@t",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(repo)
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?}");
}

/// A workspace with `webagent-v3` (two commits, a modified file, an untracked file, a
/// staged file), `unsafe` (an ssh command in its config) and `leaky` (a committed
/// credential-shaped line).
fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("workspace");
    let repo = dir.path().join("webagent-v3");
    std::fs::create_dir_all(repo.join("src")).expect("repo");
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "one\n").expect("file");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "first"]);
    std::fs::write(repo.join("a.txt"), "one\ntwo\n").expect("file");
    git(&repo, &["commit", "-qam", "second change"]);
    std::fs::write(repo.join("a.txt"), "one\ntwo\nthree\n").expect("file");
    std::fs::write(repo.join("new.txt"), "new\n").expect("file");
    for (name, config) in [("unsafe", true), ("leaky", false)] {
        let other = dir.path().join(name);
        std::fs::create_dir_all(&other).expect("repo");
        git(&other, &["init", "-q"]);
        let line = ["pass", "word = \"", "abcdefghijklmnopqrs", "\"\n"].concat();
        std::fs::write(other.join("conf"), line).expect("file");
        git(&other, &["add", "."]);
        git(&other, &["commit", "-qm", "only"]);
        if config {
            git(&other, &["config", "core.sshCommand", "ssh -i key"]);
        }
    }
    dir
}

fn available() -> bool {
    let ok = webagent_worker::sandbox::abi().is_ok_and(|abi| abi >= 6);
    if !ok {
        eprintln!("skipped: Landlock ABI 6 unavailable");
    }
    ok
}

async fn read(base: &Path, args: Value) -> Value {
    let base = std::fs::canonicalize(base).expect("base");
    webagent_worker::git::execute(&base, "git_tool", &args, &Value::Null).await
}

async fn status_reports_changes_with_a_stable_review_digest() {
    if !available() {
        return;
    }
    let dir = workspace();
    let first = read(dir.path(), json!({"operation":"status"})).await;
    assert_eq!(first["status"], "success", "{first}");
    let output = &first["output"];
    assert_eq!(output["head"].as_str().map(str::len), Some(40));
    assert!(output["status"]
        .as_str()
        .unwrap_or_default()
        .contains(" M a.txt"));
    assert_eq!(output["notice"]["dirty"], true);
    assert_eq!(output["has_upstream"], false);
    let second = read(dir.path(), json!({"operation":"status","repo":""})).await;
    assert_eq!(second["output"]["review_sha256"], output["review_sha256"]);
    std::fs::write(dir.path().join("webagent-v3/new.txt"), "changed\n").expect("file");
    let third = read(dir.path(), json!({"operation":"status"})).await;
    assert_ne!(third["output"]["review_sha256"], output["review_sha256"]);
}

async fn reads_return_git_text() {
    if !available() {
        return;
    }
    let dir = workspace();
    let log = read(dir.path(), json!({"operation":"log","limit":1})).await;
    assert!(
        log["output"]["text"]
            .as_str()
            .unwrap_or_default()
            .ends_with(" second change\n"),
        "{log}"
    );
    let diff = read(dir.path(), json!({"operation":"diff","paths":["a.txt"]})).await;
    assert!(
        diff["output"]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("+three"),
        "{diff}"
    );
    let branches = read(dir.path(), json!({"operation":"branch_list"})).await;
    assert_eq!(branches["output"]["text"], "* main\n");
}

async fn unsafe_input_and_repositories_are_refused() {
    if !available() {
        return;
    }
    let dir = workspace();
    let refused = |result: &Value, message: &str| {
        assert_eq!(result["status"], "error", "{result}");
        assert_eq!(result["output"], message, "{result}");
    };
    refused(
        &read(dir.path(), json!({"operation":"status","paths":["a.txt"]})).await,
        "Arguments do not apply to this Git operation",
    );
    refused(
        &read(dir.path(), json!({"operation":"log","ref":"../x"})).await,
        "Invalid Git reference",
    );
    refused(
        &read(dir.path(), json!({"operation":"log","limit":0})).await,
        "Invalid Git option",
    );
    refused(
        &read(
            dir.path(),
            json!({"operation":"diff","paths":["a.txt","a.txt"]}),
        )
        .await,
        "Specify 1 to 64 unique exact paths",
    );
    refused(
        &read(dir.path(), json!({"operation":"status","bogus":1})).await,
        "Invalid typed Git arguments",
    );
    refused(
        &read(dir.path(), json!({"operation":"status","repo":"unsafe"})).await,
        "Unsafe Git configuration",
    );
    refused(
        &read(dir.path(), json!({"operation":"show","repo":"leaky"})).await,
        "Git output withheld by credential guard",
    );
    std::os::unix::fs::symlink("webagent-v3", dir.path().join("linked")).expect("link");
    refused(
        &read(dir.path(), json!({"operation":"status","repo":"linked"})).await,
        "Symlink repository unavailable",
    );
}

async fn the_watch_notices_uncommitted_work() {
    if !available() {
        return;
    }
    let dir = workspace();
    let base = std::fs::canonicalize(dir.path()).expect("base");
    let watched = webagent_worker::git::watch(&base).await;
    assert_eq!(watched["status"], "success", "{watched}");
    assert_eq!(watched["output"]["notify"], true);
    assert_eq!(watched["output"]["ahead"], 2);
    let empty = tempfile::tempdir().expect("empty");
    let missing =
        webagent_worker::git::watch(&std::fs::canonicalize(empty.path()).expect("base")).await;
    assert_eq!(
        missing,
        json!({"status":"failed","output":"Ordinary workspace repository required"})
    );
}

async fn pushes_scan_outgoing_history_and_conflicts_resolve() {
    if !available() {
        return;
    }
    let dir = workspace();
    let base = std::fs::canonicalize(dir.path()).expect("base");
    let call = |name: &'static str, args: Value| {
        let base = base.clone();
        async move { webagent_worker::git::execute(&base, name, &args, &Value::Null).await }
    };
    let leaky = base.join("leaky");
    git(&base, &["init", "-q", "--bare", "leaky-remote.git"]);
    git(&leaky, &["remote", "add", "origin", "../leaky-remote.git"]);
    let head = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&leaky)
            .output()
            .expect("head")
            .stdout,
    )
    .expect("head");
    let blocked = call(
        "git_tool",
        json!({"operation":"push","repo":"leaky","expected_head":head.trim()}),
    )
    .await;
    assert_eq!(blocked["error_kind"], "git_credential_guard", "{blocked}");
    assert_eq!(blocked["diagnostics"]["path"], "conf");
    assert_eq!(blocked["diagnostics"]["line"], 1);
    let conflict = base.join("conflict");
    std::fs::create_dir_all(&conflict).expect("repo");
    git(&conflict, &["init", "-q"]);
    std::fs::write(conflict.join("f.txt"), "base\n").expect("file");
    git(&conflict, &["add", "f.txt"]);
    git(&conflict, &["commit", "-qm", "base"]);
    git(&conflict, &["checkout", "-qb", "other"]);
    std::fs::write(conflict.join("f.txt"), "theirs\n").expect("file");
    git(&conflict, &["commit", "-qam", "theirs"]);
    git(&conflict, &["checkout", "-q", "main"]);
    std::fs::write(conflict.join("f.txt"), "ours\n").expect("file");
    git(&conflict, &["commit", "-qam", "ours"]);
    let _ = Command::new("git")
        .args([
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@t",
            "merge",
            "-q",
            "other",
        ])
        .current_dir(&conflict)
        .output();
    let marked = std::fs::read(conflict.join("f.txt")).expect("conflict");
    let digest = {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(&marked))
    };
    let resolved = call(
        "resolve_conflict",
        json!({"repo":"conflict","path":"f.txt","strategy":"both","expected_sha256":digest}),
    )
    .await;
    assert_eq!(resolved["status"], "success", "{resolved}");
    assert_eq!(
        std::fs::read_to_string(conflict.join("f.txt")).expect("file"),
        "ours\ntheirs\n"
    );
}

async fn the_file_manager_lists_checkouts_past_the_read_cap() {
    if !available() {
        return;
    }
    let dir = tempfile::tempdir().expect("checkout");
    let root = std::fs::canonicalize(dir.path()).expect("root");
    git(&root, &["init", "-q"]);
    // 1500 tracked paths: about 90 KiB of `ls-files -z`, past the 64 KiB read cap.
    for index in 0..1500 {
        let folder = root.join(format!("folder-with-a-long-name-{:02}", index % 50));
        std::fs::create_dir_all(&folder).expect("folder");
        std::fs::write(
            folder.join(format!("file-with-a-long-name-{index:04}.txt")),
            "x\n",
        )
        .expect("file");
    }
    git(&root, &["add", "."]);
    git(&root, &["commit", "-qm", "many files"]);
    let listed = webagent_worker::git::manager::action(
        &root,
        &json!({"action":"git_tree_status"}),
        &Value::Null,
    )
    .await;
    assert_eq!(listed["status"], "success", "{listed}");
    assert_eq!(
        listed["output"]["tracked"].as_array().map(Vec::len),
        Some(1500)
    );
}

fn main() {
    harness::main(&[
        ("status_reports_changes_with_a_stable_review_digest", || {
            Box::pin(status_reports_changes_with_a_stable_review_digest())
        }),
        (
            "reads_return_git_text",
            || Box::pin(reads_return_git_text()),
        ),
        ("unsafe_input_and_repositories_are_refused", || {
            Box::pin(unsafe_input_and_repositories_are_refused())
        }),
        ("the_file_manager_lists_checkouts_past_the_read_cap", || {
            Box::pin(the_file_manager_lists_checkouts_past_the_read_cap())
        }),
        ("the_watch_notices_uncommitted_work", || {
            Box::pin(the_watch_notices_uncommitted_work())
        }),
        ("pushes_scan_outgoing_history_and_conflicts_resolve", || {
            Box::pin(pushes_scan_outgoing_history_and_conflicts_resolve())
        }),
    ]);
}
