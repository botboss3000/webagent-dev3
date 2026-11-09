//! Terminals and preview tunnels through a running worker, its terminal supervisor (this
//! test executable) and the sandbox. Skipped where the kernel lacks Landlock ABI 6.
mod harness;
use serde_json::{json, Value};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use webagent_worker::{files::Roots, serve, Config};

const SESSION: &str = "6c2e8d2b-1b5f-4e1c-8b76-4d2a4c2b3b21";

struct Worker {
    dir: tempfile::TempDir,
    socket: PathBuf,
}

fn sandbox_available() -> bool {
    match webagent_worker::sandbox::abi() {
        Ok(abi) if abi >= 6 => true,
        _ => {
            eprintln!("skipped: Landlock ABI 6 unavailable");
            false
        }
    }
}

async fn worker() -> Worker {
    let dir = tempfile::tempdir().expect("temporary directory");
    let control = dir.path().join("control");
    std::fs::create_dir_all(&control).expect("control");
    std::fs::write(control.join("dispatch.lock"), "").expect("lock");
    std::fs::write(control.join("shutoff"), "0").expect("shutoff");
    std::fs::create_dir_all(dir.path().join("workspaces")).expect("workspaces");
    let socket = dir.path().join("tools.sock");
    let config = Config {
        socket: socket.clone(),
        client_uid: std::fs::metadata(dir.path()).expect("directory").uid(),
        files: Roots {
            workspaces: dir.path().join("workspaces"),
            live_checkout: dir.path().join("absent-checkout"),
            control,
            context_config: dir.path().join("absent-context.json"),
            port_claims: dir.path().join("port-claims"),
        },
        runner: harness::runner(),
        plugin_runner: harness::plugin_runner(),
        packages: webagent_worker::plugins::Packages::container(),
        browser: webagent_worker::browser::Settings::container(),
    };
    tokio::spawn(serve(config));
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Worker { dir, socket }
}

impl Worker {
    fn workspace(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.path().join("workspaces"))
            .expect("workspaces")
            .join(SESSION)
    }

    async fn call(&self, name: &str, arguments: Value) -> Value {
        let mut stream = UnixStream::connect(&self.socket).await.expect("connect");
        let request = json!({"version":2,"id":"c","name":name,"workspace":SESSION,
            "arguments":arguments});
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
        reply["result"].clone()
    }

    /// Waits until the terminal's whole output contains `text`; returns the last read.
    async fn until(&self, terminal: &str, text: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let read = self
                .call("terminal_read", json!({"terminal_id":terminal,"cursor":0}))
                .await;
            let output = read["output"]["output"].as_str().unwrap_or_default();
            if output.contains(text) {
                return read;
            }
            assert!(Instant::now() < deadline, "{text} never appeared: {read}");
            self.call(
                "terminal_wait",
                json!({"terminal_id":terminal,"timeout_seconds":1}),
            )
            .await;
        }
    }

    async fn open(&self, command: &str) -> String {
        let opened = self.call("terminal_open", json!({"command":command})).await;
        assert_eq!(opened["status"], "success", "{opened}");
        assert_eq!(
            opened["output"]["preview_ports"].as_array().map(Vec::len),
            Some(4)
        );
        opened["output"]["terminal_id"]
            .as_str()
            .expect("terminal id")
            .to_owned()
    }
}

async fn a_terminal_answers_input_after_a_fresh_read() {
    if !sandbox_available() {
        return;
    }
    let worker = worker().await;
    let id = worker
        .open("printf 'READY\\n'; read answer; printf 'RESULT:%s\\n' \"$answer\"; pwd; test -t 0 && echo ON_A_TTY; echo END")
        .await;
    let read = worker.until(&id, "READY").await;
    let sequence = read["output"]["sequence"].clone();
    // A send without the read's sequence is refused; the exact one is accepted once.
    let stale = worker
        .call(
            "terminal_send",
            json!({"terminal_id":id,"expected_sequence":sequence.as_i64().unwrap_or(0)+1,"text":"x\n"}),
        )
        .await;
    assert_eq!(stale["status"], "failed", "{stale}");
    let fresh = worker.until(&id, "READY").await;
    let sent = worker
        .call(
            "terminal_send",
            json!({"terminal_id":id,"expected_sequence":fresh["output"]["sequence"],"text":"acceptance\n"}),
        )
        .await;
    assert_eq!(sent["status"], "success", "{sent}");
    let done = worker.until(&id, "\nEND").await;
    assert!(done["output"]["output"]
        .as_str()
        .unwrap_or_default()
        .contains("RESULT:acceptance"));
    let output = done["output"]["output"].as_str().unwrap_or_default();
    let workspace = worker.workspace();
    assert!(output.contains(&*workspace.to_string_lossy()), "{output}");
    assert!(output.contains("ON_A_TTY"), "{output:?}");
    let listed = worker.call("terminal_list", json!({})).await;
    assert_eq!(
        listed["output"]["terminals"].as_array().map(Vec::len),
        Some(1)
    );
    let closed = worker
        .call("terminal_close", json!({"terminal_id":id}))
        .await;
    assert_eq!(closed["output"]["closed"], true, "{closed}");
    let listed = worker.call("terminal_list", json!({})).await;
    assert_eq!(
        listed["output"]["terminals"].as_array().map(Vec::len),
        Some(0)
    );
}

