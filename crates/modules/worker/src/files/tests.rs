use super::*;

const SESSION: &str = "2244991a-c2e3-415b-ae5c-054570c91d32";

struct Fixture {
    _dir: tempfile::TempDir,
    roots: Roots,
    workspace: PathBuf,
}

/// A worker layout without a live checkout, so relative paths resolve in the workspace.
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("temporary directory");
    let control = dir.path().join("control");
    std::fs::create_dir_all(&control).expect("control");
    std::fs::write(control.join("dispatch.lock"), "").expect("lock");
    std::fs::write(control.join("shutoff"), "0").expect("shutoff");
    let workspaces = dir.path().join("workspaces");
    std::fs::create_dir_all(&workspaces).expect("workspaces");
    let roots = Roots {
        workspaces: workspaces.clone(),
        live_checkout: dir.path().join("absent-checkout"),
        control,
        context_config: dir.path().join("absent-context.json"),
        port_claims: dir.path().join("port-claims"),
    };
    Fixture {
        workspace: std::fs::canonicalize(&workspaces)
            .expect("workspaces")
            .join(SESSION),
        _dir: dir,
        roots,
    }
}

impl Fixture {
    fn call(&self, name: &str, arguments: Value) -> Value {
        call(
            &self.roots,
            &json!({"version":2,"id":"t","name":name,"workspace":SESSION,"arguments":arguments}),
        )
    }

    fn path(&self, relative: &str) -> String {
        self.workspace.join(relative).to_string_lossy().into_owned()
    }
}

#[test]
fn write_read_list_edit_and_search_a_workspace_file() {
    let fixture = fixture();
    let path = fixture.path("notes/a.txt");
    let written = fixture.call("file_write", json!({"path":path,"content":"one\ntwo\n"}));
    assert_eq!(written["status"], "success", "{written}");
    assert_eq!(written["output"], "written");
    assert_eq!(written["implementation_version"], IMPLEMENTATION_VERSION);

    let read = fixture.call("file_read", json!({"path":path}));
    assert_eq!(read["output"], "one\ntwo\n");
    assert_eq!(read["total_lines"], 2);
    assert_eq!(read["next_offset"], Value::Null);

    let listed = fixture.call("file_list", json!({"path":fixture.path("notes")}));
    assert_eq!(listed["output"], "a.txt");
    assert_eq!(listed["has_more"], false);

    let edited = fixture.call(
        "file_edit",
        json!({"path":path,"old_text":"two","new_text":"three"}),
    );
    assert_eq!(edited["status"], "success", "{edited}");
    assert_eq!(
        edited["previous_sha256"],
        hex::encode(Sha256::digest(b"one\ntwo\n"))
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("file"),
        "one\nthree\n"
    );

    let found = fixture.call(
        "file_search",
        json!({"path":fixture.path("notes"),"query":"three"}),
    );
    assert_eq!(found["status"], "success", "{found}");
    assert!(found["output"]
        .as_str()
        .expect("text")
        .ends_with("a.txt:2:three"));
}

#[test]
fn a_new_file_gets_the_workspace_mode_and_an_existing_one_keeps_its_mode() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = fixture();
    let path = fixture.path("mode.txt");
    fixture.call("file_write", json!({"path":path,"content":"a"}));
    let mode = std::fs::metadata(&path).expect("file").permissions().mode() & 0o777;
    assert_eq!(mode, 0o660);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).expect("chmod");
    fixture.call("file_write", json!({"path":path,"content":"b"}));
    let mode = std::fs::metadata(&path).expect("file").permissions().mode() & 0o777;
    assert_eq!(mode, 0o640);
}

#[test]
fn reads_page_by_lines_and_report_the_next_offset() {
    let fixture = fixture();
    let path = fixture.path("lines.txt");
    fixture.call("file_write", json!({"path":path,"content":"1\n2\n3\n"}));
    let page = fixture.call("file_read", json!({"path":path,"offset":2,"limit":1}));
    assert_eq!(page["output"], "2\n");
    assert_eq!(page["next_offset"], 3);
    assert_eq!(page["truncated"], true);
    let bad = fixture.call("file_read", json!({"path":path,"offset":0}));
    assert_eq!(bad["output"], "offset must be a positive line number");
    let bad = fixture.call("file_read", json!({"path":path,"limit":true}));
    assert_eq!(bad["output"], "limit must be between 1 and 2000 lines");
}

