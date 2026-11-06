//! The per-command supervisor as its own executable, for the worker's integration tests;
//! the container runs the same code as `webagent-v3 --tool-runner`.
fn main() {
    let argv = std::env::args().nth(1).unwrap_or_default();
    std::process::exit(webagent_worker::runner::supervise(&argv));
}
