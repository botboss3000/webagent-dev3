//! `ability_plugin_invoke` through a running worker: binding verification against a
//! published package tree, and one protocol frame through the plugin supervisor.
mod harness;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use webagent_worker::plugins::{package, Packages};
use webagent_worker::{files::Roots, serve, Config};

const SESSION: &str = "5e1f7c1a-9a4f-4d0b-9a65-3c1f3b1f2a11";
const RELEASE: &str = "0b1c2d3e-4f50-4617-8899-aabbccddeeff";

struct Fixture {
    dir: tempfile::TempDir,
    packages: Packages,
    root: PathBuf,
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// An echo plugin in Python, published as an immutable release package.
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("temporary directory");
    let base = std::fs::canonicalize(dir.path()).expect("directory");
    let packages = Packages {
        releases: base.join("releases"),
        artifacts: base.join("artifacts"),
    };
    let root = packages
        .releases
        .join(RELEASE)
        .join("v3/plugins/abilities/Core/echo");
    std::fs::create_dir_all(&root).expect("package");
    let manifest = json!({"id":"echo","protocol":1,"dispatch":"isolated","effect":"write",
        "runtime":"python3","entrypoint":"runner.py","schemas":"schemas.json",
        "tools":["echo_text"]});
    std::fs::write(root.join("plugin.json"), manifest.to_string()).expect("manifest");
    std::fs::write(
        root.join("runner.py"),
        "import json,sys\nframe=json.loads(sys.stdin.readline())\n\
         print(json.dumps({'protocol':1,'invocation_id':frame['invocation_id'],\
         'status':'success','output':{'echo':frame['arguments'],'tool':frame['tool']}}))\n",
    )
    .expect("runner");
    std::fs::write(root.join("schemas.json"), "[]").expect("schemas");
    Fixture {
        dir,
        packages,
        root,
    }
}

impl Fixture {
    fn binding(&self) -> Value {
        let read = |name: &str| std::fs::read(self.root.join(name)).expect("package file");
        let hashes = json!([
            hash(&read("plugin.json")),
            hash(&read("runner.py")),
            hash(&read("schemas.json"))
        ]);
        json!({"root":self.root,"id":"echo","name":"echo_text","invocation_id":"inv-1",
            "generation":hash(hashes.to_string().as_bytes()),"generation_scheme":1})
    }
}

fn a_current_binding_verifies_and_runs_its_python_entrypoint() {
    let fixture = fixture();
    let verified = package(&fixture.packages, &fixture.binding()).expect("verified");
    assert_eq!(verified.root, fixture.root);
    assert_eq!(
        verified.argv,
        [
            "/usr/bin/python3",
            "-I",
            "-B",
            &fixture.root.join("runner.py").to_string_lossy()
        ]
    );
}

fn changed_moved_or_foreign_packages_are_refused() {
    let fixture = fixture();
    let binding = fixture.binding();
    let refusal = |binding: &Value| {
        package(&fixture.packages, binding)
            .expect_err("refused")
            .to_string()
    };
    let mut other = binding.clone();
    other["name"] = json!("not_a_tool");
    assert_eq!(refusal(&other), "Stale package binding");
    let mut other = binding.clone();
    other["id"] = json!("other");
    assert_eq!(refusal(&other), "Plugin identity mismatch");
    let link = fixture.dir.path().join("link");
    std::os::unix::fs::symlink(&fixture.root, &link).expect("link");
    let mut other = binding.clone();
    other["root"] = json!(link);
    assert_eq!(
        refusal(&other),
        "Plugin must use an immutable published package"
    );
    std::fs::write(fixture.root.join("runner.py"), "print('changed')\n").expect("runner");
    assert_eq!(refusal(&binding), "Stale package binding");
}