#[test]
fn an_edit_must_match_exactly_once_and_respects_the_expected_hash() {
    let fixture = fixture();
    let path = fixture.path("twice.txt");
    fixture.call("file_write", json!({"path":path,"content":"x x"}));
    let twice = fixture.call(
        "file_edit",
        json!({"path":path,"old_text":"x","new_text":"y"}),
    );
    assert_eq!(twice["output"], "old_text must match exactly once");
    let stale = fixture.call(
        "file_edit",
        json!({"path":path,"old_text":"x x","new_text":"y","expected_sha256":"0".repeat(64)}),
    );
    assert_eq!(stale["output"], "file changed since inspection");
}

#[test]
fn symlinks_are_not_followed_after_resolution_and_specials_are_refused() {
    let fixture = fixture();
    std::fs::create_dir_all(&fixture.workspace).expect("workspace");
    let real = fixture.path("real.txt");
    std::fs::write(&real, "secret").expect("file");
    // A resolved symlink is read through its target, as Path.resolve() does.
    std::os::unix::fs::symlink(&real, fixture.path("link.txt")).expect("link");
    let read = fixture.call("file_read", json!({"path":fixture.path("link.txt")}));
    assert_eq!(read["output"], "secret");
    let folder = fixture.call(
        "file_read",
        json!({"path":fixture.workspace.to_string_lossy()}),
    );
    assert_eq!(folder["status"], "failed");
}

#[test]
fn shutoff_stops_every_call() {
    let fixture = fixture();
    std::fs::write(fixture.roots.control.join("shutoff"), "1").expect("shutoff");
    let result = fixture.call("file_list", json!({"path":"."}));
    assert_eq!(result["output"], "execution disabled");
    assert!(result.get("implementation_version").is_none());
}

#[test]
fn workspace_identifiers_must_be_canonical_uuids() {
    let fixture = fixture();
    let upper = call(
        &fixture.roots,
        &json!({"name":"file_list","workspace":SESSION.to_uppercase(),"arguments":{}}),
    );
    assert_eq!(upper["output"], "invalid workspace identifier");
    let junk = call(
        &fixture.roots,
        &json!({"name":"file_list","workspace":"../etc","arguments":{}}),
    );
    assert_eq!(junk["output"], "badly formed hexadecimal UUID string");
}

#[test]
fn relative_writes_and_task_scopes_belong_to_the_broker() {
    let fixture = fixture();
    let relative = fixture.call("file_write", json!({"path":"src/a.rs","content":"x"}));
    assert_eq!(
        relative["output"],
        "Only file effects execute in the plugin process"
    );
    let private = fixture.call("file_write", json!({"path":"data/a","content":"x"}));
    assert_eq!(
        private["output"],
        "Source path excluded; use the owning runtime/workspace API"
    );
    let task = fixture.call("file_read", json!({"path":"a","task_id":"t"}));
    assert_eq!(
        task["output"],
        "Only file effects execute in the plugin process"
    );
    let denied = fixture.call(
        "file_read",
        json!({"path":fixture.path("a"),"request_id":"r"}),
    );
    assert_eq!(
        denied["output"],
        "Task write arguments require task_id; live fallback denied"
    );
}

#[test]
fn checkout_edits_refuse_runtime_state_and_ambiguous_requests() {
    let fixture = fixture();
    let state = fixture.call("checkout_edit", json!({"path":"data/x","content":"x"}));
    assert_eq!(
        state["output"],
        "Path must be a live-checkout source path outside runtime state"
    );
    let both = fixture.call(
        "checkout_edit",
        json!({"path":"src/x.rs","content":"x","old_text":"a"}),
    );
    assert_eq!(
        both["output"],
        "Pass content to write a file, or old_text and new_text to edit one"
    );
}

