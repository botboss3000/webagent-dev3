//! `shell` and `python` through a running worker, its supervisor binary and the sandbox.
//! Skipped where the kernel lacks Landlock ABI 6 (some build sandboxes).
use serde_json::{json, Value};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use webagent_worker::{files::Roots, serve, Config};

const SESSION: &str = "5e1f7c1a-9a4f-4d0b-9a65-3c1f3b1f2a10";

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
        legacy_socket: dir.path().join("legacy.sock"),
        client_uid: std::fs::metadata(dir.path()).expect("directory").uid(),
        files: Roots {
            workspaces: dir.path().join("workspaces"),
            live_checkout: dir.path().join("absent-checkout"),
            control,
            context_config: dir.path().join("absent-context.json"),
            port_claims: dir.path().join("port-claims"),
        },
        runner: vec![env!("CARGO_BIN_EXE_webagent-tool-runner").into()],
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

    async fn send(&self, name: &str, arguments: Value, timeout: u64) -> (UnixStream, ()) {
        let mut stream = UnixStream::connect(&self.socket).await.expect("connect");
        let request = json!({"version":2,"id":"c","name":name,"workspace":SESSION,
            "arguments":arguments,"timeout_seconds":timeout});
        stream
            .write_all(format!("{request}\n").as_bytes())
            .await
            .expect("request");
        (stream, ())
    }

    async fn call(&self, name: &str, arguments: Value, timeout: u64) -> Value {
        let (stream, ()) = self.send(name, arguments, timeout).await;
        let mut line = Vec::new();
        BufReader::new(stream)
            .read_until(b'\n', &mut line)
            .await
            .expect("reply");
        let reply: Value = serde_json::from_slice(&line).expect("reply is JSON");
        reply["result"].clone()
    }
}

#[tokio::test]
async fn a_shell_command_runs_in_its_workspace_with_merged_output() {
    if !sandbox_available() {
        return;
    }
    let worker = worker().await;
    let result = worker
        .call(
            "shell",
            json!({"command":"pwd; echo err >&2; echo \"$HOME\"; echo $V3_ALLOWED_LISTEN_PORTS; exit 3"}),
            30,
        )
        .await;
    assert_eq!(result["status"], "failed", "{result}");
    assert_eq!(result["exit_code"], 3);
    let output = result["output"].as_str().expect("text");
    let workspace = worker.workspace();
    let workspace = workspace.to_string_lossy();
    assert!(
        output.starts_with(&format!("{workspace}\nerr\n{workspace}\n")),
        "{output}"
    );
    assert_eq!(output.lines().nth(3).expect("ports").split(',').count(), 4);
}

#[tokio::test]
async fn python_runs_isolated() {
    if !sandbox_available() {
        return;
    }
    let worker = worker().await;
    let result = worker
        .call(
            "python",
            json!({"code":"import sys; print(sys.flags.isolated)"}),
            30,
        )
        .await;
    assert_eq!(result["status"], "success", "{result}");
    assert_eq!(result["output"], "1\n");
}

#[tokio::test]
async fn commands_write_only_their_workspace_and_cannot_open_unix_sockets() {
    if !sandbox_available() {
        return;
    }
    let worker = worker().await;
    let outside = worker.dir.path().join("outside.txt");
    let script = format!(
        "echo ok > inside.txt && cat inside.txt; echo no > {} || echo denied",
        outside.display()
    );
    let result = worker.call("shell", json!({"command":script}), 30).await;
    let output = result["output"].as_str().expect("text");
    assert!(
        output.starts_with("ok\n") && output.contains("denied"),
        "{output}"
    );
    assert!(!outside.exists());
    let result = worker
        .call(
            "python",
            json!({"code":"import socket\ntry:\n socket.socket(socket.AF_UNIX)\n print('open')\nexcept PermissionError:\n print('fenced')"}),
            30,
        )
        .await;
    assert_eq!(result["output"], "fenced\n", "{result}");
}

#[tokio::test]
async fn a_timeout_stops_the_command_and_its_descendants() {
    if !sandbox_available() {
        return;
    }
    let worker = worker().await;
    let marker = worker.workspace().join("late");
    let started = Instant::now();
    let result = worker
        .call(
            "shell",
            json!({"command":format!("(sleep 3; touch {}) & sleep 30", marker.display())}),
            1,
        )
        .await;
    assert_eq!(result["status"], "timeout", "{result}");
    assert!(started.elapsed() < Duration::from_secs(10));
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(!marker.exists(), "a descendant outlived the timeout");
}

#[tokio::test]
async fn a_caller_that_hangs_up_stops_the_command() {
    if !sandbox_available() {
        return;
    }
    let worker = worker().await;
    let marker = worker.workspace().join("after-hang-up");
    let (stream, ()) = worker
        .send(
            "shell",
            json!({"command":format!("sleep 2; touch {}", marker.display())}),
            30,
        )
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(stream);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!marker.exists(), "the command outlived its caller");
}

#[tokio::test]
async fn output_is_capped_at_one_mebibyte() {
    if !sandbox_available() {
        return;
    }
    let worker = worker().await;
    let result = worker
        .call("python", json!({"code":"print('x' * 2000000)"}), 30)
        .await;
    assert_eq!(result["truncated"], true);
    assert_eq!(result["output"].as_str().expect("text").len(), 1024 * 1024);
}

#[tokio::test]
async fn task_scoped_commands_are_refused() {
    let worker = worker().await;
    let result = worker
        .call("shell", json!({"command":"true","task_id":"t"}), 30)
        .await;
    assert_eq!(result["output"], "Task scope unsupported");
}

#[tokio::test]
async fn a_quiesce_stops_the_workspace_commands() {
    if !sandbox_available() {
        return;
    }
    let worker = worker().await;
    let marker = worker.workspace().join("after-quiesce");
    let (stream, ()) = worker
        .send(
            "shell",
            json!({"command":format!("sleep 2; touch {}", marker.display())}),
            30,
        )
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    // No Python worker here: the quiesce itself fails after stopping the command.
    worker.call("administrator_quiesce", json!({}), 30).await;
    let mut line = Vec::new();
    BufReader::new(stream)
        .read_until(b'\n', &mut line)
        .await
        .expect("reply");
    let reply: Value = serde_json::from_slice(&line).expect("reply is JSON");
    assert_eq!(reply["result"]["output"], "Session not found");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!marker.exists(), "the command outlived the quiesce");
}
