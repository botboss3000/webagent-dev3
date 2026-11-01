# build_work

Before the first source tool creates a draft, explicitly begin the task with its
verified originating chat ID in the bounded summary. Follow Codebase Admin’s
Chat provenance for broker work section: report chat ID → task ID to the user
at start, handoff and completion, adding job/release IDs when available. The CLI’s
shared operator workspace is not a chat ID; labels never change ownership.

Use build_work for isolated task lifecycle, review/check/preflight, explicit authorized finish, status and uncertain-operation receipts. See Codebase Admin guidance. File tools can auto-create tasks; explicitly begin first to record chat provenance. Never retry uncertain deployment.

check accepts check=check (optional feature_profile=default|live) or check=test (optional test_filter, a test-name substring such as runtime::recovery), and timeout_seconds from 1 to 600. check=check runs cargo check --locked in the pinned Rust image; check=test compiles the tests and runs the matching ones single-threaded. Without test_filter it runs the whole suite, which does not fit in 600 seconds, so always filter. Checks run against read-only isolated task source with separate Cargo output. One check runs beside a running build, so check before finish to catch failures early; a second check queues behind the first. Supply the current expected_inputs_sha256. A queued check holds your call until its job ends, like finish, and the chat shows its stages meanwhile; the result carries the status and a log excerpt. Reuse the same request ID to reconcile a lost response instead of starting another check. check=check does not compile #[cfg(test)] modules, so a test-only change needs check=test. Input drift makes results stale. No arbitrary commands, formatting, Clippy or profiling action is provided.

Use reservations/reserve before editing; continue for corrections. A completed release closes itself: the broker commits exactly the released paths and releases the task's claims with that commit, so no acceptance, complete or release call follows. Handoff transfers to a fresh empty followup. Resources and bounded compiler pause/resume/priority/reconcile share sealed caller authority. See docs/controlled-build-scheduler.md.
