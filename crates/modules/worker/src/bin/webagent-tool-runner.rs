//! The per-call supervisors as their own executable, for the worker's integration tests;
//! the container runs the same code as `webagent-v3 --tool-runner` / `--plugin-runner`.
fn main() {
    let mut args = std::env::args().skip(1);
    let first = args.next().unwrap_or_default();
    std::process::exit(if first == "--plugin" {
        webagent_worker::runner::supervise_plugin(&args.next().unwrap_or_default())
    } else {
        webagent_worker::runner::supervise(&first)
    });
}
