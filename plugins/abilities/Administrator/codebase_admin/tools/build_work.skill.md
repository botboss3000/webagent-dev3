# build_work

Before the first source tool creates a draft, explicitly begin the task with its
verified originating chat ID in the bounded summary. Follow Codebase Admin’s
Chat provenance for broker work section: report chat ID → task ID to the user
at start, handoff and completion, adding job/release IDs when available. The CLI’s
shared operator workspace is not a chat ID; labels never change ownership.

Use build_work for isolated task lifecycle, review/check/preflight, explicit authorized finish, status and uncertain-operation receipts. See Codebase Admin guidance. File tools can auto-create tasks; explicitly begin first to record chat provenance. Never retry uncertain deployment.

check accepts check=check or check=test and timeout_seconds from 1 to 600. It runs cargo check --locked or cargo test --locked -- --test-threads=1 in the pinned Rust image, against read-only isolated task source with separate Cargo output. Supply the current expected_inputs_sha256. Stable request IDs return a queued job immediately; Rust supplies an ID derived from the pending tool call when omitted. Poll that job receipt, and reuse the same request ID to reconcile a lost response instead of starting another check. Interrupted checks require an explicit new request. Input drift makes results stale. No arbitrary commands, benchmark, formatting, Clippy, or profiling action is provided.

Build completion retains logical ownership. Use reservations/reserve before editing; continue for corrections, acceptance with the verified release commit and checks you performed, then complete to release claims. Handoff transfers to a fresh empty followup. Resources and bounded compiler pause/resume/priority/reconcile share sealed caller authority. See docs/controlled-build-scheduler.md.
