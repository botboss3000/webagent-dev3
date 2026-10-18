# Codebase Admin

For code review, test assessment, diagnosis without fixes, or release-readiness verification, load the selectable code_verification skill. It preserves read-only review scope and binds verdicts to fresh evidence; implementation or release authority must come from the user's request.

For ability/plugin changes and Markdown, JSON/configuration, UI assets, Python or other non-Rust files, load the selectable live_plugin_publication skill in this ability before choosing a release path. It covers independently replaceable app components, dependency-aware compilation, and live asset/configuration publication while distinguishing the intended architecture from currently supported broker lanes.

For UI presentation/layout questions and every UI change, read `plugins/abilities/Core/ui_admin/ui_admin.skill.md` first. Use its general investigation workflow for visual properties and apply its button, touch-feedback, and border-state defaults when changing controls. UI Admin remains pending; read that file through Codebase Admin rather than assuming its legacy tools are available.

For v3 Rust work, load the selectable rust_quality skill in this ability for ownership, async design, correctness, and measured optimization guidance. This standard skill remains authoritative for task editing and release workflow.

## Early Rust feature checks

Before a full v3 release build, run default and live compile checks when changing shared host code, feature-gated paths, engine integration, service wrappers, or implementation-crate dependencies. These checks catch feature-boundary errors before optimized release compilation. Guidance-only and asset-only changes do not require Rust checks unless their actual compile dependencies change.

The default profile follows the workspace's existing default features. The live profile disables defaults and enables live-context. Code can compile and pass tests in the linked build yet fail in the live host because replaceable implementation crates are intentionally not linked there. Keep that boundary; do not add an implementation dependency merely to make the live check compile. Verification of replaceable implementations should bind expected identities to trusted component metadata.

1. Obtain the current task input digest through build_work review. Run check=check with feature_profile=default and feature_profile=live against that same unchanged digest.
2. Supply a distinct stable request_id for each profile. Profile changes require a new request ID; reconcile a timeout or uncertain response through the original request/job instead of submitting a duplicate.
3. A queued/running receipt is not a pass. Inspect each durable job until its terminal result is completed, with the expected profile and input digest. Changed inputs or a stale result require fresh affected checks.
4. Fix compile failures before starting the full release build. Run applicable tests separately and retain the normal optimized linked/live equivalence, publication, health, promotion and exact-commit gates.

The live profile is supported only for compile checks (check=check), not check=test or resource_probe. Omitting feature_profile retains existing default behavior. Profiles are a fixed allowlist, not arbitrary Cargo flags. Checks retain isolated snapshots, ownership, unchanged-input attestation and enforced resource limits; they do not deploy, promote source or start documentation/index maintenance.

The protected operator CLI and broker API expose feature_profile today. The in-app build_work tool schema does not yet declare it; in-app agents should hand this check to an authorized operator until that tool surface is extended. Ordinary in-app default checks remain available.

Example operator broker request (replace placeholders with actual task values):

~~~json
{
  "action": "check",
  "task_id": "<owned-task>",
  "check": "check",
  "feature_profile": "live",
  "expected_inputs_sha256": "<fresh-input-digest>",
  "request_id": "<unique-live-check-id>",
  "timeout_seconds": 600
}
~~~

