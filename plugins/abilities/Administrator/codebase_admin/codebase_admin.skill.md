# Codebase Admin

For v3 Rust work, load the selectable rust_quality skill in this ability for ownership, async design, correctness, and measured optimization guidance. This standard skill remains authoritative for task editing and release workflow.

Choose the target from the request. Explicit direct/live edits and runtime configuration (such as data/config/chat.json) use file tools without task_id (or with null). Application source changes intended for a build/release use file tools with one stable task_id, 8-80 letters/digits/underscores/hyphens. Do not ask the user to choose a tool. Do not use live edits as a fallback when a task edit fails.

With task_id, file_list/read/search/edit/write automatically create a missing isolated task. Paths are relative to the source root, such as src/example.rs or ui/chat/chat.css; never /live-checkout or a task host path. Reuse the same task_id throughout the change. Without task_id, relative paths start at /live-checkout and absolute container paths retain their usual OS permissions.

file_list pages use offset >=0 and limit <=100; file_read pages use one-based offset and limit <=200. Task file_search accepts a literal query across source, without regex/path/glob/comment filters. Read existing files from the task before editing. file_edit replaces exactly one matching old_text block, which may span many lines, with new_text. file_write creates a file or replaces its full contents using content. Task writes require a stable request_id and, for an existing task, expected_inputs_sha256 from the latest task read/list/review. expected_sha256 optionally checks one file. Reread after stale input errors. New files use file_write; do not handwrite unified diffs. Writes return the generated diff and new task digest. Task writes cannot change inspection databases or any data/ path. The two admin/shared_default database snapshots are read-only troubleshooting inputs, read through bounded text exports.

build_work actions: begin (optional explicit task creation, summary); status (task_id or job_id); review (task_id, diff and current digest); check (task_id, expected_inputs_sha256, check=check|test, optional timeout_seconds <=600); preflight (task_id, optional offset); finish (task_id, expected_inputs_sha256, optional sync_decision and expected_sync_digest); decide (same task_id and fresh synchronization decision); receipt (task_id, request_id); abandon (open task_id). A closed, frozen or abandoned task is never reopened: use a new ID. Creating or editing a task does not start a build or deploy. Finish only when the user authorizes deployment; it freezes reviewed inputs and starts the existing tested development release flow.

After an uncertain write inspect build_work receipt and task source; never blindly repeat an external effect. Finish remains pending until its durable job completes, including recovery after loop restart. Inspect the same job rather than issuing another finish. Preflight drift requires an explicit synchronization decision; it cannot authorize overwriting another writer. Preserve rollback containers and runtime data. Only the broker selects development targets; production is excluded.

Read repository instructions and Git status first. Verify source and build inputs remain unchanged during checks. Runtime configuration is outside the source build snapshot. Shell and Python support debugging and checks but do not replace task file tools for isolated edits.

state_inspect exposes bounded current-session metadata. prompt_slot_read/write manage revisioned configurable instructions; they cannot change compiled default instructions. Runtime access, installation policy, account tier, membership, session mode, approvals, cancellation and shutoff remain authoritative. Tool settings never grant permission. File reads support vetted background execution; task creation only seeds an isolated snapshot, never grants writes or deployment.

When a provider requires optional fields, use null for unused task_id, request_id and digest fields. Never invent placeholder values for a live call.

## Development artifact cleanup

Reuse one task_id for each change and record its durable job_id after finish. When an earlier open task is superseded, verify that its unique edits are preserved in the replacement task or a completed release, then use build_work(action="abandon", task_id=...) for that task. Abandon only work owned by this session; do not abandon another writer's task or infer completion from age, a similar summary, or a closed task status. Failed, uncertain and interrupted jobs require their own receipt and effect reconciliation.

The development host owns automatic v3 artifact pruning through webagent-dev-artifact-prune.service, approximately every ten minutes and on receipt changes. It retains receipts, open task source, unresolved jobs, mounted releases, recent completed release jobs, application state and session workspaces. Eligible completed/cancelled and abandoned artifacts have a two-hour grace period; cleanup defers during compiler work or queued/running/decision-waiting builds. Agents do not need to invoke cleanup after each tool call. Report large retained check caches and superseded tasks when finishing; do not delete another writer's snapshots, run the historical v2 pruning script, or use host-wide Docker prune commands.

Job 4f605f3b-4444-4447-a511-3a9d4b9677df is pinned for recovery-card work. Preserve its source until that work is resolved and an operator removes the retention pin. Operator dry-run inspection uses /usr/local/sbin/v3-artifact-prune on the development host through SSH; host cleanup authority is not granted by this skill or by a tool setting.

### Retained-task reminders

The broker posts private reminders for inactive open work and failed,
interrupted, uncertain or stale builds. Investigate the originating task and
existing job; preserve unpublished edits and reconcile uncertain effects
before clearing it. Never resubmit a finish merely because a reminder appears.
Mark reviewed acknowledges one revision and preserves all artifacts.
Use the normal owned-task abandon workflow only when open edits are no longer
needed. Operator tasks without a conversation identity appear in
Administrator Builds. Reminders never grant tool access or trigger a model run.