async fn a_terminal_writes_only_its_workspace() {
    if !sandbox_available() {
        return;
    }
    let worker = worker().await;
    let outside = worker.dir.path().join("outside");
    let command = format!(
        "touch inside && echo INSIDE_OK; touch {} || echo OUTSIDE_DENIED; echo DONE",
        outside.display()
    );
    let id = worker.open(&command).await;
    let read = worker.until(&id, "DONE").await;
    let output = read["output"]["output"].as_str().unwrap_or_default();
    assert!(output.contains("INSIDE_OK"), "{output}");
    assert!(output.contains("OUTSIDE_DENIED"), "{output}");
    assert!(worker.workspace().join("inside").exists());
    assert!(!outside.exists());
}

async fn quiesce_closes_terminals_and_resources_count_them() {
    if !sandbox_available() {
        return;
    }
    let worker = worker().await;
    let id = worker.open("echo UP; sleep 300").await;
    worker.until(&id, "UP").await;
    let counted = worker.call("administrator_resources", json!({})).await;
    assert_eq!(
        counted["output"],
        json!({"terminals":1,"tunnels":0}),
        "{counted}"
    );
    let quiesced = worker.call("administrator_quiesce", json!({})).await;
    assert_eq!(quiesced, json!({"status":"success"}), "{quiesced}");
    let counted = worker.call("administrator_resources", json!({})).await;
    assert_eq!(
        counted["output"],
        json!({"terminals":0,"tunnels":0}),
        "{counted}"
    );
    let missing = worker
        .call("terminal_read", json!({"terminal_id":id}))
        .await;
    assert_eq!(missing["status"], "failed", "{missing}");
}

async fn a_preview_needs_a_port_a_workspace_terminal_listens_on() {
    if !sandbox_available() {
        return;
    }
    let worker = worker().await;
    for port in [8081, 20000] {
        let refused = worker
            .call("start_tunnel", json!({"port":port,"duration_seconds":30}))
            .await;
        assert_eq!(
            refused["error_kind"], "preview_tunnel_rejected",
            "{refused}"
        );
    }
    let status = worker.call("get_tunnel_url", json!({})).await;
    assert_eq!(status["output"]["status"], "absent", "{status}");
}

async fn terminal_arguments_are_checked() {
    let worker = worker().await;
    for (name, arguments) in [
        ("terminal_open", json!({"command":""})),
        ("terminal_open", json!({"command":"true","rows":1})),
        ("terminal_list", json!({"extra":1})),
        ("terminal_read", json!({"terminal_id":"missing"})),
    ] {
        let refused = worker.call(name, arguments).await;
        assert_eq!(refused["status"], "failed", "{name}: {refused}");
    }
}

fn main() {
    harness::main(&[
        ("a_terminal_answers_input_after_a_fresh_read", || {
            Box::pin(a_terminal_answers_input_after_a_fresh_read())
        }),
        ("a_terminal_writes_only_its_workspace", || {
            Box::pin(a_terminal_writes_only_its_workspace())
        }),
        ("quiesce_closes_terminals_and_resources_count_them", || {
            Box::pin(quiesce_closes_terminals_and_resources_count_them())
        }),
        (
            "a_preview_needs_a_port_a_workspace_terminal_listens_on",
            || Box::pin(a_preview_needs_a_port_a_workspace_terminal_listens_on()),
        ),
        ("terminal_arguments_are_checked", || {
            Box::pin(terminal_arguments_are_checked())
        }),
    ]);
}
