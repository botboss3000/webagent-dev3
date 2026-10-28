//! The toy module in both modes: linked (direct calls) and live (service process).
use anyhow::Result;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use webagent_module_service::{Limits, Process, Service, Services, Supervised};
use webagent_module_toy::{Implementation, Toy, ToyClient, ToyHost, ToyHostServer};

struct Host;

impl ToyHost for Host {
    async fn label(&self) -> Result<String> {
        Ok("hello".into())
    }
}

fn host_services() -> Services {
    Arc::new(|_| vec![Arc::new(ToyHostServer(Arc::new(Host))) as Arc<dyn Service>])
}

fn command(generation: u32) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_webagent-toy-service"));
    command.arg(generation.to_string());
    command
}

fn live(limits: Limits) -> Process {
    Process::spawn(command(0), limits, &host_services()).expect("toy service starts")
}

async fn exercise(toy: &impl Toy) -> Vec<String> {
    vec![
        format!("{:?}", toy.add(2, 3).await.ok()),
        format!(
            "{:?}",
            toy.add(u64::MAX, 1).await.map_err(|e| e.to_string())
        ),
        format!("{:?}", toy.greet("module").await.ok()),
        format!("{:?}", toy.echo("é\n\0".into()).await.ok()),
        format!("{:?}", toy.fail("no").await.map_err(|e| e.to_string())),
    ]
}

#[tokio::test]
async fn linked_and_live_modes_are_equivalent() {
    let linked = Implementation {
        host: Host,
        own_process: false,
        generation: 0,
    };
    let process = live(Limits::default());
    let client = ToyClient(process.peer().clone());
    let expected = exercise(&linked).await;
    assert_eq!(expected[0], "Some(5)", "sanity: {expected:?}");
    assert_eq!(
        expected[2], "Some(\"hello, module\")",
        "callback: {expected:?}"
    );
    assert_eq!(
        exercise(&client).await,
        expected,
        "live replies match linked calls"
    );
}

#[tokio::test]
async fn concurrent_calls_share_one_pipe() {
    let process = live(Limits::default());
    let client = ToyClient(process.peer().clone());
    let slow = client.sleep(300);
    let fast = async {
        let started = std::time::Instant::now();
        assert_eq!(client.add(1, 1).await.ok(), Some(2));
        started.elapsed()
    };
    let (slow, fast) = tokio::join!(slow, fast);
    assert!(slow.is_ok());
    assert!(
        fast < Duration::from_millis(250),
        "a slow call must not block others: {fast:?}"
    );
}

#[tokio::test]
async fn deadline_frame_limit_and_crash_fail_closed() {
    let limits = Limits {
        max_frame_bytes: 4096,
        call_timeout: Duration::from_millis(200),
    };
    let process = live(limits);
    let client = ToyClient(process.peer().clone());
    let timeout = client.sleep(2_000).await.expect_err("deadline");
    assert!(timeout.to_string().contains("timed out"), "{timeout}");
    let oversized = client
        .echo("x".repeat(8192))
        .await
        .expect_err("frame limit");
    assert!(oversized.to_string().contains("frame limit"), "{oversized}");
    // A refused request never reached the wire, so the connection stays usable.
    assert_eq!(
        client.echo("x".repeat(64)).await.ok().map(|s| s.len()),
        Some(64)
    );
    assert!(
        client.exit().await.is_err(),
        "a crashed service fails the call"
    );
    assert!(process.peer().is_closed());
    let after = client.add(1, 2).await.expect_err("closed");
    assert!(after.to_string().contains("unavailable"), "{after}");
}

#[tokio::test]
async fn supervisor_restarts_replaces_withdraws_and_restores() {
    let generation = Arc::new(AtomicU32::new(0));
    let next = generation.clone();
    let supervised = Supervised::new(
        move || command(next.fetch_add(1, Ordering::SeqCst)),
        Limits::default(),
        host_services(),
    );
    let call =
        |process: Arc<Process>| async move { ToyClient(process.peer().clone()).generation().await };
    let first = supervised.current().await.expect("start");
    assert_eq!(call(first.clone()).await.ok(), Some(0));
    // Crash: the next call starts a fresh generation.
    assert!(ToyClient(first.peer().clone()).exit().await.is_err());
    assert_eq!(
        call(supervised.current().await.expect("restart"))
            .await
            .ok(),
        Some(1)
    );
    // Replace: an admitted call finishes on its generation; later calls adopt the new one.
    let admitted = supervised.current().await.expect("current");
    let draining = tokio::spawn({
        let admitted = admitted.clone();
        async move { ToyClient(admitted.peer().clone()).sleep(200).await }
    });
    supervised.replace().await;
    assert_eq!(
        call(supervised.current().await.expect("replace"))
            .await
            .ok(),
        Some(2)
    );
    assert!(
        draining.await.expect("join").is_ok(),
        "admitted work drains on the old generation"
    );
    supervised.withdraw().await;
    let refused = supervised.current().await.err().map(|e| e.to_string());
    assert_eq!(refused.as_deref(), Some("module service withdrawn"));
    supervised.restore().await;
    assert_eq!(
        call(supervised.current().await.expect("restore"))
            .await
            .ok(),
        Some(3)
    );
}

#[tokio::test]
async fn service_has_no_ambient_files_or_network() {
    // The event loop fits the shared seccomp profile, and opening a file is denied.
    let process = live(Limits::default());
    let client = ToyClient(process.peer().clone());
    assert_eq!(client.add(40, 2).await.ok(), Some(42));
    let denied = client
        .open("/etc/hostname")
        .await
        .expect_err("no ambient files");
    assert!(
        denied.to_string().contains("Operation not permitted"),
        "{denied}"
    );
    let linked = Implementation {
        host: Host,
        own_process: false,
        generation: 0,
    };
    assert!(
        linked.open("/etc/hostname").await.is_ok(),
        "linked mode is not sandboxed"
    );
}
