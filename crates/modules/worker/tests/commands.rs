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
        self.call_with(name, arguments, timeout, Value::Null).await
    }

    /// A call carrying the `trusted` fields only the app sends.
    async fn call_with(&self, name: &str, arguments: Value, timeout: u64, trusted: Value) -> Value {
        let mut stream = UnixStream::connect(&self.socket).await.expect("connect");
        let request = json!({"version":2,"id":"c","name":name,"workspace":SESSION,
            "arguments":arguments,"timeout_seconds":timeout,"trusted":trusted});
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

async fn a_run_stop_stops_the_workspace_commands() {
    if !sandbox_available() {
        return;
    }
    let worker = worker().await;
    let marker = worker.workspace().join("after-run-stop");
    let (stream, ()) = worker
        .send(
            "shell",
            json!({"command":format!("sleep 2; touch {}", marker.display())}),
            30,
        )
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let stopped = worker.call("run_stop", json!({}), 30).await;
    assert_eq!(stopped, json!({"status":"success"}));
    let mut line = Vec::new();
    BufReader::new(stream)
        .read_until(b'\n', &mut line)
        .await
        .expect("reply");
    let reply: Value = serde_json::from_slice(&line).expect("reply is JSON");
    assert_eq!(reply["result"]["output"], "Session not found");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!marker.exists(), "the command outlived the run stop");
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

/// The Sandbox ability's limits as the app sends them, with `overrides` applied.
fn sandbox_limits(overrides: Value) -> Value {
    let mut sandbox = json!({"timeout_seconds":30,"memory_mb":256,"processes":16,
        "file_mb":1,"open_files":256,"folder_mb":3,"read_kb":64,"room_bytes":u64::MAX});
    for (key, value) in overrides.as_object().expect("overrides") {
        sandbox[key] = value.clone();
    }
    json!({"sandbox":sandbox})
}

impl Worker {
    async fn sandbox(&self, name: &str, arguments: Value) -> Value {
        self.call_with(name, arguments, 60, sandbox_limits(json!({})))
            .await
    }
}

async fn sandbox_tools_need_the_apps_limits() {
    let worker = worker().await;
    let result = worker
        .call("sandbox_shell", json!({"command":"touch made"}), 30)
        .await;
    assert_eq!(result["output"], "Sandbox limits missing", "{result}");
    assert!(!worker.workspace().join("sandbox/made").exists());
}

async fn sandbox_commands_see_only_their_scratch_folder() {
    if !sandbox_available() {
        return;
    }
    let worker = worker().await;
    std::fs::create_dir_all(worker.workspace()).expect("workspace");
    let beside = worker.workspace().join("beside.txt");
    std::fs::write(&beside, "WORKSPACE_FIXTURE").expect("fixture");
    let private = worker.dir.path().join("unrelated-private");
    std::fs::write(&private, "PRIVATE_FIXTURE").expect("fixture");
    let script = format!(
        "pwd; echo ok > inside.txt && cat inside.txt; \
         cat {beside} || echo no-workspace; cat {private} || echo no-private; \
         echo x > {private} || echo no-write; ls /proc >/dev/null 2>&1 || echo no-proc; \
         cat /etc/hostname 2>/dev/null || echo no-etc; ls / >/dev/null 2>&1 || echo no-root; \
         kill -0 $PPID 2>/dev/null || echo no-signal; ln -s {private} link; cat link || echo no-link",
        beside = beside.display(),
        private = private.display()
    );
    let result = worker
        .sandbox("sandbox_shell", json!({"command":script}))
        .await;
    let output = result["output"].as_str().expect("text");
    let scratch = worker.workspace().join("sandbox");
    assert!(
        output.starts_with(&format!("{}\nok\n", scratch.display())),
        "{output}"
    );
    for refused in [
        "no-workspace",
        "no-private",
        "no-write",
        "no-proc",
        "no-etc",
        "no-root",
        "no-signal",
        "no-link",
    ] {
        assert!(output.contains(refused), "{refused} missing: {output}");
    }
    assert!(!output.contains("FIXTURE"), "{output}");
    assert_eq!(
        std::fs::read_to_string(&private).expect("kept"),
        "PRIVATE_FIXTURE"
    );
}

async fn sandbox_commands_have_no_network() {
    if !sandbox_available() {
        return;
    }
    let worker = worker().await;
    let open = std::net::TcpListener::bind("127.0.0.1:0").expect("ephemeral listener");
    let port = open.local_addr().expect("address").port();
    let code = format!(
        "import socket\n\
         for family, kind in [(socket.AF_INET, socket.SOCK_STREAM), (socket.AF_INET, socket.SOCK_DGRAM), \
         (socket.AF_INET6, socket.SOCK_DGRAM), (socket.AF_UNIX, socket.SOCK_STREAM)]:\n \
         try:\n  s = socket.socket(family, kind); s.connect(('127.0.0.1', {port})) if family == socket.AF_INET else None; print('open')\n \
         except OSError as e:\n  print('denied', e.errno)"
    );
    let result = worker.sandbox("sandbox_python", json!({"code":code})).await;
    assert_eq!(
        result["output"], "denied 1\ndenied 1\ndenied 1\ndenied 1\n",
        "{result}"
    );
}

async fn sandbox_commands_run_under_resource_limits() {
    if !sandbox_available() {
        return;
    }
    let worker = worker().await;
    let memory = worker
        .sandbox(
            "sandbox_python",
            json!({"code":"try:\n b = bytearray(600 * 1024 * 1024); print('allocated')\nexcept MemoryError:\n print('memory capped')"}),
        )
        .await;
    assert_eq!(memory["output"], "memory capped\n", "{memory}");
    let large = worker
        .sandbox(
            "sandbox_python",
            json!({"code":"try:\n open('big','wb').write(b'x' * (2 * 1024 * 1024)); print('written')\nexcept OSError as e:\n print('file capped', e.errno)"}),
        )
        .await;
    assert_eq!(large["output"], "file capped 27\n", "{large}");
    let big = std::fs::metadata(worker.workspace().join("sandbox/big")).expect("partial file");
    assert!(big.len() <= 1024 * 1024);
    let forks = worker
        .sandbox(
            "sandbox_python",
            json!({"code":"import os, time\npids = []\ntry:\n for i in range(100):\n  pid = os.fork()\n  if pid == 0:\n   time.sleep(5)\n   os._exit(0)\n  pids.append(pid)\n print('uncapped')\nexcept OSError as e:\n print('capped', len(pids), e.errno)\nfor pid in pids:\n os.kill(pid, 9)"}),
        )
        .await;
    // The kernel limit refuses the fork (errno 11) where it can apply; otherwise the
    // supervisor stops the command once it passes its allowance.
    let output = forks["output"].as_str().expect("text");
    match output.strip_prefix("capped ") {
        Some(rest) => {
            let started: u64 = rest
                .split_whitespace()
                .next()
                .and_then(|count| count.parse().ok())
                .unwrap_or_else(|| panic!("{forks}"));
            assert!(
                started <= 16 && output.trim_end().ends_with(" 11"),
                "{forks}"
            );
        }
        None => {
            assert_eq!(forks["exit_code"], 126, "{forks}");
            assert!(output.contains("more than 16 processes"), "{forks}");
        }
    }
}

async fn sandbox_commands_stop_at_the_folder_cap() {
    if !sandbox_available() {
        return;
    }
    let worker = worker().await;
    let result = worker
        .sandbox(
            "sandbox_shell",
            json!({"command":"i=0; while true; do head -c 900000 /dev/zero > f$i || exit 9; i=$((i+1)); done"}),
        )
        .await;
    assert_eq!(result["exit_code"], 125, "{result}");
    assert!(result["output"]
        .as_str()
        .expect("text")
        .contains("size limit"));
    let refused = worker
        .sandbox("sandbox_shell", json!({"command":"echo more > g"}))
        .await;
    assert!(
        refused["output"]
            .as_str()
            .expect("text")
            .starts_with("Sandbox folder is full"),
        "{refused}"
    );
    let freed = worker
        .sandbox("sandbox_file", json!({"action":"delete","path":"f0"}))
        .await;
    assert_eq!(freed["status"], "success", "{freed}");
}

async fn sandbox_files_round_trip_inside_the_scratch_folder() {
    let worker = worker().await;
    let wrote = worker
        .sandbox(
            "sandbox_file",
            json!({"action":"write","path":"element/index.html","content":"<p>hi</p>"}),
        )
        .await;
    assert_eq!(wrote["status"], "success", "{wrote}");
    let read = worker
        .sandbox(
            "sandbox_file",
            json!({"action":"read","path":"element/index.html"}),
        )
        .await;
    assert_eq!(read["output"], "<p>hi</p>", "{read}");
    let escape = worker
        .sandbox(
            "sandbox_file",
            json!({"action":"write","path":"../outside.txt","content":"x"}),
        )
        .await;
    assert_eq!(escape["status"], "failed");
    assert!(!worker.workspace().join("outside.txt").exists());
    let usage: Value = serde_json::from_slice(
        &std::fs::read(worker.dir.path().join("sandbox-usage.json")).expect("usage"),
    )
    .expect("usage JSON");
    assert!(usage[SESSION].as_u64().expect("bytes") >= 9);
}

async fn saved_elements_are_read_only_in_the_sandbox() {
    if !sandbox_available() {
        return;
    }
    let worker = worker().await;
    let put = json!({"action":"put","path":"mine/toggle","revision":2,"html":"<p>saved</p>"});
    let refused = worker.call("sandbox_element", put.clone(), 30).await;
    assert_eq!(refused["status"], "failed", "{refused}");
    let synced = worker
        .call_with("sandbox_element", put, 30, json!({"element_sync":true}))
        .await;
    assert_eq!(synced["status"], "success", "{synced}");
    let status = worker
        .call_with(
            "sandbox_element",
            json!({"action":"status"}),
            30,
            json!({"element_sync":true}),
        )
        .await;
    assert_eq!(status["output"]["revisions"]["mine/toggle"], 2, "{status}");
    let shell = worker
        .sandbox(
            "sandbox_shell",
            json!({"command":"cat ../saved/mine/toggle/index.html; echo; echo x > ../saved/mine/toggle/index.html || echo read-only"}),
        )
        .await;
    assert_eq!(
        shell["output"]
            .as_str()
            .map(|o| o.contains("<p>saved</p>") && o.contains("read-only")),
        Some(true),
        "{shell}"
    );
    let read = worker
        .sandbox(
            "sandbox_file",
            json!({"action":"read","path":"saved/mine/toggle/index.html"}),
        )
        .await;
    assert_eq!(read["output"], "<p>saved</p>", "{read}");
    let write = worker
        .sandbox(
            "sandbox_file",
            json!({"action":"write","path":"saved/mine/toggle/index.html","content":"x"}),
        )
        .await;
    assert_eq!(write["status"], "failed", "{write}");
    let own = worker
        .sandbox(
            "sandbox_file",
            json!({"action":"write","path":"draft/index.html","content":"<p>draft</p>"}),
        )
        .await;
    assert_eq!(own["status"], "success", "{own}");
    let proposed = worker
        .call_with(
            "sandbox_element",
            json!({"action":"read","folder":"draft"}),
            30,
            json!({"element_sync":true}),
        )
        .await;
    assert_eq!(proposed["output"]["html"], "<p>draft</p>", "{proposed}");
}

fn main() {
    harness::main(&[
        ("saved_elements_are_read_only_in_the_sandbox", || {
            Box::pin(saved_elements_are_read_only_in_the_sandbox())
        }),
        ("sandbox_tools_need_the_apps_limits", || {
            Box::pin(sandbox_tools_need_the_apps_limits())
        }),
        ("sandbox_commands_see_only_their_scratch_folder", || {
            Box::pin(sandbox_commands_see_only_their_scratch_folder())
        }),
        ("sandbox_commands_have_no_network", || {
            Box::pin(sandbox_commands_have_no_network())
        }),
        ("sandbox_commands_run_under_resource_limits", || {
            Box::pin(sandbox_commands_run_under_resource_limits())
        }),
        ("sandbox_commands_stop_at_the_folder_cap", || {
            Box::pin(sandbox_commands_stop_at_the_folder_cap())
        }),
        ("sandbox_files_round_trip_inside_the_scratch_folder", || {
            Box::pin(sandbox_files_round_trip_inside_the_scratch_folder())
        }),
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
        ("a_run_stop_stops_the_workspace_commands", || {
            Box::pin(a_run_stop_stops_the_workspace_commands())
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
