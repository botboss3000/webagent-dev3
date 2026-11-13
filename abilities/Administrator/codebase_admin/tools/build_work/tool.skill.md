# build_work

The isolated broker task lifecycle: begin, status, review, check, preflight,
finish, decide, receipt, wait, cancel and abandon. File tools create a task
automatically; begin explicitly only to set a summary. Creating or editing a
task never builds or deploys.

## Checks before finish

A full build takes about 25 minutes and fails late, so prove a Rust change first
on the same unchanged input digest (from a fresh review, list or read):

1. check=check with feature_profile=default.
2. check=check with feature_profile=live when you changed shared host code,
   engine integration, service wrappers, feature-gated paths or
   implementation-crate dependencies. The live build deliberately leaves out
   replaceable implementation crates; never add a dependency to make it compile.
3. check=test with test_filter (a test-name substring such as
   runtime::recovery) for each changed module's tests and any new test by name.
   Without a filter the whole suite does not fit in 600 seconds. check=check
   skips `#[cfg(test)]`, so a test-only change needs check=test.
4. check=fmt and check=clippy. They report only on the Rust files your task
   changed; fmt never rewrites, so apply its formatting with your own edit.
5. check=ui_test for UI changes: it runs the browser fixtures the changed paths
   select and its log excerpt carries page errors a failed release may cut off.

Every build_work result has `status` at the top. On success `output` is an object
with the action's fields (`inputs_sha256`, `job_id`, ...). When a call failed, was
held (`error_kind`) or is running in the background, `output` is a message string:
read `status` first, and for a running release take `job_id`, `broker_task_id` and
`inputs_sha256` from the top level (`task_id` there is the background task).

When a change alters what a route or query returns, lists or counts (for example
hiding empty sessions), search the whole test suite for tests that assert the old
result (the route path, the field, `[0]` row checks) and run check=test on each
match before finish. The release runs every test; a missed fixture costs a full cycle.

When a release fails, read `failing_tests` and `failure_line` in the finish, wait
or status result (a failure notice leads with them too), fix those named tests,
and rerun each with check=test before the next finish. Never guess the failing
test from the log tail: it holds the tests that ran after the failure.

Check whether `failure_line` is a timeout (an `.unwrap()` on `tokio::time::timeout`)
or an assertion. A timeout in a test your change does not touch is usually load
from the full suite: rerun it alone with check=test, and if it passes, retry the
release once on the same digest without editing it.

check=clippy reports every warning in the files your task touches, including old
ones. Fix findings in your own lines; older warnings do not block a release, so
name them in your report.

When you resume a stopped task, its claims may have lapsed. Call reservations,
then recover with the revision and epoch it shows, before the first check.

After release, keep acceptance to one live check of the changed behaviour when
tests already cover it. Read the result from the page URL (browser_read results
carry `url`) or an API response, not by searching selectors. A completed release
closes its task: cite the release job ID, and read it with status and job_id.
If the live check is impossible, say so and stop.

timeout_seconds is 1–600. Use a distinct stable request_id per check; reuse it
to reconcile a lost response instead of starting another. One check runs beside
a running build; a second queues. Guidance, JSON, UI and other non-Rust changes
need no Rust checks; their lane runs its own tests. No arbitrary commands,
auto-formatting, benchmarks or profiling are offered.

## Finish and background notices

finish freezes the reviewed inputs (expected_inputs_sha256) and starts the
release. finish, check and wait return at once with a job ID; end your turn
without polling. A background notice wakes you when the job ends, when a stage
passes the high end of its normal range (p95, then at most every 5 minutes), or
when it shows no progress for 15 minutes. On a notice tell the user, then use
status with job_id; judge slow against the stage's normal range and whether it
still reports progress, not wall time. cancel stops one job of your own session
and keeps the task; a job that had started deploying is recorded as uncertain
and needs reconciliation.

The finish result carries `stages` (seconds against each stage's p50–p90 and
time left) and `closeout` (release commit and claim release). Report the stages
as a table and the commit hash. A closeout other than complete needs
investigation, not another finish. Preflight drift needs an explicit
synchronization decision (decide); it never authorises overwriting another
writer.

## Waiting for claims

A file edit refused with `ownership_conflict` is not lost: your task is now
queued for those files behind the named blockers. Do not tell the user you are
blocked or ask them to say retry. Call `wait` with your `broker_id` (no
`job_id`) and end your turn; it runs in the background and a notice wakes you
when the task holds its claims (then reread the files and apply your edits
again), when the task closes, or if it is still queued after 2 hours (then
tell the user which session blocks it). The wait recovers your task itself
if it orphans meanwhile. Work on files that are not blocked first. Never abandon another session's task to get ahead of it.

## Ownership and cleanup

reservations/reserve before editing; continue reopens corrections after a
failed release; handoff transfers to an empty followup; orphaned claims need
recover, and the broker drops them after 2 hours without a heartbeat. A completed release commits exactly the released paths and releases
the claims itself: no acceptance, complete or release call follows. A closed,
frozen or abandoned task is never reopened; use a new one. When a task of your
own is superseded, confirm its unique edits are preserved, then abandon it.
Never abandon another writer's task unless the user asks you to clear it: then,
as administrator, call `takeover_preview` with its `task_id`, check its jobs show
no live or effect-started release, and call `abandon` with `task_id`, `task_key`
(the preview's `task.id`) and a `reason`. It drops the task's claims and queued
requests; a live or uncertain release is refused until cancelled or reconciled. Artifact pruning is automatic: never run
cleanup or Docker prune. Broker reminders about retained work never justify
resubmitting a finish.

When a provider requires optional fields, pass null for unused task_id,
request_id and digest fields; never invent placeholder values.
