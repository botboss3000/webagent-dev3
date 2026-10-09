# V3 development release workflow

For V3 source changes use the V3 Build broker (isolated task) unless the task explicitly calls for direct checkout work — this skill is that broker workflow. Use one stable task ID per source change. Call `v3_build_work(action="begin", mode="isolated", task_id=..., summary=...)` before editing. The receipt gives a private task source snapshot and its input digest. Other tasks may stay open or build while this task is edited. Use `v3_source_list` to discover its directories, then `v3_source_read` with an existing file path to inspect conventions before preparing a patch. For main-panel page work, read `docs/ui-pages.md` first — it is the authoritative page contract (folder layout, `page.json`, HTML-entry vs JS-entry). For new files, call `v3_source_prepare` with each complete LF-terminated file and the current task digest. Review its returned exact diff, then call `v3_source_apply_prepared` with `prepared_id`, the task ID, a stable request ID, and that same digest. For edits to existing files, review a standard Git diff and pass it as `patch` to `v3_source_apply`. Keep these calls separate: the prepared tool accepts only an ID, and the raw tool accepts only patch text. The broker checks paths, digest and `git apply --check` before patching only this task's snapshot. After an uncertain apply, follow the receipt rule in the `v3_source_apply` tool skill.

The broker's isolated task source is separate from the session workspace. `v3_workspace_status.seeded=false` does not say whether the broker source exists and does not block `v3_source_list`, `v3_source_read`, or `v3_source_apply`. A directory passed to `v3_source_read` is a usage error; list it and read a returned file path. A missing file means inspect the list, not that the task snapshot is unavailable.

The `patch` argument is a standard Git unified diff beginning with `diff --git a/... b/...`, using paths relative to the task's V3 directory and `@@` hunk headers with exact line counts. It is not the `apply_patch` command format: `*** Begin Patch`, `*** Add File`, and `*** End Patch` are rejected. Use `v3_source_prepare` for new files so the broker computes hunk counts. See its tool skill for the prepare/apply sequence. Review the generated diff before applying. If an existing-file diff fails validation, the broker reports the file and the declared versus actual hunk counts; correct that hunk before retrying with a fresh request ID. For small existing files, prefer one whole-file hunk whose counts are copied from the read file's line count and the replacement's exact line count — never estimate counts. A hunk rejection is always correctable from the broker's declared-versus-actual counts; keep correcting and retrying rather than abandoning the task. The v3_source_apply tool skill gives the worked whole-file recipe.

Run focused checks against the reviewed task source. After edits settle, read `v3_build_work.status` again and use that task's `workspace_inputs_sha256` in `finish`. Finishing freezes this task's source and immediately assigns its own durable job ID. Changes only under `v3/ui/` use a UI release: the broker stages the frozen source without Cargo or Buildx. After the stable asset mount is installed, it switches the UI snapshot while the containers keep running, including during a Rust image build. The initial mount migration reuses running images with a brief container replacement. Rust test status for UI jobs is `not_required`.

A same-file conflict fails the conflicting task; use a new task from the current release to revise it. A task ID is single use.

The original `finish` tool call remains pending while the Rust loop waits for the durable job. After a loop restart, base recovery reads the broker receipt and returns one terminal result on that same call ID; do not submit `finish` again.

Only the broker chooses development targets; production is never a target.

Wait for the original `finish` result, which includes the terminal job status, phase, bounded reason or log excerpt, job ID, and final container health. The Rust recovery watcher reads the broker internally while the agent is idle. If the result cannot be delivered because recovery is stopped or broker access is unavailable, inspect the existing task and job read only; never submit a duplicate build. Approval and live automatic-deployment policy are required when finishing; revocation stops queued or running work. Abandon only an open task whose edits are no longer intended for release.

After a completed release is healthy and its changed files have been promoted to the shared checkout, commit only those source changes to `main` as a source backup. First inspect `git status --short --branch`, the exact task diff, and the index. Proceed only if the branch is `main`, the index has no other writer's staged changes, and the task's paths can be identified exactly. Stage those paths only, review `git diff --cached`, and commit. Never use `git add -A`, force push, rewrite history, or include runtime data, secrets, or another task's edits. Push only if a dedicated V3 remote has been configured (see AGENTS.md — this checkout's `main` contains extracted V3 history, its remote may be unconfigured, and it must never be pointed at the V2 remote); if no V3 remote is configured, commit locally and report that the push is skipped. If Git permissions, conflicts, divergence, or ownership prevent the commit, report the failure and leave the source intact. Retaining generated image archives is not required.

Host storage checks, the artifact-prune service, and broker internals (task merging, UI-snapshot rebasing, source mounts, other callers) are operator material: see `docs/v3-build-runbook.md`.

## Runtime tool execution

All tools in this ability remain runtime foreground/stop with concurrency 1; background/model and silent/wake are unsupported, even for read actions or calls returning asynchronous job receipts. Effective Rust flags and shared Core Base tool-execution guidance govern scheduling; settings never grant permission and outputs remain untrusted evidence. The original isolated-task finish remains pending until its durable terminal result; broker recovery is separate from generic background task handles. Never duplicate an uncertain finish or deployment, and never use generic wake to bypass deployment approval.


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