#[test]
fn search_honours_globs_comments_and_regex_options() {
    let fixture = fixture();
    let folder = fixture.path("code");
    fixture.call(
        "file_write",
        json!({"path":fixture.path("code/a.rs"),"content":"// note here\nlet note = 1;\n"}),
    );
    fixture.call(
        "file_write",
        json!({"path":fixture.path("code/b.py"),"content":"note = 2\n"}),
    );
    let globbed = fixture.call(
        "file_search",
        json!({"path":folder,"query":"note","file_glob":"*.py"}),
    );
    assert!(globbed["output"]
        .as_str()
        .expect("text")
        .ends_with("b.py:1:note = 2"));
    let comments = fixture.call(
        "file_search",
        json!({"path":folder,"query":"note","comments_only":true}),
    );
    let comments = comments["output"].as_str().expect("text");
    assert!(comments.ends_with("a.rs:1:// note here"), "{comments}");
    assert_eq!(comments.lines().count(), 1);
    let empty = fixture.call("file_search", json!({"path":folder,"query":""}));
    assert_eq!(empty["output"], "query must not be empty");
}

#[test]
fn globs_follow_fnmatch() {
    assert!(glob_matches("a/b.rs", "*.rs"));
    assert!(glob_matches("b.rs", "?.rs"));
    assert!(glob_matches("b.rs", "[a-c].rs"));
    assert!(!glob_matches("d.rs", "[a-c].rs"));
    assert!(glob_matches("d.rs", "[!a-c].rs"));
    assert!(glob_matches("[x", "[x"));
    assert!(!glob_matches("b.rs", "*.py"));
}

#[test]
fn lines_split_like_python() {
    assert_eq!(split_lines("a\r\nb\rc\n"), ["a", "b", "c"]);
    assert_eq!(split_lines("a\u{2028}b"), ["a", "b"]);
    assert!(split_lines("").is_empty());
}

#[test]
fn resolution_keeps_missing_tails_and_applies_parent_steps() {
    assert_eq!(
        resolve(Path::new("/no-such-root/a/../b")),
        PathBuf::from("/no-such-root/b")
    );
    assert_eq!(resolve(Path::new("/")), PathBuf::from("/"));
}

#[test]
fn decision_candidates_page_glob_and_excerpt() {
    let fixture = fixture();
    for (path, content) in [
        ("d/a.md", "alpha"),
        ("d/b.txt", "beta"),
        ("d/sub/c.md", "gamma"),
    ] {
        fixture.call(
            "file_write",
            json!({"path":fixture.path(path),"content":content}),
        );
    }
    let folder = fixture.path("d");
    let all = fixture.call("decision_candidates", json!({"path":folder}));
    assert_eq!(all["status"], "success", "{all}");
    assert_eq!(
        all["output"],
        json!([{"name":"a.md","kind":"file"},{"name":"b.txt","kind":"file"},
            {"name":"sub","kind":"directory"}])
    );
    assert_eq!(all["next_offset"], Value::Null);
    assert!(all.get("implementation_version").is_none());
    let deep = fixture.call(
        "decision_candidates",
        json!({"path":folder,"file_glob":"*.md","recursive":true,"include_content":true}),
    );
    assert_eq!(
        deep["output"],
        json!([{"name":"a.md","kind":"file","snippet":"alpha","content_truncated":false},
            {"name":"sub/c.md","kind":"file","snippet":"gamma","content_truncated":false}])
    );
    let refused = fixture.call(
        "decision_candidates",
        json!({"path":folder,"recursive":true}),
    );
    assert_eq!(
        refused["output"],
        "recursive and include_content require file_glob"
    );
    let refused = fixture.call(
        "decision_candidates",
        json!({"path":folder,"file_glob":"a/b"}),
    );
    assert_eq!(
        refused["output"],
        "file_glob must be a basename pattern of at most 128 characters"
    );
}

#[test]
fn reads_page_beyond_the_first_mebibyte() {
    let fixture = fixture();
    let content: String = (0..70000)
        .map(|index| format!("line-{index:05}: source context\n"))
        .collect();
    assert!(content.len() > LIMIT);
    std::fs::create_dir_all(&fixture.workspace).expect("workspace");
    std::fs::write(fixture.workspace.join("deep.rs"), &content).expect("file");
    let path = fixture.path("deep.rs");
    let page = fixture.call("file_read", json!({"path":path,"offset":60001,"limit":3}));
    let expected: String = (60000..60003)
        .map(|index| format!("line-{index:05}: source context\n"))
        .collect();
    assert_eq!(page["output"], expected);
    assert_eq!(page["total_lines"], 70000);
    assert_eq!(page["next_offset"], 60004);
    assert_eq!(page["truncated"], true);
    let next = fixture.call("file_read", json!({"path":path,"offset":60004,"limit":2}));
    assert!(next["output"]
        .as_str()
        .expect("text")
        .contains("line-60003"));
    assert_eq!(next["next_offset"], 60006);
    let bad = fixture.call("file_read", json!({"path":path,"limit":2001}));
    assert_eq!(bad["output"], "limit must be between 1 and 2000 lines");
}