Use another request ID with feature_profile=default for the companion check. For operator CLI syntax and receipt details, see [the broker reference](../../../../build-service/README.md#standalone-rust-feature-checks).

## Automatic broker identity and work selection

The authenticated WebAgent runtime supplies the current user and chat session to
the broker. Do not look up your own session ID, ask the user for it, or stop
source work because the model context does not expose it. Session identity is
not a tool argument and must never be guessed or copied from another chat.
The broker records provenance separately from the descriptive work summary.
The external operator CLI has a shared operator identity; it must not be
reported as a verified originating Codex chat.

Use **Broker ID** for an isolated work item, **Broker Job ID** for an execution
attempt, and **Broker Release ID** for a publication. One independently deployable
outcome normally shares a Broker ID across files, edits, checks and builds.
Start separate work for an unrelated outcome, even in the same conversation.

When the tool schema exposes `broker_id`, omit it to continue current work,
pass `"new"` to create independent work within that same source operation,
or pass a returned Broker ID to select earlier work. Never invent an ID.
Source edits use this automatic selection. Ordinary reads without a selector keep their published/container behavior, including background reads and search filters. Pass the returned Broker ID when reading or searching the private draft.
The tool result returns the resolved ID and input digest. Continue using that
digest for edits to existing source; automatic identity does not bypass stale
input, ownership, authorization, testing or publication checks. Retries use the
same operation/request identity and must not create another work item.

`task_id` is a compatibility alias on older tool schemas; never provide both
selectors. If `broker_id` is not yet exposed, source edit/write calls without
`task_id` already create/reuse an isolated draft and return its `task_id`.
Use the returned alias for related reads and edits. No explicit begin or
session-ID lookup is required. Explicit runtime/workspace paths retain their
own tools and policy; automatic source work never authorizes direct live edits.

Expanded activity tool details display available broker provenance and receipts.
Preserve original provenance when continuing work. A Broker ID selects only
work authorized in this caller's session; it cannot take over another owner.

## Isolated source workflow

Application source changes use one stable isolated task ID. Source writes without task_id create/reuse a broker draft and return its ID; continue reading/editing that task with its latest digest. Relative source paths and /live-checkout source writes enter this workflow. Explicit runtime/workspace paths use their own tools and live policy; source data, generated output and inspection databases cannot be written through task file tools. Never fall back to live source writes when a task conflicts.

With task_id, file_list/read/search/edit/write create a missing task. Paths are source-relative, such as src/example.rs or ui/chat/chat.css. Reuse the returned task_id throughout the change. Read reservations before editing: held modules/paths belong to their logical task until explicit verified completion. A queued unchanged draft refreshes to current published code when acquired; reread before applying changes.

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


### Automatic rebuildable check-cache retention

The protected broker expires direct `target` and `cargo` directories under
`build-jobs/checks/<task-key>` and legacy `build-jobs/coordination/checks/<task-id>`
after two hours without cache writes or broker-recorded task activity. Open,
failed and uncertain tasks can retain their source and evidence while losing
rebuildable outputs. The minute post-process removes at most four cache folders
per cycle, shares the host artifact-prune lock, defers pending builds and external
compilers, and pins running-container mounts. Symlinks and overly large inspection
trees fail closed. Receipts under `build-jobs/cache-pruning` record intended and
completed removal. Source snapshots, logs, experiment scratch directories, job
receipts, runtime data and rollback releases are outside this cache policy.
Operator checks must start Cargo before writing these caches and must mount any
cache used by a disposable container; prefer broker Cargo checks for admission
coordination. Expired caches rebuild on the next check.


### Checkout caches and reconciled terminal artifacts

The same broker minute pass expires fixed checkout `target/debug`,
`target/release`, `target/cargo-home` and `target/session-naming-check/debug`
after two idle hours. Adjacent logs and evidence remain. Specific V3 container
mounts, including stopped rollback mounts, are preserved; the broad live checkout
mount does not pin rebuildable caches.

Failed, interrupted, stale and uncertain task source/job artifacts require an
explicit local operator investigation before automatic deletion. Preserve unique
edits elsewhere and reconcile external effects first. Read the current reminder
identity and use `buildctl.py clear-retention NOTICE_ID --unique-work-preserved
--effects-reconciled` only when both assertions are true. This records exact source
and job digests, then starts a two-hour grace period. A changed source or job
invalidates eligibility. The pinned `4f605f3b-4444-4447-a511-3a9d4b9677df` job is
excluded. Job JSON receipts and task metadata remain, including original uncertain
status; clearance is not a build replay or outcome rewrite. Reminder review alone
does not authorize deletion. No live database or session workspace is eligible.


## Independent package publication

Protocol 1 runs runner.py in a bounded isolated Python process per invocation. The protected broker publishes complete immutable packages; API and loop read guidance at model-call boundaries and the worker pins one package directory per invocation. Compatible runner, existing guidance and manifest metadata updates use plugin_deploy without Cargo or replacing API, loop, worker or gate containers. The package build manifest separates runner.py from runtime guidance assets.

Installed tool names and schemas remain interface 1. Tool additions/removals or incompatible protocol changes require a coordinated core release. Invalid/incomplete candidates are rejected before activation, retaining the current revision. Current invocations finish on their pinned package; cancellation terminates the child tree. Stateful prompt/session operations stay in core services; tasks, checks, promotion and deployment stay in the protected broker. Identity, policy, approval, leases and durable recovery remain authoritative in those services.


### Verify which package executed

Live file results report implementation_version and implementation_sha256 for
the executable that handled the call. plugin_revision identifies the immutable
published snapshot. Guidance or compatible catalog metadata can change while
the executable hash stays the same. Compare the executable hash before diagnosing
a stale implementation; a new snapshot ID alone does not prove executable bytes
changed. New invocations pin the selected snapshot, and publication does not
replace the API, agent loop, worker or gate processes.

## Logical completion and controlled builds

Use build_work reservations, reserve and heartbeat with current revisions/epochs and stable request IDs. finish publishes a frozen generation while retaining ownership. continue reopens corrections after a completed release. acceptance binds the latest synchronized release, exact commit and successful behavioral checks you performed; complete explicitly releases claims after fresh broker verification. Never equate build completion with agent completion. handoff transfers atomically to an empty followup. Orphaned claims require recover; administrator takeover needs preview, a reason and reconciled execution/effects.

resources exposes measured phases. pause affects only a compiler, resume requires safe headroom, priority is bounded from -2 to 2, and reconcile cleans only exact stopped pre-publication phases. Insufficient reclaim waits for intervention rather than restarting work. Shell/Python run in the session workspace, read source and cannot write source or sibling workspaces. Use broker checks for task verification. See docs/controlled-build-scheduler.md.
