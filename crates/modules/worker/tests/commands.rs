//! `shell` and `python` through a running worker, its supervisor (this test executable) and
//! the sandbox. Skipped where the kernel lacks Landlock ABI 6 (some build sandboxes).
mod harness;
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

async fn task_scoped_commands_are_refused() {
    let worker = worker().await;
    let result = worker
        .call("shell", json!({"command":"true","task_id":"t"}), 30)
        .await;
    assert_eq!(result["output"], "Task scope unsupported");
}

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

const DOUBLE_FORK: &str = "import os,time\n\
if os.fork()==0:\n os.setsid()\n if os.fork()!=0: os._exit(0)\n \
time.sleep(2); open('detached-survived','w').write('bad'); os._exit(0)\n";

async fn a_cancelled_command_takes_its_double_forked_new_session_child_with_it() {
    if !sandbox_available() {
        return;
    }
    let worker = worker().await;
    let (stream, ()) = worker
        .send(
            "python",
            json!({"code":format!("{DOUBLE_FORK}time.sleep(30)\n")}),
            60,
        )
        .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    drop(stream);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!worker.workspace().join("detached-survived").exists());
}

async fn a_finished_command_leaves_no_detached_child_behind() {
    if !sandbox_available() {
        return;
    }
    let worker = worker().await;
    let result = worker.call("python", json!({"code":DOUBLE_FORK}), 30).await;
    assert_eq!(result["status"], "success", "{result}");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!worker.workspace().join("detached-survived").exists());
}

async fn commands_read_outside_their_workspace_but_cannot_change_it() {
    if !sandbox_available() {
        return;
    }
    let worker = worker().await;
    let private = worker.dir.path().join("unrelated-private");
    std::fs::write(&private, "PRIVATE_FIXTURE").expect("fixture");
    let read = worker
        .call(
            "shell",
            json!({"command":format!("cat {}", private.display())}),
            30,
        )
        .await;
    assert_eq!(read["output"], "PRIVATE_FIXTURE", "{read}");
    let write = worker
        .call(
            "shell",
            json!({"command":format!("echo changed > {}", private.display())}),
            30,
        )
        .await;
    assert_eq!(write["status"], "failed");
    assert_eq!(
        std::fs::read_to_string(&private).expect("fixture"),
        "PRIVATE_FIXTURE"
    );
}

async fn commands_connect_only_to_web_preview_and_ephemeral_ports() {
    if !sandbox_available() {
        return;
    }
    let worker = worker().await;
    // A fixture listener on an ephemeral port is reachable; one on an internal-service style
    // port outside the allowed ports is not.
    let open = std::net::TcpListener::bind("127.0.0.1:0").expect("ephemeral listener");
    let blocked = (15000..15200)
        .find_map(|port| std::net::TcpListener::bind(("127.0.0.1", port)).ok())
        .expect("blocked-port listener");
    let probe = |port: u16| {
        format!(
            "import socket\ntry:\n socket.create_connection(('127.0.0.1',{port}),2).close();print('open')\nexcept OSError as e:print('denied',e.errno)"
        )
    };
    let reached = worker
        .call(
            "python",
            json!({"code":probe(open.local_addr().expect("address").port())}),
            30,
        )
        .await;
    assert_eq!(
        reached["output"].as_str().map(str::trim),
        Some("open"),
        "{reached}"
    );
    let refused = worker
        .call(
            "python",
            json!({"code":probe(blocked.local_addr().expect("address").port())}),
            30,
        )
        .await;
    assert_eq!(
        refused["output"].as_str().map(str::trim),
        Some("denied 13"),
        "{refused}"
    );
}

async fn shutoff_stops_commands_before_they_start() {
    let worker = worker().await;
    std::fs::write(worker.dir.path().join("control/shutoff"), "1").expect("shutoff");
    let result = worker
        .call("shell", json!({"command":"touch forbidden"}), 30)
        .await;
    assert_eq!(result["output"], "execution disabled");
    assert!(!worker.workspace().join("forbidden").exists());
}

fn main() {
    harness::main(&[
        (
            "a_shell_command_runs_in_its_workspace_with_merged_output",
            || Box::pin(a_shell_command_runs_in_its_workspace_with_merged_output()),
        ),
        ("python_runs_isolated", || Box::pin(python_runs_isolated())),
        (
            "commands_write_only_their_workspace_and_cannot_open_unix_sockets",
            || Box::pin(commands_write_only_their_workspace_and_cannot_open_unix_sockets()),
        ),
        ("a_timeout_stops_the_command_and_its_descendants", || {
            Box::pin(a_timeout_stops_the_command_and_its_descendants())
        }),
        ("a_caller_that_hangs_up_stops_the_command", || {
            Box::pin(a_caller_that_hangs_up_stops_the_command())
        }),
        ("output_is_capped_at_one_mebibyte", || {
            Box::pin(output_is_capped_at_one_mebibyte())
        }),
        ("task_scoped_commands_are_refused", || {
            Box::pin(task_scoped_commands_are_refused())
        }),
        ("a_quiesce_stops_the_workspace_commands", || {
            Box::pin(a_quiesce_stops_the_workspace_commands())
        }),
        (
            "a_cancelled_command_takes_its_double_forked_new_session_child_with_it",
            || Box::pin(a_cancelled_command_takes_its_double_forked_new_session_child_with_it()),
        ),
        ("a_finished_command_leaves_no_detached_child_behind", || {
            Box::pin(a_finished_command_leaves_no_detached_child_behind())
        }),
        (
            "commands_read_outside_their_workspace_but_cannot_change_it",
            || Box::pin(commands_read_outside_their_workspace_but_cannot_change_it()),
        ),
        (
            "commands_connect_only_to_web_preview_and_ephemeral_ports",
            || Box::pin(commands_connect_only_to_web_preview_and_ephemeral_ports()),
        ),
        ("shutoff_stops_commands_before_they_start", || {
            Box::pin(shutoff_stops_commands_before_they_start())
        }),
    ]);
}
