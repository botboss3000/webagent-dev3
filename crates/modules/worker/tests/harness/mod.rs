//! A small runner for this crate's integration tests (`harness = false`). The test
//! executable is also the per-call supervisor (`--runner ARGV` / `--plugin SPEC` /
//! `--terminal-supervisor SPEC`), because
//! the broker replays retained test executables without Cargo's other build targets.
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;

pub type Test = (&'static str, fn() -> Pin<Box<dyn Future<Output = ()>>>);

fn exe() -> String {
    std::env::current_exe()
        .expect("test executable")
        .to_string_lossy()
        .into_owned()
}

/// The command supervisor's command line: this executable in runner mode.
#[allow(dead_code, reason = "not every test file starts commands")]
pub fn runner() -> Vec<String> {
    vec![exe(), "--runner".into()]
}

/// The plugin supervisor's command line: this executable in plugin mode.
#[allow(dead_code, reason = "not every test file starts plugins")]
pub fn plugin_runner() -> Vec<String> {
    vec![exe(), "--plugin".into()]
}

/// Runs as a supervisor when asked to; otherwise runs every test whose name contains a
/// positional filter (all without one), each on its own runtime, and exits 101 on failure.
/// Flags such as `--test-threads=1` are accepted and ignored: tests always run one by one.
pub fn main(tests: &[Test]) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let spec = args.get(1).cloned().unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("--runner") => std::process::exit(webagent_worker::runner::supervise(&spec)),
        Some("--plugin") => std::process::exit(webagent_worker::runner::supervise_plugin(&spec)),
        Some("--terminal-supervisor") => {
            std::process::exit(webagent_worker::terminal::supervise(&spec))
        }
        _ => {}
    }
    let filters: Vec<&String> = args.iter().filter(|arg| !arg.starts_with('-')).collect();
    let selected: Vec<&Test> = tests
        .iter()
        .filter(|(name, _)| filters.is_empty() || filters.iter().any(|f| name.contains(f.as_str())))
        .collect();
    println!("\nrunning {} tests", selected.len());
    let mut failed = Vec::new();
    for (name, test) in &selected {
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(test());
        }));
        println!(
            "test {name} ... {}",
            if outcome.is_ok() { "ok" } else { "FAILED" }
        );
        if outcome.is_err() {
            failed.push(*name);
        }
    }
    let passed = selected.len() - failed.len();
    if failed.is_empty() {
        println!("\ntest result: ok. {passed} passed; 0 failed\n");
    } else {
        println!("\nfailures:\n    {}\n", failed.join("\n    "));
        println!(
            "test result: FAILED. {passed} passed; {} failed\n",
            failed.len()
        );
        std::process::exit(101);
    }
}
