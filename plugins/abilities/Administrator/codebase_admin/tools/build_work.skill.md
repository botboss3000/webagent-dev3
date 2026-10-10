# build_work

Use build_work for isolated task lifecycle, review/check/preflight, explicit authorized finish, status and uncertain-operation receipts. See Codebase Admin guidance. File tools auto-create tasks; begin is optional. Never retry uncertain deployment.

check accepts check=check or check=test and timeout_seconds from 1 to 600. It runs cargo check --locked or cargo test --locked -- --test-threads=1 in the pinned Rust image, against read-only isolated task source with separate Cargo output. Supply the current expected_inputs_sha256. Input drift makes results stale. No arbitrary commands, benchmark, formatting, Clippy, or profiling action is provided.
