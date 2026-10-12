# v3_build_work

Begin with mode `isolated`, a stable task ID, and a summary before source edits. The receipt provides a private workspace path and input digest. Read the current `workspace_inputs_sha256` from status after applying reviewed patches and focused checks. Finish with that digest to freeze this task and immediately queue its own serial build and automatic V3 development deployment. Other registered tasks may remain open or continue editing.

The broker compares the frozen candidate with the latest healthy release. A same-file conflict fails without deploying; fix it under a new task ID. The original finish call remains pending until the Rust recovery watcher delivers the durable terminal result on that same call ID. Confirm both containers healthy after a completed result. If delivery is unavailable, inspect this task's existing job ID read only through `v3_worker_job`; never submit a duplicate finish to recover a lost response. An uncertain deployment requires reconciliation, never automatic retry. Abandon only an open task. Production is not a target.

This tool remains runtime foreground/stop with concurrency 1. Its durable broker finish/recovery path is not generic background execution; _execution_mode and generic silent/wake cannot replace it or grant deployment permission.


## Source synchronization decisions

Call v3_build_work(action="preflight", task_id=...) before finish. The content-free
report compares all allowlisted V3 checkout source with the deployed release,
lists missing/modified/checkout-only paths and flags paths touched by this build
and promotion conflicts. Runtime data and file contents are excluded. Page with
offset when next_offset is present. Status and the Builds dashboard also flag drift.

With drift, finish keeps the task open and returns needs_decision. Choose
sync_decision="synchronize", "proceed" or "cancel" and pass expected_sync_digest
from the current report alongside the normal expected_inputs_sha256. Synchronize
returns needs_synchronization without overwriting source: review and reconcile
the listed files, then preflight/finish again. Proceed uses the frozen deployed
release and preserves its changes, but does not repair checkout drift or authorize
overwriting source conflicts. Cancel abandons the open task.

Admission and pre-deployment checks can pause an already queued job with
needs_decision. Use action="decide" with the same task_id, choice and fresh digest;
this resumes or cancels that same durable job, never submits another finish.
Changes to checkout, release or candidate invalidate the decision. A decision
never overrides live deployment authorization or uncertainty reconciliation.
After deployment, a promotion failure remains uncertain and includes a refreshed
synchronization report; never replay its external effects. Legacy direct-checkout
builds require synchronization or migration to an isolated task before proceeding.
