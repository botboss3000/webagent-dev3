//! Several agent-loop processes on one state root (multi-loop study, risk 1 and the C5 limits).
//! These tests start real processes, so they are not in the per-release suite (the broker runs it
//! at 8 threads beside other builds, where starting loop processes failed); run them on their own
//! with `cargo test --bin webagent-v3 multi_loop -- --ignored`. They run one at a time.
//! Each test starts real loop processes: this test binary run again as a loop
//! ([`loop_child_process`]), with the same code the loop container runs. A debug-build hook
//! (`V3_TEST_LOOP_FILTER_OFF`) lets two loops visit the same accounts, so the claim, lease and
//! recovery fences are shown to keep every run executing once even when the partition fails.
use super::*;
use crate::runtime_tests::{conversation, fixture};
use std::path::{Path as FsPath, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use webagent_engine::partition::loop_for;

/// How long a test waits for a condition (a loop serving, a run finishing) before it fails: long,
/// so a loaded machine is slower but never fails; each wait ends as soon as the condition holds.
const PATIENCE: Duration = Duration::from_secs(300);

/// One process test at a time: each starts two or three loop processes.
static PROCESSES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The child half: this test binary run as loop `V3_LOOP_INDEX` of `V3_LOOPS`.
#[test]
#[ignore = "started by the multi-loop tests as a loop process"]
fn loop_child_process() {
    if std::env::var_os("V3_TEST_LOOP_CHILD").is_none() {
        return;
    }
    // Few threads: the test machine runs the suite at 8 threads beside other builds.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("loop runtime");
    if let Err(error) = runtime.block_on(crate::run_process()) {
        eprintln!("loop child: {error:#}");
        std::process::exit(1);
    }
}

/// One loop process; killed when dropped so a failed test leaves none behind.
struct Loop(Child);

impl Loop {
    fn start(root: &FsPath, index: u32, loops: u32, filter_off: bool) -> Self {
        Self::start_in(root, &checkout(), index, loops, filter_off)
    }
    /// Start with `code` as the checkout (its `config/` is the live configuration).
    fn start_in(root: &FsPath, code: &FsPath, index: u32, loops: u32, filter_off: bool) -> Self {
        let mut command = Command::new(std::env::current_exe().expect("test binary"));
        command
            .args([
                "--exact",
                "multi_loop_tests::loop_child_process",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("V3_TEST_LOOP_CHILD", "1")
            .env("V3_PROCESS_ROLE", "loop")
            .env("V3_STATE_ROOT", root)
            .env("V3_CONTROL_ROOT", root.join("control"))
            .env("V3_RUNTIME_ROOT", root.join("runtime"))
            .env("V3_WORKSPACE_ROOT", root.join("workspaces"))
            .env("V3_TOOL_SOCKET", root.join("runtime/no-worker.sock"))
            .env("V3_BUILD_SOCKET", root.join("runtime/no-build.sock"))
            .env("V3_ABILITIES_ROOT", checkout().join("abilities"))
            .env("V3_CHECKOUT_ROOT", code)
            .current_dir(code)
            .env("V3_LOOP_INDEX", index.to_string())
            .env("V3_LOOPS", loops.to_string())
            .env_remove("V3_TEST_LOOP_FILTER_OFF")
            .stdin(Stdio::null())
            .stdout(log(root, index))
            .stderr(Stdio::inherit());
        if filter_off {
            command.env("V3_TEST_LOOP_FILTER_OFF", "1");
        }
        // A busy machine can refuse a new process for a moment (EAGAIN): try again, and say why
        // when it keeps refusing.
        let mut attempts = 0;
        loop {
            match command.spawn() {
                Ok(child) => return Self(child),
                Err(error) if attempts < 30 => {
                    attempts += 1;
                    eprintln!("loop process {index} did not start ({error}); retrying");
                    std::thread::sleep(Duration::from_secs(1));
                }
                Err(error) => panic!("loop process {index} did not start: {error}"),
            }
        }
    }
    /// Ask for a drain, as `docker stop` does.
    fn stop(&self) {
        webagent_platform::process::request_stop(self.0.id()).expect("stop request");
    }
    /// Whether the process has exited (and with success).
    fn exited(&mut self) -> Option<bool> {
        self.0
            .try_wait()
            .expect("loop status")
            .map(|status| status.success())
    }
}

impl Drop for Loop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start loops `0..loops` one after another, each once the one before serves (as the
/// supervisor does): processes setting up the same databases at the same moment can meet
/// "database is locked".
async fn start_loops(
    app: &App,
    root: &FsPath,
    code: &FsPath,
    loops: u32,
    filter_off: bool,
) -> Vec<Loop> {
    let mut started = Vec::new();
    for index in 0..loops {
        started.push(Loop::start_in(root, code, index, loops, filter_off));
        let serving = || async {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM loop_members WHERE idx=? AND serving=1",
            )
            .bind(index)
            .fetch_one(&app.store.app_pool)
            .await
            .unwrap_or(0)
                == 1
        };
        assert!(eventually(PATIENCE, serving).await, "loop {index} serves");
    }
    started
}

/// Loop `index`'s log in the test's state root (appended across restarts).
fn log(root: &FsPath, index: u32) -> std::fs::File {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join(format!("loop-{index}.log")))
        .expect("loop log")
}

/// What loop `index` logged, for a failing test's message.
fn problems(root: &FsPath, index: u32) -> String {
    std::fs::read_to_string(root.join(format!("loop-{index}.log")))
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.contains("cache_estimate"))
        .map(|line| line.chars().take(300).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn checkout() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Wait up to `limit` for `done`, checking every 200 ms.
async fn eventually<F, Fut>(limit: Duration, mut done: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = Instant::now();
    while start.elapsed() < limit {
        if done().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

/// The default agent answers with the server-scripted `loadtest` model (no network).
async fn scripted_model(app: &App) {
    let mut catalog = app.store.model_catalog().await.unwrap();
    // The loadtest provider ignores its link value, but a connection needs a stored one.
    let reference = "provider_credential:multi-loop-test";
    let mut link = serde_json::Map::new();
    link.insert(
        concat!("api", "_key").into(),
        json!(concat!("loadtest", "-link")),
    );
    app.store
        .put(reference, &Value::Object(link))
        .await
        .unwrap();
    let connection = catalog.connections.get_mut("default-subscription").unwrap();
    connection.transport = model_selection::Transport::Loadtest;
    connection.credential_ref = reference.into();
    catalog
        .entries
        .get_mut("default-subscription")
        .unwrap()
        .upstream_model = "loadtest-echo".into();
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
}

/// Send `text` in a new chat of `app`'s account; the queued run's id.
async fn message(app: &App, text: &str) -> String {
    let sid = conversation(app).await;
    send(
        State(app.clone()),
        Path(sid),
        Json(json!({"client_message_id":uuid(),"text":text})),
    )
    .await
    .unwrap()
    .1
     .0["run_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn status(app: &App, run: &str) -> String {
    sqlx::query_scalar("SELECT status FROM run_index WHERE id=?")
        .bind(run)
        .fetch_one(&app.store.pool)
        .await
        .unwrap()
}

async fn claims(app: &App, run: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM loop_events WHERE run_id=? AND kind='claimed'")
        .bind(run)
        .fetch_one(&app.store.pool)
        .await
        .unwrap()
}

async fn ready(app: &App) -> Option<u32> {
    app.store.loops_ready().await.unwrap()
}

#[tokio::test]
#[ignore = "starts real loop processes; run on its own: cargo test --bin webagent-v3 multi_loop -- --ignored"]
async fn a_second_process_for_the_same_loop_index_is_refused() {
    let _serial = PROCESSES.lock().await;
    let (root, app) = fixture().await;
    let _first = Loop::start(root.path(), 0, 1, false);
    assert!(eventually(PATIENCE, || async { ready(&app).await == Some(1) }).await);
    let mut second = Loop::start(root.path(), 0, 1, false);
    assert!(
        eventually(PATIENCE, || {
            let exited = second.exited();
            async move { exited == Some(false) }
        })
        .await,
        "the second loop 0 exits on the index lock"
    );
    assert_eq!(ready(&app).await, Some(1));
}

#[tokio::test]
#[ignore = "starts real loop processes; run on its own: cargo test --bin webagent-v3 multi_loop -- --ignored"]
async fn loops_started_with_different_counts_never_serve_at_once() {
    let _serial = PROCESSES.lock().await;
    let (root, app) = fixture().await;
    let mut single = Loop::start(root.path(), 0, 1, false);
    assert!(eventually(PATIENCE, || async { ready(&app).await == Some(1) }).await);
    // Loop 1 of 2 starts while the single loop lives: it stays outside, nothing is written.
    let mut later = Loop::start(root.path(), 1, 2, false);
    tokio::time::sleep(Duration::from_secs(8)).await;
    assert_eq!(later.exited(), None, "it waits at the fence");
    assert_eq!(ready(&app).await, Some(1));
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM loop_members WHERE idx=1")
        .fetch_one(&app.store.app_pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    // The single loop drains and leaves (its index lock with it); loop 1 of 2 joins, and with
    // loop 0 of 2 both serve.
    single.stop();
    assert!(
        eventually(PATIENCE, || {
            let exited = single.exited();
            async move { exited.is_some() }
        })
        .await,
        "the single loop drains and exits"
    );
    let first = Loop::start(root.path(), 0, 2, false);
    assert!(eventually(PATIENCE, || async { ready(&app).await == Some(2) }).await);
    drop(first);
}

#[tokio::test]
#[ignore = "starts real loop processes; run on its own: cargo test --bin webagent-v3 multi_loop -- --ignored"]
async fn two_loops_visiting_one_account_execute_each_run_once() {
    let _serial = PROCESSES.lock().await;
    let (root, app) = fixture().await;
    scripted_model(&app).await;
    let loops = start_loops(&app, root.path(), &checkout(), 2, true).await;
    assert!(eventually(PATIENCE, || async { ready(&app).await == Some(2) }).await);
    let mut runs = Vec::new();
    for _ in 0..6 {
        runs.push(message(&app, "THINK 3000\nREPLY done").await);
    }
    for run in &runs {
        let completed = eventually(PATIENCE, || async {
            status(&app, run).await == "completed"
        })
        .await;
        let saved = app.run(run).await.unwrap();
        assert!(
            completed,
            "run {run}: status {} error {} model error {}",
            saved["status"], saved["error"], saved["last_model_error"]
        );
    }
    for run in &runs {
        assert_eq!(claims(&app, run).await, 1, "one claim for {run}");
        let saved = app.run(run).await.unwrap();
        assert!(saved["restart_interruption"].is_null());
        assert!(saved["error"].is_null(), "{}", saved["error"]);
    }
    drop(loops);
}

#[tokio::test]
#[ignore = "starts real loop processes; run on its own: cargo test --bin webagent-v3 multi_loop -- --ignored"]
async fn a_killed_loops_run_is_taken_back_only_once_its_loop_is_gone() {
    let _serial = PROCESSES.lock().await;
    let (root, app) = fixture().await;
    scripted_model(&app).await;
    let mut loops = start_loops(&app, root.path(), &checkout(), 2, true).await;
    assert!(eventually(PATIENCE, || async { ready(&app).await == Some(2) }).await);
    let run = message(&app, "THINK 40000\nREPLY done").await;
    assert!(eventually(PATIENCE, || async { status(&app, &run).await == "running" }).await);
    // Kill (SIGKILL, no drain) the loop that claimed it.
    let owner: String = sqlx::query_scalar("SELECT lease_owner FROM run_index WHERE id=?")
        .bind(&run)
        .fetch_one(&app.store.pool)
        .await
        .unwrap();
    let index: u32 = sqlx::query_scalar("SELECT idx FROM loop_members WHERE owner=?")
        .bind(&owner)
        .fetch_one(&app.store.app_pool)
        .await
        .unwrap();
    let killed = Instant::now();
    let _ = loops[index as usize].0.kill();
    // The other loop takes it back only after the killed loop's row went stale (8-10 s) or the
    // lease lapsed (30 s), at its next 10-second recovery pass.
    assert!(
        eventually(PATIENCE, || async { claims(&app, &run).await == 2 }).await,
        "claimed again"
    );
    assert!(
        killed.elapsed() >= Duration::from_secs(7),
        "{:?}",
        killed.elapsed()
    );
    let completed = eventually(PATIENCE, || async {
        status(&app, &run).await == "completed"
    })
    .await;
    let saved = app.run(&run).await.unwrap();
    assert!(
        completed,
        "status {} error {} model error {} pending {}",
        saved["status"], saved["error"], saved["last_model_error"], saved["pending"]
    );
    assert_eq!(claims(&app, &run).await, 2);
    let finals: i64 = sqlx::query_scalar("SELECT count(*) FROM chat WHERE terminal_run_id=?")
        .bind(&run)
        .fetch_one(&app.store.pool)
        .await
        .unwrap();
    assert_eq!(finals, 1, "one final reply");
    loops.clear();
}

/// A checkout copy whose live configuration has a ceiling of `ceiling` runs, `reserved` of them
/// kept for administrators.
fn checkout_with_ceiling(root: &FsPath, ceiling: usize, reserved: u32) -> PathBuf {
    fn copy(from: &FsPath, to: &FsPath) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }
    // Everything else of the checkout is linked, so every checkout-relative read still works.
    let code = root.join("code");
    std::fs::create_dir_all(&code).unwrap();
    for entry in std::fs::read_dir(checkout()).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() != "config" {
            std::os::unix::fs::symlink(entry.path(), code.join(entry.file_name())).unwrap();
        }
    }
    copy(&checkout().join("config"), &code.join("config"));
    let path = code.join(webagent_engine::runtime::config::LIVE);
    let mut runtime: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    runtime["scheduler"]["max_active_runs"] = json!(ceiling);
    std::fs::write(&path, serde_json::to_vec_pretty(&runtime).unwrap()).unwrap();
    let path = code.join("config/access/policy.json");
    let mut policy: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    policy["run_limits"]["reserved_admin_runs"] = json!(reserved);
    // Every tier may use the server-scripted test model (as `tools/capacity-test.py` allows).
    if let Some(tiers) = policy["tiers"].as_object_mut() {
        for tier in tiers.values_mut() {
            if let Some(models) = tier["allowed_models"].as_array_mut() {
                models.push(json!("loadtest-echo"));
            }
        }
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&policy).unwrap()).unwrap();
    code
}

/// An ordinary (non-administrator) account with its own view, served by loop `index` of 2.
async fn account_on(app: &App, index: u32, name: &str) -> App {
    for attempt in 0.. {
        let account = app
            .store
            .create_user_account(&format!("{name}{attempt}"), "long-hash-placeholder")
            .await
            .unwrap();
        let id = account["user_id"].as_str().unwrap().to_owned();
        if loop_for(&id, 2) != index {
            continue;
        }
        app.store
            .update_account_access(&id, 1, crate::access::AccountStatus::Active, "free")
            .await
            .unwrap();
        app.store
            .set_agent_membership(
                &id,
                "shared_default",
                crate::access::Relationship::Member,
                0,
            )
            .await
            .unwrap();
        let mut view = app.clone();
        view.store = app.store.for_user(&id).await.unwrap();
        return view;
    }
    unreachable!()
}

#[tokio::test]
#[ignore = "starts real loop processes; run on its own: cargo test --bin webagent-v3 multi_loop -- --ignored"]
async fn the_servers_run_ceiling_and_reserved_slot_hold_across_two_loops() {
    let _serial = PROCESSES.lock().await;
    let (root, app) = fixture().await;
    scripted_model(&app).await;
    // Ceiling 10, 8 slots kept for administrators: 2 ordinary runs at once, over both loops.
    let code = checkout_with_ceiling(root.path(), 10, 8);
    // This test process reads the same policy as the loops (its tiers name the accounts' levels).
    let policy = app.store.access_repository().access_policy_path();
    std::fs::create_dir_all(policy.parent().unwrap()).unwrap();
    std::fs::copy(code.join("config/access/policy.json"), policy).unwrap();
    let _loops = start_loops(&app, root.path(), &code, 2, false).await;
    assert!(eventually(PATIENCE, || async { ready(&app).await == Some(2) }).await);
    let mut users = Vec::new();
    for (index, name) in [(0, "first"), (0, "second"), (1, "third"), (1, "fourth")] {
        users.push(account_on(&app, index, name).await);
    }
    let mut runs = Vec::new();
    for user in &users {
        runs.push((user.clone(), message(user, "THINK 6000\nREPLY done").await));
    }
    let going = || async {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM running_runs")
            .fetch_one(&app.store.app_pool)
            .await
            .unwrap()
    };
    let two = eventually(PATIENCE, || async { going().await == 2 }).await;
    if !two {
        for index in 0..2 {
            eprintln!("loop {index}:\n{}", problems(root.path(), index));
        }
        for (user, run) in &runs {
            let saved = user.run(run).await.unwrap();
            let admission = match user.store.requester_claim_ready(&saved).await {
                Ok(true) => "yes".to_owned(),
                Ok(false) => "no".to_owned(),
                Err(error) => format!("error {error}"),
            };
            let authorized = if user.run_model_authorized(&saved).await {
                "yes"
            } else {
                "no"
            };
            eprintln!(
                "run {run}: status {} error {} model error {} admission {admission:?} model authorized {authorized}",
                saved["status"], saved["error"], saved["last_model_error"]
            );
        }
    }
    assert!(two, "two ordinary runs going at once");
    // The administrator's run starts beside them, in the reserved slot.
    let admin_run = message(&app, "THINK 3000\nREPLY done").await;
    assert!(
        eventually(PATIENCE, || async {
            status(&app, &admin_run).await != "queued"
        })
        .await
    );
    // Never more than 2 ordinary runs at once, until all are done.
    let start = Instant::now();
    let mut done = false;
    while start.elapsed() < PATIENCE && !done {
        let mut running = 0;
        let mut finished = 0;
        for (user, run) in &runs {
            match status(user, run).await.as_str() {
                "running" => running += 1,
                "completed" => finished += 1,
                _ => {}
            }
        }
        assert!(running <= 2, "{running} ordinary runs at once");
        assert!(
            going().await <= 3,
            "the two ordinary runs and the administrator's"
        );
        done = finished == runs.len();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(done, "every run completed");
    // Each run was claimed once, by the loop that serves its account (the administrator: 0).
    let served_by = |user: App, run: String| async move {
        let owner: String = sqlx::query_scalar("SELECT lease_owner FROM run_index WHERE id=?")
            .bind(&run)
            .fetch_one(&user.store.pool)
            .await
            .unwrap();
        sqlx::query_scalar::<_, u32>("SELECT idx FROM loop_members WHERE owner=?")
            .bind(owner)
            .fetch_one(&user.store.app_pool)
            .await
            .unwrap()
    };
    for (user, run) in &runs {
        assert_eq!(claims(user, run).await, 1);
        let expected = loop_for(user.store.current_user_id(), 2);
        assert_eq!(served_by(user.clone(), run.clone()).await, expected);
    }
    assert_eq!(served_by(app.clone(), admin_run.clone()).await, 0);
    assert!(eventually(PATIENCE, || async { going().await == 0 }).await);
}
