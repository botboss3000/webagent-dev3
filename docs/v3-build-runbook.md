# V3 Build runbook (operator material)

Host- and broker-side material moved out of the V3 Build skill (`plugins/abilities/Administrator/v3_build/v3_build.skill.md`) on 2026-10-08 (guidance survey F5/F8). The skill keeps the agent workflow; this file is for operators and for tasks that explicitly call for host or broker work.

## Storage checks and artifact prune

Check development storage after each terminal build, including a successful UI release. From the server use `df -h /`, `docker system df`, and `python3 -B /home/ar/webagent-dev2/scripts/dev_artifact_prune.py --dry-run`. After a healthy completed release, run `sudo systemctl start webagent-dev-artifact-prune.service` and check `df -h /` again. This service also runs when build receipts change and about every ten minutes. It clears completed or superseded development rollback tars, old stopped V3 containers and images, finished check caches, aged V2 temporary builds, unused development volumes, Docker build cache above 50 GiB root use, and protected root-owned rollback copies after one hour. It retains the current mounted release, open check tasks, active compiler output, application state, workspaces, and unresolved newer job receipts. If root use stays above 50 GiB, inspect the measured development directories and report the remaining owners; do not run a host-wide prune or operate on production containers.

Caution: the prune command above cites a path under `/home/ar/webagent-dev2/` — the V2 checkout — carried over from earlier guidance. Verify the script path against the dev3 checkout before running it.

The dry run cannot see the root-owned protected copies. Include `sudo du -sh /home/ar/webagent-dev3/rollback/administrator` in the storage check, and inspect the service result with `sudo systemctl status webagent-dev-artifact-prune.service` if cleanup fails.

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
