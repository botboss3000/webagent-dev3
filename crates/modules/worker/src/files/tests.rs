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