#[test]
fn search_and_listing_stop_at_their_budgets() {
    let fixture = fixture();
    std::fs::create_dir_all(&fixture.workspace).expect("workspace");
    let big = vec![b'x'; LIMIT];
    for index in 0..34 {
        std::fs::write(fixture.workspace.join(format!("{index:02}.txt")), &big).expect("file");
    }
    let found = fixture.call(
        "file_search",
        json!({"path":fixture.workspace.to_string_lossy(),"query":"missing"}),
    );
    assert_eq!(found["truncated"], true, "{found}");
    assert!(
        found["output"]
            .as_str()
            .is_some_and(|text| text.contains("budget")),
        "{found}"
    );
    let many = fixture.workspace.join("many");
    std::fs::create_dir_all(&many).expect("folder");
    for index in 0..10001 {
        std::fs::write(many.join(index.to_string()), "").expect("file");
    }
    let listed = fixture.call("file_list", json!({"path":many.to_string_lossy()}));
    assert_eq!(listed["truncated"], true);
}

#[test]
fn tools_reach_only_this_workspace_and_checkout_source() {
    let mut fixture = fixture();
    let checkout = fixture._dir.path().join("checkout");
    for folder in ["src", "data", "build-jobs"] {
        std::fs::create_dir_all(checkout.join(folder)).expect("checkout");
    }
    std::fs::write(checkout.join("src/a.rs"), "needle source").expect("source");
    std::fs::write(checkout.join("data/key"), "needle secret").expect("state");
    std::fs::write(checkout.join("build-jobs/job.json"), "needle job").expect("job");
    fixture.roots.live_checkout = checkout.clone();
    let other = fixture
        .roots
        .workspaces
        .join("5f0a3a52-6b5e-4a8f-9d2f-7e1c2b3a4d5e");
    std::fs::create_dir_all(&other).expect("other workspace");
    std::fs::write(other.join("theirs.txt"), "theirs").expect("other file");
    let outside = "Path outside this session's workspace and the source checkout";

    let source = fixture.call("file_read", json!({"path":"src/a.rs"}));
    assert_eq!(source["output"], "needle source");
    for path in [
        checkout.join("data/key"),
        checkout.join("build-jobs/job.json"),
        other.join("theirs.txt"),
        PathBuf::from("/etc/hostname"),
    ] {
        let read = fixture.call("file_read", json!({"path":path.to_string_lossy()}));
        assert_eq!(read["output"], outside, "{}", path.display());
    }
    let escape = fixture.call("file_read", json!({"path":"../checkout/data/key"}));
    assert_eq!(escape["output"], outside);
    let write = fixture.call(
        "file_write",
        json!({"path":other.join("new.txt").to_string_lossy(),"content":"x"}),
    );
    assert_eq!(write["output"], outside);
    assert!(!other.join("new.txt").exists());

    let found = fixture.call("file_search", json!({"path":".","query":"needle"}));
    let output = found["output"].as_str().expect("search output");
    assert!(output.contains("src/a.rs"), "{output}");
    assert!(
        !output.contains("secret") && !output.contains("job"),
        "{output}"
    );
    let mine = fixture.call(
        "file_write",
        json!({"path":fixture.path("mine.txt"),"content":"ok"}),
    );
    assert_eq!(mine["status"], "success");
}

#[test]
fn search_treats_a_pipe_as_alternation() {
    // Regex search runs rg, which the deployed worker image has and some test images lack.
    if std::process::Command::new("rg").arg("--version").output().is_err() {
        eprintln!("skipped: rg is absent");
        return;
    }
    let fixture = fixture();
    std::fs::create_dir_all(&fixture.workspace).expect("workspace");
    std::fs::write(fixture.workspace.join("a.txt"), "alpha\nbeta\ngamma\n").expect("file");
    let found = fixture.call(
        "file_search",
        json!({"path":fixture.workspace.to_string_lossy(),"query":"alpha|gamma"}),
    );
    let output = found["output"].as_str().expect("output");
    assert!(
        output.contains(":1:alpha") && output.contains(":3:gamma"),
        "{found}"
    );
}
