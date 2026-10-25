# V3 Build runbook (operator material)

Host- and broker-side material moved out of the V3 Build skill (`build-service/docs/legacy/v3_build/v3_build.skill.md`) on 2026-10-08 (guidance survey F5/F8). The skill keeps the agent workflow; this file is for operators and for tasks that explicitly call for host or broker work.

## Storage checks and artifact prune

Check development storage after each terminal build, including UI releases.
Use df -h /, docker system df, and sudo /usr/local/sbin/v3-artifact-prune
for a dry run. The v3-only webagent-dev-artifact-prune.service runs about
every ten minutes and on receipt changes. Its apply mode retains open tasks,
failed/uncertain/interrupted work, the pinned recovery task, active compilers,
mounted releases, runtime data and stopped rollback containers. It deletes
eligible aged completed/cancelled or abandoned task artifacts while preserving
receipts. Inspect its bounded operator-pruning deletion reports.
Do not substitute the historical v2 prune script or host-wide Docker prune.

Inspect the service result if cleanup fails. Retained rollback containers and
protected rollback material need a separate reviewed cleanup decision.


## Broker internals

Compatible queued full-build tasks from one caller may be merged before compilation, up to eight tasks per image build. Each retains its own durable job ID and receives the shared tested-build result. A started image build stays frozen. If a UI release finishes during it, the broker checks that non-UI inputs are identical, then rebases its candidate onto the new UI snapshot before deployment. A same-file conflict fails the conflicting task; use a new task from the current release to revise it. A task ID is single use.

The API serves UI assets and seeds new coding workspaces from a broker-owned, versioned source snapshot behind a stable read-only mount. The broker atomically selects the complete snapshot and verifies the served UI. Runtime code, config and abilities stay pinned to their tested container release. Edits in another task workspace or the writable File Manager checkout cannot change mounted release assets. Only the broker chooses development targets; production is never a target. An uncertain external effect blocks subsequent releases until reconciliation.

After both replacements are healthy, the broker promotes changed release files
into the shared checkout if those paths still match the prior or new release.
A conflicting direct checkout edit holds subsequent releases for reconciliation.

Codex uses the same broker through server-side `buildctl.py`: `begin` defaults to an isolated workspace, `inputs --task ID` reads its digest, `finish ID --expected DIGEST` queues one release, and `wait ID` polls without creating a job. The V2 Python bridge uses the same task registry and its own authenticated caller identity. Legacy shared-checkout work must not overlap isolated tasks.

## Targeted cancellation

Use `buildctl.py cancel <job-id> [--request-id <stable-id>]` or the administrator
Builds stop button. Read the same durable job after a timeout; never resubmit
finish or create a replacement build to discover its outcome. Open source tasks
still use abandon. A queued job becomes cancelled immediately. An active build
requests termination of its subprocess group; Buildx backend cleanup can outlive
the client. Admitted batch members share one tested candidate and stop together.
After UI activation or container stop intent, cancellation only stops at a safe
phase boundary and yields uncertain until reconciliation. No automatic rollback
or retry is performed. Receipts, frozen source, and rollback material remain.

CLI cancellation of an admitted shared build requires `--include-batch` after
reviewing its affected job IDs. Both clients bind their request to the observed
batch membership; if admission changed it, refresh and confirm the new scope.


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

## Queue compatibility preview

Pending job projections include an advisory `queue_preflight` report before
compilation. Builds cards link peer jobs and show conflicting or overlapping
source paths, same-caller eligibility, already-started builds, separate UI
releases and pending source decisions. Identical edits are compatible; sequential
edits are checked in queue order. The preview does not start, cancel or combine
jobs. Admission retains the authoritative merge and eight-task batch limit.
Reports show bounded examples with an explicit partial flag. Owner-scoped
status never reveals other callers' jobs; administrator Builds views may compare
all callers. Only path names and fingerprints are examined, never credentials
or runtime databases. Behavioral conflicts across different files still need tests.

## Optional Code Index

The current protected policy (`post_deployment_indexing`, `after_deployment`)
publishes source with an immutable source receipt and no index, and records no
review obligations. An index is built only when an operator explicitly requests
`retry_post_deployment`. The text below describes how an index is built and
verified, and the legacy indexed publication mode. The protected installed
`index-scanner.py` parses candidate source as data in a networkless, read-only,
unprivileged Docker process with memory, process and time bounds; candidate
Python code is never imported.

Each immutable assets release carries `.code-index/index.sqlite` and
`manifest.json`. The manifest binds the release ID, complete archive digest,
complete source digest, SQLite digest, schema/index version, parser code digest and
parser dependency versions. Enumeration/scanner limits reject the release;
supported files cannot be silently omitted. Unsupported/generated/directory
exclusions and uncertain/limited parsers are recorded explicitly. Zero physical
line gaps, stale files and unindexed supported files are mandatory for an index.

Activation validates the pair before atomic pointer replacement and directory
fsync. Failed staging or an interrupted switch retains the prior pair. Rollback
must select a release containing a verified pair; historical unindexed assets
fail closed and need a reviewed backfill/migration rather than silent selection.
The index is derived metadata, never a session database. Do not copy or inspect
session databases for this workflow.

Before any full or UI-only release is staged, the protected broker requires the
candidate v3/plugins/abilities/Administrator/code_index/adapter.py bytes to
match its installed index-scanner.py. A mismatch fails before asset staging or
the indexing executor, preserving the active release.

Every adapter change requires installing that reviewed task's parser before
finish. The root operator runs the protected installer with
--install --install-tree <exact-64-character-task-tree-hash> while preserving
the exact task source, waits until all builds are idle, and restarts the
protected broker supervisor. Finishing the same task then creates a source/index
pair bound to the matching parser. Do not alter a frozen published index or
finish a task against a mismatched scanner. Gate changes to broker code follow
the same exact-task install and idle-restart sequence. The source is confined to
the broker task namespace; source ownership, single-link checks and
unchanged-input verification still apply. Never restart a busy broker. Session
seeding resolves the active release once; published tools are read only and
subject to normal Rust authorization.
Modified session workspaces and private
indexes are retained independently.


## Retained task reminder post-process

The protected broker reconciles reminders every 60 seconds, with a default
inactivity window of 86400 seconds. Operators may create the single-link
regular JSON file build-jobs/coordination/leftover-notice-policy.json containing
only {"stale_after_seconds":86400}. Bounds are 60 seconds through 30 days;
malformed policy fails closed and defers reconciliation.

GET /api/v3/sessions/{sid}/administrator/notices projects private reminders
with current Codebase Admin access and user/session ownership.
POST /api/v3/sessions/{sid}/administrator/notices/{id}/review requires browser
CSRF protection and acknowledges only that revision. Administrator Builds
shows up to 50 unreviewed reminders with available source, conversation,
build receipt and retained-space evidence; session reads show up to 20.
Reviewing entries reveals later entries on refresh.

Operator CLI commands buildctl.py leftovers and buildctl.py review-leftover
<exact-notice-id> operate within their own caller namespace. Codex tasks have
no actual Codex conversation ID; Administrator Builds is their fallback.
The broker does not send messages into Codex chats or start WebAgent model runs.
Reminders are UI metadata and do not change tool permission. Reviewing one
does not make artifacts eligible for pruning. Investigate unique edits,
ownership and uncertain effects before using the existing abandon or operator
reconciliation workflow.


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