/// Ported from the retired Python bridge's generation test: scheme 2 also pins declared
/// helper files, and scheme 1 bindings stay readable.
fn declared_helpers_are_pinned_and_legacy_bindings_remain_readable() {
    let fixture = fixture();
    let manifest = json!({"id":"echo","protocol":1,"dispatch":"isolated","effect":"write",
        "runtime":"python3","entrypoint":"runner.py","schemas":"schemas.json",
        "tools":["echo_text"],"build":{"inputs":["runner.py","helper.py"],"assets":[]}});
    std::fs::write(fixture.root.join("plugin.json"), manifest.to_string()).expect("manifest");
    std::fs::write(fixture.root.join("helper.py"), "VALUE = 1").expect("helper");
    let read = |name: &str| std::fs::read(fixture.root.join(name)).expect("package file");
    let hashes = json!([
        hash(&read("plugin.json")),
        hash(&read("runner.py")),
        hash(&read("schemas.json"))
    ]);
    let files = json!({"helper.py":hash(&read("helper.py")),"runner.py":hash(&read("runner.py"))});
    let mut binding = fixture.binding();
    binding["generation_scheme"] = json!(2);
    binding["generation"] = json!(hash(json!([hashes, files]).to_string().as_bytes()));
    package(&fixture.packages, &binding).expect("scheme 2 verifies");
    let mut legacy = binding.clone();
    legacy["generation_scheme"] = json!(1);
    legacy["generation"] = json!(hash(hashes.to_string().as_bytes()));
    package(&fixture.packages, &legacy).expect("scheme 1 verifies");
    std::fs::write(fixture.root.join("helper.py"), "VALUE = 2").expect("helper");
    let refusal = package(&fixture.packages, &binding).expect_err("stale");
    assert_eq!(refusal.to_string(), "Stale package binding");
}

async fn one_frame_reaches_the_plugin_and_its_reply_comes_back() {
    if !webagent_worker::sandbox::abi().is_ok_and(|abi| abi >= 6) {
        eprintln!("skipped: Landlock ABI 6 unavailable");
        return;
    }
    let fixture = fixture();
    let control = fixture.dir.path().join("control");
    std::fs::create_dir_all(&control).expect("control");
    std::fs::write(control.join("dispatch.lock"), "").expect("lock");
    std::fs::write(control.join("shutoff"), "0").expect("shutoff");
    std::fs::create_dir_all(fixture.dir.path().join("workspaces")).expect("workspaces");
    let socket = fixture.dir.path().join("tools.sock");
    tokio::spawn(serve(Config {
        socket: socket.clone(),
        client_uid: std::fs::metadata(fixture.dir.path()).expect("dir").uid(),
        files: Roots {
            workspaces: fixture.dir.path().join("workspaces"),
            live_checkout: fixture.dir.path().join("absent-checkout"),
            control,
            context_config: fixture.dir.path().join("absent-context.json"),
            port_claims: fixture.dir.path().join("port-claims"),
        },
        runner: harness::runner(),
        plugin_runner: harness::plugin_runner(),
        packages: fixture.packages.clone(),
        browser: webagent_worker::browser::Settings::container(),
    }));
    for _ in 0..100 {
        if Path::new(&socket).exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut stream = UnixStream::connect(&socket).await.expect("connect");
    let request = json!({"version":2,"id":"p","name":"ability_plugin_invoke","workspace":SESSION,
        "arguments":{"text":"hi"},"trusted":{"ability_plugin":fixture.binding()}});
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .expect("request");
    let mut line = Vec::new();
    BufReader::new(stream)
        .read_until(b'\n', &mut line)
        .await
        .expect("reply");
    let reply: Value = serde_json::from_slice(&line).expect("reply is JSON");
    let result = &reply["result"];
    assert_eq!(result["status"], "success", "{reply}");
    assert_eq!(
        result["output"],
        json!({"echo":{"text":"hi"},"tool":"echo_text"})
    );
    assert_eq!(result["invocation_id"], "inv-1");
    assert_eq!(result["uncertain"], false);
}

fn main() {
    harness::main(&[
        (
            "a_current_binding_verifies_and_runs_its_python_entrypoint",
            || Box::pin(async { a_current_binding_verifies_and_runs_its_python_entrypoint() }),
        ),
        ("changed_moved_or_foreign_packages_are_refused", || {
            Box::pin(async { changed_moved_or_foreign_packages_are_refused() })
        }),
        (
            "declared_helpers_are_pinned_and_legacy_bindings_remain_readable",
            || {
                Box::pin(async {
                    declared_helpers_are_pinned_and_legacy_bindings_remain_readable()
                })
            },
        ),
        (
            "one_frame_reaches_the_plugin_and_its_reply_comes_back",
            || Box::pin(one_frame_reaches_the_plugin_and_its_reply_comes_back()),
        ),
    ]);
}
