# Current v3 broker workflow

Deployment runs the applicable functional/safety tests and exact source/artifact,
ownership, compatibility, synchronization and health checks. Code Index
construction, documentation maintenance and Code Index/documentation reviews are
not part of the release workflow: publication records no review obligations and
nothing waits on them. There is no automatic indexing on deployment or restart.

After verifying and committing exact promoted bytes, release source write claims
with build_work release_claims and the release commit. The broker verifies the
completed synchronized release, commit bytes and health; releasing the last held
claim this way completes the task, with no separate acceptance or complete call.
A partial release keeps the task owned. Reserve only modules fundamental to the
actual edit; acquire other affected paths when changing them and release them
when their deployed changes are safely committed. Post-deployment metadata work
does not need an open task.

An operator may optionally request retry_post_deployment with task_id, job_id
and request_id to construct an index for the exact deployed snapshot. It does not
deploy or change application source. Until an index exists, lookup reports
unavailable. Task acceptance, claim release and completion never require an
index or reviews. Earlier review ordering, approval and deferral text below
describes the protected before_deployment policy and legacy jobs only.

## Release-scope guards

Checked source patches are text unified diffs, except that image and font assets
under `ui/` (`.png`, `.jpg`, `.jpeg`, `.webp`, `.gif`, `.ico`, `.woff`, `.woff2`)
may be added or replaced with Git binary hunks from `git diff --binary`. Binary
hunks elsewhere, data-less `Binary files ... differ` diffs, renames and copies are
refused. The 1 MiB operator patch bound applies to the encoded patch.

Source changes are compared by bytes and the executable bit only, in task review,
preflight, finish and acceptance alike. A private-tree mode rewrite (for example
0664 to 0600) is not a change, so a byte-identical revert leaves the release.
preflight returns release_plan with the input digest, computed operation and
changed_paths. Pass the intended set as finish expected_paths (V3-relative) and
the lane as expected_operation; finish refuses a mismatch before any reservation
or job exists, and rechecks after reservation. Narrow claims on an open task with
release_claims and explicit scopes. When scope changes, start a fresh task rather
than reverting by reverse patch. Acceptance accepts a released path absent from
the commit only when its released bytes and executable bit equal the job base.
A change limited to host-lane broker files is refused unless its tree is already
installed (lifecycle.py install --install-tree), because build_deploy would
rebuild unchanged app images and leave the old broker running; preflight shows
the refusal. Any other release touching build-service/ records
broker_install.required, and a host_service_deploy records the installed
service_dir.

# Host build service
All maintained host build and legacy source-workspace code lives here.
Administrator tools are protocol clients. Ordinary shell, browser, Git and live
file tools remain in the isolated worker. Agents receive neither root nor Docker
access. Docker-group membership is privileged host authority.

## Linux installation
Requires Docker/Buildx, Python 3, systemd, and the project's pinned Rust builder.
Run the reviewed lifecycle.py install --checkout-root /srv/webagent --instance
myapp --api-container myapp-api --loop-container myapp-loop --public-origin
https://dev.example.com as root. A missing default webagent-build service
account is created. Use --service-account for an existing Docker-enabled account.
Source ownership is resolved from the checkout independently of service identity.
Write claims follow the edited files' owners only; see Cargo module source
reservations below.
Operators run buildctl as the configured service account (sudo -u ACCOUNT when
needed); Unix peer identity authenticates this operator channel.
Root-owned configuration under /etc/webagent/<instance>/ owns paths and identities.
CLI calls use WEBAGENT_BUILD_CONFIG or lifecycle --config. Writable configuration,
symlinks, and excluded targets are rejected.

Reviewed code installs under /var/lib/webagent/<instance>/build-service/releases/
<digest>/. The webagent-build-<instance>.service unit runs the protected supervisor.
The broker drops privileges; its helper supplies allowlisted snapshots and
creates only canonical session workspace directories with the fixed worker identity.
Use lifecycle validate, start, stop, status, drain and resume. A source release
cannot replace the installed service; operators install an exact reviewed tree
with --install-tree HASH while idle.

buildctl.py maintenance drain --task-id TASK persists a migration gate. Only
that exact local operator task may mutate source or finish a build while drained.
Other writes/checks/profiling are refused. Reads, receipts, cancellation and
lease controls remain available. maintenance resume reopens admission.
State stays under the configured state root. Sockets, PIDs and supervisor
diagnostics stay under the runtime root. Receipts and archive names are preserved.
Retain previous configuration, installed release and stopped application containers.
Reconcile uncertain effects before rollback. Shared UI inputs are bundled under
shared-ui; no v2 checkout or v2 application data is required. Fresh chat
configuration comes from the v3 template.

For a new instance, run deploy.py build, build-loop and build-gate with
WEBAGENT_BUILD_CONFIG, then start-gate, start and start-loop. The initial source
snapshot is indexed lazily by the service before the first workspace operation.
First-start configuration is seeded only when absent. Subsequent releases use
isolated broker tasks rather than direct deployment commands.

A dedicated account requires the Linux acl utility. The installer grants that
account source promotion rights only over source paths; it does not grant access
to application data. Operators retain access to newly promoted files through
directory default ACLs. Container UIDs remain fixed by the pinned images and
installation validation rejects incompatible identity values.

Run first-start deployment commands as root with the explicit protected
WEBAGENT_BUILD_CONFIG path. API bootstrap uses retained directory descriptors
to create app-owned data/config directories and seeds only absent defaults.
It rejects symlinks and preserves existing configuration contents. Ordinary
releases continue through the unprivileged broker and preserve runtime state.

An isolated local operator task changing only host.py, ownership.py, this README
and their test_build_service.py/test_scheduler_ownership.py suites can publish
after each changed executable and the candidate host.py have been
installed while idle. The service verifies the protected installed bytes again,
freezes and indexes the source, verifies unchanged healthy application images
and served assets, then performs checked source promotion. Application, worker,
workspace executor, policy and other module changes always use the image build
workflow. This host-only path does not grant agents installation authority.

The administrator Builds dashboard returns every unreviewed retained-task notice
and every non-adopted build receipt, without a display-page limit. Caller-scoped
agent notice reads remain bounded independently from this administrator feed.


General drop-in ability publication is described in ../docs/drop-in-abilities.md.
All ability catalogs/skills and source JSON configuration templates can use
plugin_deploy once the general engine/worker contract is installed. New isolated
Python or standalone executable packages publish independently; the installed
engine validates the complete candidate and API/loop adoption is verified.
Existing native adapters keep their core interfaces. Runtime configuration changes
use domain APIs; protected infrastructure changes use lifecycle reconfigure, which
retains images and replaces only affected services.

Codebase Admin interface 1 is an independent Python executable package under
plugins/abilities/Administrator/codebase_admin. Its build manifest lists runner.py
as the executable input; existing guidance and compatible catalog metadata are
runtime assets. Isolated tasks affecting only this package use plugin_deploy:
validate the complete protocol/catalog, compile-check Python, run package tests,
stage a frozen revision, then atomically activate the existing asset pointer.
API, loop, worker and gate keep their images and processes. Incompatible tool or
schema contracts require a coordinated core release. Core state/prompt APIs and
protected broker effects keep their existing authorization and recovery.

Compatible Codebase Admin metadata obeys the core loader's UTF-8 byte limits
and types; contract fields compare canonical JSON so booleans cannot masquerade
as version numbers. New tool/selectable catalog IDs need coordinated installation.
Package tests run in the retained API image as the worker UID, with no network,
application state, credentials or control sockets. Host Python syntax feedback
does not establish compatibility with the installed runtime.

Controlled ownership, attributable parallel phases and operator policy adoption are described in ../docs/controlled-build-scheduler.md. Build completion retains source claims until explicit verified agent completion.


Controlled scheduling is described in ../docs/controlled-build-scheduler.md.
Compilation and tests use identified containers and measured admission; unrelated
standalone packages compile before taking the serialized publication lock.
Publication rebases and revalidates verified cached outputs before activation.
Compile directly from the job frozen source; create its immutable served asset
revision only after the publication rebase, so independent releases cannot pin
an earlier candidate under the final job revision.
A successful build retains its source reservation until the agent verifies
acceptance, commits its exact promoted inputs and explicitly completes ownership.
The canonical maintenance ledger retains historical task/retry evidence under a bounded 512 MiB broker budget; scoped synchronization still limits each review transaction.
Between broker installation and source promotion, use the exact protected installed
buildctl.py so its configuration parser matches the installed broker.

## Engine component publication

The registered context-assembly interface uses `plugins/engine/context-assembly/component.json`. The broker builds only the context-service package and local Cargo dependency closure, runs service/library checks in the pinned toolchain, and binds compiler dep-info and executable hashes to a content-addressed artifact. A complete immutable publication seals the executable under that generation’s `artifact/` directory before index construction and retained-host validation. Invalid candidates do not activate. Removing the descriptor uses the existing plugin publication lane and pauses affected work through the engine host.

Development core builds select `live-context`; stable builds select `linked-context`. Published core-inputs follow the live main package’s resolved Cargo dependency graph, so changing service/assembly implementation can retain API and loop binaries, while contract changes request coordinated builds. Cargo workspace membership alone does not grant hot publication. Engine and ability adoption must acknowledge the same revision before release completion.

Ownership FIFO applies only to live waiting requests. Orphaned tasks retain every held scope and historical requested scope, but requests they never acquired do not reserve components or block new work. Recovery remains explicit; this distinction neither releases held claims nor deletes old source or ownership records.

Engine publication seals `plugins/engine/catalog.json` with the exact revision, descriptor/executable hashes and sorted withdrawn IDs derived only from the prior protected publication. Empty membership with an explicit withdrawal pauses dependents; unknown requirements remain invalid. Reinstall clears that ID from withdrawals. Cargo dependency caches may be reused across implementation edits only after locked manifests, pinned recipe and retained cache integrity match; artifact reuse still requires exact complete input hashes.

Checks persist admission headroom status and include admission waiting in their overall deadline. Completed non-OOM Rust diagnostic checks can supply measured peak estimates for the same fixed check recipe with the existing 20 percent margin; killed, interrupted, timed-out and incomplete executions never calibrate estimates. Memory reserve and source claims remain authoritative.

Independent engine replacement requires current API and loop adoption receipts both reporting live execution. Linked, unknown, missing or stale mode receipts route engine source edits to a coordinated core release; admission rechecks the mode before hot publication. Successful release receipts retain actual per-consumer engine execution modes, so a linked application never represents a newly published service as its executing implementation.

Compiler checking, test compilation, test execution and release compilation use separate measured profiles. A one-time migration retains old mixed test samples under a legacy key, reassigns completed samples using actual recorded phase identities, excludes failed/incomplete runtime samples, and keeps all phase rows. Public job views expose the bounded waiting cause, memory estimate/headroom and admission deadline.

Local operators may explicitly request compiler_memory_cap=4026531840 for checks or coordinated releases. Only engine compile_test/compile_release phases use it; runtime tests and publication retain their normal profiles. Admission retains the configured host reserve and requires actual kernel memory.max readback and zero swap allowance before compiler execution. Receipts record requested/observed cap, recipe, container identity, peak and OOM counters. Any OOM is a failed check and cannot calibrate successful memory history. Omission preserves normal uncapped behavior.

Local operator test checks may supply test_filter as an ASCII Rust unit-name substring (letters, digits, underscore, colon; maximum 160 characters). It selects runtime tests and enables nocapture for bounded diagnosis, never arbitrary Cargo arguments. The filter is bound to the idempotency signature and public receipt; omitted filters retain full-test behavior.

The coordinated engine recipe now compiles an optimized linked-context executable first, executes its fixed offline context-smoke equivalence check, and retains it under stable/webagent-v3 with its hash/mode/proof. The live workspace build runs afterward and remains the deployed artifact. Linked compilation uses its own measured recipe and the explicit compiler cap when requested; optimized smoke uses normal test admission. Cached artifacts must include the linked proof under the new recipe identity.

Linked optimized compilation uses /output/linked-target and retains separate hashed dep-info inputs beside its stable executable. Live compilation and deployment use /output/target exclusively. Fixed optimized live smoke runs beside the official context service, requires reuse/drain, and compares its two prepared results exactly with linked smoke before certification; both proofs are retained.

Candidate preparation stream-hashes private inspection members by mode, exact size and SHA-256; changed inspection snapshots still fail, while release candidates retain only application source. Full allowlist validation remains mandatory. Attested archives are released after durable source preparation, and the protected snapshot helper clears its last archive on both successful and failed sends. Durable inspection snapshots and source reservations remain unchanged.

Fixed linked/live smoke recipes write their mode-specific JSON proofs under /output/stable. The broker verifies bounded regular single-link files before parsing; environment-value redaction applies only to public logs, preserving typed true/false proof values. Successful phase output is persisted immediately as a bounded already-scrubbed job log, so later proof validation failures retain test/compiler evidence. Failed or unsealed outputs do not receive cache certification.

Local coordinated releases may request package_memory_cap=4026531840 (--package-memory-cap), bounding only their dedicated BuildKit ancestor with actual kernel memory.max/zero-swap proof and OOM-failure receipts; ordinary profiles and host reserve are unchanged. Packaging admission has a600-second deadline.

The complete-tests-v4 certificate retains the exact Cargo-declared unit/integration ELFs plus official debug context-service support, hashes and package/source provenance. Replay restores that debug binary at its original /output/target/debug path and rejects missing/tampered/incomplete manifests. Older incomplete caches do not qualify; failed/unsealed output is never retro-certified.


The second registered engine interface is background-dispatch (`background.plan`, interface 1). Its fixed independent recipe builds webagent-background-service and the declared background-dispatch/contracts plus reusable engine-service closure. Requests contain only bounded lifecycle metadata; storage, authority and execution remain core-owned. New contracts require coordinated release and broker registration installation. Compatible implementation publications retain core consumers only when live adoption and actual compile closure permit it. Each output is sealed by its component identity; multi-component retries must process every output. See docs/engine-modularization.md for current progress/evidence.


## Boot recovery component and host authority
The third fixed engine registration is boot-supervisor (boot.plan, interface 1).
Its declared closure builds webagent-boot-service, boot-supervisor/contracts and
the reusable engine-service wrapper. Stable builds call the Rust library through
--boot-plan. Shared boot-contracts edits require coordinated publication even
when a changed manifest is not a Rust dep-info file. Compatible implementation
updates use the engine lane only with actual API/loop/boot live-mode adoption.

The protected boot_host.py adapter owns Docker event transport, persistence,
desired state, manual/uncertain pauses, crash guards and every container effect.
webagent-boot-host is a fixed host executable reusing the existing pre-exec
confinement; install it only from the successful release's attested artifact,
alongside its protected hash receipt. Library/service descriptors cannot replace
that authority or choose its targets. The UI is an unprivileged keyboard-woken
status/control client outside this checkout. See docs/boot-supervisor.md.

Host-source publication allows only the already-installed fixed boot authority
module and its explicit docs/tests, with exact protected byte equality. Actual
boot adoption also binds the running host-code SHA and rejects an enabled but
unavailable service. Descriptor withdrawal remains valid and retains pending
host input. Source-only host publication does not require container recreation.
The shared effect lease serializes recovery/manual effects with deployment.

After adding a fixed installed module, verify the full installed manifest.
If the prior lifecycle installer omitted a newly inventoried module, use the
newly protected bootstrap's supported --install --install-tree while drained
and stopped, then verify exact bytes before resuming. Do not execute mutable
source as privileged service code or bypass publication/classification gates.

## Broker timings

The background cycle and request handler record content-free timings: step and
operation names, durations and process read/write byte counts, never arguments
or output. Each cycle rewrites `build-jobs/coordination/performance.json` (last
300 steps plus request totals since start, keyed by operation and action). Use it to attribute
idle broker load; it carries no authority and is safe to delete.

`buildctl progress JOB_ID` returns stage k of N for a job, each finished stage's
duration and the running stage's elapsed time against its class p50/p90, and a
remaining-time range. Classes are the work actually done (operation, compiled or
reused artifacts, tests run or reused), computed from completed receipts. The
read follows the same ownership rule as `buildctl job` and never returns logs.

## Compiled-dependency snapshots

Engine compiles start from a verified copy of third-party objects instead of an
empty target. The first job for a key (locked downloads, manifests, compiler
image, flags and compile recipes) builds `build-jobs/compiled-dependencies/<key>`
from a stub workspace: exact manifests, every workspace target replaced by an
empty file, the engine's test, linked and live compiles, then `cargo clean` of
every member and a check that no stub product survived. No task source is
compiled into it. Each job copies the snapshot into its private target and
verifies every file's hash against the receipt; Cargo fingerprints still decide
freshness and all test, smoke and equivalence gates are unchanged. Any failure
leaves an empty target and the job compiles cold (`dependency_snapshot` in the
receipt records hit, prepared, skipped or unavailable). The three most recent
keys are kept; the cache is rebuildable and safe to delete while no job runs.

## Suite and linked compile overlap

After the focused tests, the full workspace suite and the linked release compile
run at the same time. Tests mount the application writable, so the compile reads
a private copy of the captured sources, verified against the same digest and
mounted by no test container; the copy is removed afterwards and the post-suite
digest check is unchanged. A suite failure is reported before a compile failure.
Admission lets phases of one job share their component; other jobs still
serialize per component. Each phase records its own execution history; the live
`execution_observation` shows whichever phase reported last.

## Explicit owner claim release

Operators may use `buildctl schedule --input request.json` with action
`release_claims`, task_id, an explicit scopes array copied from `takeover_preview`
or from `reservations` with that task_id (the unfiltered listing caps each scope
list at 32 and marks it `scopes_truncated` with a notice),
a bounded reason, stable request_id and current expected_revision/expected_epoch.
The authenticated owner can release some or all exactly held claims without
completing or deleting the draft. Active, interrupted or uncertain task execution
must be reconciled first; active index maintenance must finish. Releasing claims
clears acceptance and pending requests and advances the ownership epoch. Later
writes/builds must reacquire their derived claims and pass normal merge checks.
Exact-request retry repairs the registry after interrupted persistence. This is
not task completion, an administrator takeover, or authorization to deploy.

## Concurrent preparation and settled-component indexing

These features are opt-in protected installation policy. Source defaults keep
`ui_file_reservations`, `component_indexing` and `concurrent_ui_preparation`
false, with `max_ui_preparations=1`. Concurrent preparation requires enforced
scheduling and measured component indexing. Enable at most two UI preparations.
Existing held module claims remain exclusive against new file claims; no legacy
parallel-UI flag bypasses enforcement. File reservations cover normalized paths,
with subtree reservations, compiler-attested overlaps and same-file conflicts
remaining conservative. A rename reserves its old and new paths.

The host prepares immutable candidates and private indexes outside publication.
Preparation captures the published base, ownership generation/epoch, source,
metadata dependencies, protected parser recipe, runtime identity and test recipe.
Unchanged hashes reuse parser output and completed matching reviews. Source,
configuration, dependency and descriptor changes invalidate only matching
bindings. Artifact sealing precedes final descriptor-dependent review binding.

Publication admits one exact candidate at a time, with the same coordinator for
UI and full backend releases. Stale bases/runtime bindings cause preparation
outside the lock, at most three attempts before `waiting_publication_base`.
The winning snapshot is installed on the asset filesystem only after durable
intent. Activation, served-byte/health checks and checked promotion are serialized.
Full backend releases incorporate intervening compatible UI changes; compiled
inputs or Rust fixture changes invalidate backend evidence. Eligible UI jobs
retain application images and processes.

Frontend dependency policy is versioned in protected `ui_policy.py`. Unknown
imports/dynamic loading conservatively select all frontend fixtures. The protected
`scheduler.ui_test_image` must be an immutable `sha256:` image ID when tests are
selected. `ui-test.Dockerfile` is the reviewed ancillary test-runtime recipe;
its browser image and Playwright package versions agree. Provision it through operator `build_work` action `prepare_ui_test_runtime`,
with the owning broker `task_id` and stable `request_id`, during a task-owned idle
drain. The fixed protected recipe runs in the existing measured packaging lane
with a 4,026,531,840-byte memory cap, zero swap and retained peak/OOM evidence.
The durable check receipt returns its immutable image ID and recipe hash; no
application image is built or deployed. The operator receipt also retains the
image through a fixed, networkless, read-only sleep holder with no application
mounts, a 32 MiB memory cap, zero swap, 0.01 CPU and eight PIDs. It runs no
browser or tests. At most two runtime versions are retained; reconcile and retire
an unused predecessor before provisioning a third. A running holder survives the host guard's stopped-container
and unused-image pruning; a tag or stopped holder does not. Reconcile that exact job after a client
timeout, and retry only the identical request to retrieve its receipt. Configure
the verified image through supported lifecycle reconfiguration. Fixtures run
read-only with no external network, application data, credentials or Docker
socket; profiles and local fixture servers live only in their container. Node
uses experimental VM modules and test concurrency one. One measured browser
worker admits globally; index scans have measured independent admission. Tests
are reused only when source/dependencies, runtime images and verification recipe
match. A selection manifest alone never reports tests passed.

### Project queue and sealed review continuation

Use the authenticated operator channel, `buildctl schedule --input request.json`.
Project actions are not exposed as new native model tools. Create with action
`index_project_create`, `project_id` and `request_id`; inspect using
`index_project_status` and the project ID. Mutations after creation also require
the current `expected_revision` from status. Attach each isolated task through
`index_project_attach` with `task_id`. Submit `index_checkpoint` with that task ID
and a settled relative file/component path in `component`. Paths, immutable
snapshots, epochs, hashes and private output locations are selected by the host.
A build alone is not a checkpoint. Add human comments and feature breadcrumbs
while editing; checkpoint when that component settles.

One event-driven worker processes ready checkpoints. An invalid component enters
`needs_attention` while unrelated queued components proceed. Scans populate
review work; they never certify documentation. Review affected units with the
existing maintenance channel. Reuse is based on exact per-file parser recipes,
source hashes and metadata dependencies; no process merges mutable SQLite files.
Restart preserves evidence and requires fresh authorization for unfinished work.

A sealed candidate with incomplete applicable reviews enters
`awaiting_prepared_reviews` without activation. The operator can pass
`prepared_job_id` to maintenance sync/claim/record/release to review that exact
host-selected snapshot; normal task-scoped list/get exposes its records. The
maintenance CLI accepts `--prepared-job-id`. Resume the same job with normal
preflight/decide after reviews, rather than submitting a duplicate build. Reviews
against raw descriptors can legitimately become stale after artifact sealing.

`index_project_finalize` verifies completed member publication, acceptance,
commits and the exact current paired source/index. Pending checkpoints must
settle; integration-superseded private evidence is reported as verified by final
publication only after exact freshness and applicable-review checks succeed.
Zero gaps, stale files and unindexed supported files are mandatory. An unfinished
project returns actionable remaining work. `index_project_reopen` creates a new
project generation while retaining reusable evidence.

### Recovery and compensating rollback

Operator `build_work` recovery actions require exact `task_id`, `job_id`, and a
stable `request_id`. `discard_prepared_release` reconciles an uncertain intent
only when the predecessor pointer, runtime identities and checkout prove no
publication effects remain. It retains candidate/index material. For an activated
UI-only failure, `restore_failed_ui` restores its verified predecessor only if
the failed release is still current. It reconciles partial source promotion and
supports exact retry. Newer active pointers or changed runtime bindings refuse
recovery; they must not be overwritten. Revocation and ownership remain enforced.

For an interrupted version-1 backend build/deploy whose original API and loop
containers have already been restored, operator action
`restore_backend_predecessor` reconciles that exact failed release. Both
container IDs, immutable image IDs, Config and HostConfig must equal the private
pre-effect capture, and both consumers must be healthy. Restored process IDs may
initially differ; durable recovery intent pins their current generations for
subsequent exact-request retries. The checkout must still match the predecessor,
and only the failed asset pointer or its verified predecessor is eligible.
Recovery restores the paired source/index pointer, retains the failed candidate
and build evidence, and records a reconciled failed release. It does not adopt
the candidate or replay container replacement. Revocation, task ownership,
maintenance drains, newer-release fences and the shared lifecycle lease still
apply. Use the original task ID, job ID and one stable recovery request ID.

To undo an older successful UI release, begin a fresh empty isolated task and
request `ui_rollback_preview` with its `task_id` and the original
`release_job_id`. The host returns a checked-patch draft plus its exact source
binding, using the receipt-bound publication predecessor. Apply it through normal
`v3_source_apply`, acquire normal claims, review, test, publish, commit and accept.
The preview performs no source or runtime mutation. Binary/oversized patches are
rejected. Later independent changes survive; divergent same-file changes produce
an explicit conflict. There is no automatic hunk merge or historical pointer
switch.

Install exact reviewed broker bytes only during a task-owned idle drain with an
empty queue, through lifecycle installation. Publish source using the classifier's
supported lane; parser, policy and new protected modules do not automatically
qualify for host-only publication. Enable concurrency only after isolated and
live acceptance. Setting `max_ui_preparations=1` restores serial preparation
without dropping claims, evidence or recovery state. Commit receipt-bound promoted
paths with fresh checkout/index checks, preserving unrelated staged work; do not
push without separate authorization.

### Bounded review-history maintenance

If even a one-file sync cannot fit the ledger budget, inspect storage before
changing limits or clearing records. Operator action `compact_index_history`
accepts `task_id` and stable `request_id`. It requires an idle task-owned drain,
no active reviews/checkpoints or queued releases, and held Code Index ownership.
It retains a private consistent SQLite backup, losslessly compresses historical
event snapshots and vacuums free pages without changing the configured storage
limit. Current records, claims, every event and exact retry receipts are retained.
The new reader accepts both legacy JSON and bounded compressed history. A failed
or uncertain request can be retried with identical arguments; no history is pruned.
Retain its backup with the installation rollback material. An older broker cannot
read compressed event history; any broker rollback must use a compatible reader
or reconcile the saved ledger backup and subsequent writes before restoration.

Rollback previews express source modes using Git's owner-execute identity
(100644/100755), not incidental group/other ACL bits from readable snapshots.
Equivalent Git modes emit no chmod; checked patch application preserves the
current permissions. Receipt hashes, same-file conflict checks and normal
publication authority still apply.


### Bounded RAM packaging

Protected scheduler settings `package_ram_bytes` and `package_tmpfs_bytes` are
opt-in and default to zero. Nonzero byte budgets must align to the host memory
page size so Docker and kernel limit readback agree exactly. They select the total packaging-container ceiling
(up to 10,000,000,000 bytes) and its disposable `/var/lib/buildkit` tmpfs (up to
8,000,000,000 bytes, with at least 1 GiB left for process overhead). Enforced
scheduling and at least 15 percent host reserve are mandatory. Reserve the full
ceiling despite earlier small measured peaks; other phases belonging to the
same task wait until packaging finishes. Kernel memory.max covers BuildKit,
its descendants, cache and tmpfs together, with swap disabled. A separate recipe
profile prevents RAM and disk measurements from polluting each other.

The mount is verified before any build workload. Scratch exhaustion or OOM
fails the job without automatic disk fallback or publication retry. Normal
exact-container cleanup removes the tmpfs. Source snapshots, tested outputs,
release receipts, imported images and rollback material remain durable. Adopt
through idle protected lifecycle installation/reconfiguration; changing a live
container is unsupported. Compare the same frozen recipe and inputs before
claiming a speedup; disk import/export and network transfers still remain.

### Prepared-review ownership leases

A sealed release waiting in needs_decision / awaiting_prepared_reviews retains
broker-managed ownership while its exact review delegation is authorized.
The durable record binds task, job, generation, epoch, prepared candidate and
source/index hashes. Ordinary five-minute orphan detection no longer mistakes
that authorized review wait for a lost worker. Review waits do not reserve
compiler/browser capacity and never resume publication automatically.

The authorization deadline remains authoritative. Revocation, changed policy
fingerprints, ownership transfer and changed candidate bindings fence renewal.
Status exposes review state, last authorized activity, deadline and an expiry
warning without private candidate paths. An expired/orphaned owner requires
explicit authorized recovery; source/index/review evidence is preserved and
normal freshness checks still govern resumption of the same durable job.
Unchanged manifest identity is cached for renewal; final publication retains
the exact source/index checks. No source scan or compilation is triggered by
the ownership heartbeat. Trusted lease controls also cover paused reviews.

### Independent private review recording

Task-scoped Code Index sync, claim, record, release and reads use their private
source bindings without waiting for the deployment lock or global published
projection reconciliation. Published snapshot reads capture one immutable active
revision directly. Global live-ledger reads still reconcile the published
projection; an unchanged pointer takes a fast path, and actual reconciliation
rechecks the pointer after acquiring publication serialization.

A concurrent activation replaces only published records, retaining private task
records and retry receipts. A stale published projection may miss evidence reuse;
inheritance still requires matching source/dependency hashes. Host authorization,
claim/revision checks, private freshness and sealed-candidate integrity remain
mandatory. Final publication still reconciles and verifies the exact source/index
and completed applicable reviews. Scans stay outside ledger write transactions;
no deployment lock is held by private review recording.

### Protected compiler limits and live growth

New enforced jobs pin the protected compiler_memory_default_bytes (5 GiB).
compiler_memory_min_bytes and compiler_memory_max_bytes bound operator overrides
through the existing compiler_memory_cap finish/check argument. All values use
bytes and must be memory-page aligned; the absolute supported range is 1–10 GiB.
Already durable jobs retain their recorded ceiling, including legacy uncapped
and 3.75 GiB jobs. Compiler admission reserves the full approved ceiling, rather
than a smaller observed estimate. The effective host reserve is at least 15%.

Operator build_work action increase_memory requires job_id, phase_id,
expected_revision, memory_bytes and request_id. It only increases an active
capped compiler; it never shrinks a running process or changes RAM packaging
scratch. The host validates live authority and source epoch, reserves complete
growth, records intent, and requests the protected helper's exact Docker update.
The helper validates current policy, identity, zero swap, authorization deadline,
host headroom and aggregate ancestor capacity, then confirms kernel memory.max.
Receipts and final compiler evidence retain the requested/observed limits,
growth history, peak RAM and OOM counters. OOM fails; it does not retry a build.

Insufficient growth headroom returns waiting_headroom while compilation retains
its old ceiling. An identical request returns its original result; after capacity
changes use a new request ID with the current phase revision. Uncertain responses
retain the larger reservation and block admission until exact kernel evidence
or stopped-phase cleanup reconciles the outcome. Restart never replays a resize
or thaws an unknown phase. Existing pause/resume/reconcile controls still apply.
Per-phase growth history is bounded; source permissions never expand.

The root lifecycle resource-policy command accepts --config, --candidate-config
and --task-id, plus optional --dry-run. The candidate must already be protected,
root-owned JSON. Only the four memory fields may differ. It persists installation
intent, atomically updates protected JSON, and reloads broker/helper policy with
matching digests without restarting the broker or application. An uncertain
adoption leaves an explicit reconciliation receipt; retry the same desired policy.
New defaults affect new jobs; existing ceilings are never silently decreased.
Other scheduler/topology changes retain idle lifecycle reconfiguration.

For bounded operator acceptance, build_work check with check=resource_probe
uses a fixed rustc --version plus 30-second idle command in an attributable
compiler container. It mounts an empty private source directory, no application
state, and has no network. It uses broker ownership rather than engine claims,
normal full-ceiling admission and the same kernel/cleanup evidence. It builds
or deploys no application artifact. New controls use the operator channel;
native application tool schemas are unchanged.

### Standalone Rust feature checks

Use standalone checks to catch feature-specific imports and cfg errors before a
full optimized build. The canonical decision rule and agent workflow are in
[Codebase Admin: Early Rust feature checks](../plugins/abilities/Administrator/codebase_admin/codebase_admin.skill.md#early-rust-feature-checks).

| feature_profile | Fixed compile recipe |
| --- | --- |
| omitted or default | cargo check --locked --offline |
| live | cargo check --locked --offline --no-default-features --features live-context |

The default profile preserves existing workspace defaults. Nondefault profiles
apply only to check=check; live is rejected for test and resource_probe. Other
profile names and arbitrary Cargo flags are unsupported. The live profile
requires enforced resource admission.

From the v3 checkout on the server, replace the uppercase placeholders with the
owned task ID, its fresh input digest, and a unique stable request ID:

~~~sh
WEBAGENT_BUILD_CONFIG=/etc/webagent/development/build-service.json \
python3 -B /home/ar/webagent-dev3/build-service/buildctl.py check TASK_ID \
  --expected INPUT_SHA256 --request-id LIVE_CHECK_ID \
  --feature-profile live --timeout 600
~~~

Run the companion check with --feature-profile default and a different request
ID against the same unchanged inputs. The operator broker API accepts the same
selection as feature_profile on build_work action=check. The in-app native tool
schema does not yet expose this field; use the authorized operator surface.

The selected profile is recorded in job status and check results and binds the
durable request signature. Reusing a request ID for another profile is rejected.
A queued/running receipt is not success: inspect its existing job until completed
with the expected profile and input digest. Reconcile uncertain calls before
retrying; edits or stale inputs require fresh affected checks.

Checks retain isolated source capture, ownership, input-integrity validation,
compiler ceilings and resource admission. They do not deploy, promote source or
start documentation/index maintenance. They supplement applicable tests and
optimized linked/live equivalence; release gates still apply.

### Bounded review ledger capacity

The root-owned installation field code_index_ledger_max_bytes bounds the shared
review ledger on disk. It defaults to 768 MiB and accepts page-aligned values
from 512 MiB to 1 GiB. Adoption requires broker maintenance/restart; it is not a
live compiler-memory field or a model argument. This is a storage ceiling, not
a RAM reservation or permission to remove history. Try supported lossless
history compaction first and retain its backup and original retry receipts.
Final publication still requires the exact source/index and applicable reviews.

If a retained-runtime UI publication becomes uncertain after its pointer switch,
the operator complete_activated_release recovery action binds task_id, job_id
and request_id to that same latest owned release. It verifies current authority,
ownership/generation, exact active source/index and applicable reviews, retained
runtime identities, served bytes, and checked source promotion. It completes the
existing receipt without reactivating assets, rebuilding images, or dropping
claims. A changed pointer/runtime, expired authority, or checkout conflict fails
closed; retry the original recovery request after resolving its exact blocker.
UI-template-only plugin_deploy releases (UI files plus config/chat.json) use the
same recovery with their normal consumer-adoption and engine-mode checks.
Executable plugin changes are excluded from this UI recovery action.

### Edited-waiter acquisition reconciliation

An edited task waiting for claims is rejected before a new acquisition commits.
Preserve its implementation and replan in a fresh task; acquisition never discards
unpublished source to refresh the task.

For a legacy reserve that committed ownership and then failed before refreshing
an edited, previously empty waiter, the authenticated local operator may use
build_work action reconcile_acquisition. Supply task_id, reserve_request containing
the exact original reserve JSON, expected_inputs_sha256 from the preserved task,
fresh expected_revision and expected_epoch, and a new stable request_id.
Save that entire request before dispatch and reuse it exactly after uncertainty.

Recovery requires the original pending request and committed receipt, matching
owner/epoch/generation/scopes, an unpublished open draft, no unsettled execution
or active reviews, and no refresh filesystem effects. It uses the normal audited
claim-release transition to release exactly that acquisition and advance its epoch,
then explicitly rejects replay of the obsolete reserve. It retains all source,
the sealed baseline, and original acquisition evidence. The returned proof binds
the preserved hashes and released scopes. A completed exact retry returns its
original result and never releases subsequent claims. Changed inputs, ownership
or partial refresh effects require separate reconciliation; this action neither
refreshes the old draft nor transfers edits into a fresh task.

### Task-aware Docker retention and disk admission

The installed docker_retention.py replaces host-wide container/image/builder
prune calls. The host guard keeps its 80 percent trigger and 70 percent best-effort
target, but deletes only a bounded batch of registered expired V3 artifacts.
Unknown/foreign images, shared builder caches, referenced images and rollback
containers remain untouched. Root-owned exact-image/phase records share a short
cleanup lock with registration and deletion; missing or inconsistent evidence
defers deletion. Job receipts retain queued, running, review-waiting and unresolved
work. Completed/cancelled jobs retain a minimum two-hour grace period.

Config: scheduler-defaults.json -> task_aware_cleanup, disk_min_free_bytes,
disk_grace_seconds, disk_cleanup_batch, merged with protected installation JSON.
Disk admission preserves 8 GiB free plus outstanding phase reservations. Cold
compiler/package floors are 6/8 GiB; other phases reserve 1 GiB. Recipe-bound
measured output footprints may raise these estimates. Resource waits identify
disk capacity and resume through existing cancellable admission, without
duplicating builds. Root helper failure is not permission to bypass admission.

Run the installed guard without --apply for a preview; --force only bypasses
the disk-pressure trigger, never retention or age checks. --apply deletes exact
eligible IDs without force. Last cleanup results remain in protected metadata.
No shared Docker cache pruning is supported. Installation uses the normal idle
broker lifecycle; the host timer remains enabled and runs the protected version.


Cleanup defaults disabled for staged rollout. The dispatcher defers until the
protected task_aware_cleanup flag is enabled through supported idle scheduler
reconfiguration. Preview the installed docker_retention.py directly without
--apply before enabling. --acceptance-probe uses two tiny disposable Docker
imports and isolated synthetic job/ownership/retention records; only that private
fixture clock advances. It proves expired deletion and queued-image retention,
then removes its own images through the same eligibility checks. Real release
metadata and application containers are not changed.


### Task-partitioned review storage

The protected host uses a per-task mutable review database and a separate
published projection. Immutable release indexes remain complete source/index
pairs. A small coordinator routes global retry identities and accounts for the
existing aggregate storage limit; it does not hold scans or review bodies.
Independent task transactions can proceed concurrently. Publication snapshots
take a bounded consistent read barrier without holding the host authority lock.

Existing installations retain the legacy layout until an operator invokes
build_work action partition_review_storage with task_id and request_id during an
idle task-owned drain, holding module:broker. The migration verifies every record,
history event and exact retry receipt, persists recovery intent and retains the
original database. A directory fences the retired ledger.sqlite pathname so an
older installation fails closed. Do not remove that fence or restore old broker
bytes alone: reconcile storage and protected installation together.

The original aggregate byte and record limits still apply across all stores.
Private writers acquire coordinator allocation only at commit, atomically with
their shard transaction. A request identity remains bound to its original task
and input digest even if validation fails; changed inputs need a new request ID.
No background migration, history deletion or automatic task-store retirement is
performed. Explicit lossless history compaction still requires an idle drain.


### Explicit approval to defer documentation assertions

Required functional/safety checks and complete, fresh, exact source/index binding
remain prepublication gates. An operator may record the user's approval of a
sealed candidate awaiting documentation review with build_work action
approve_deferred_documentation, task_id, job_id, expected_inputs_sha256,
approval_note and request_id. This does not resume a job automatically.
A source rebase, sealed descriptor change or ownership/source generation change
invalidates the approval. A build or generic resume is never approval.

This approval path applies only to the protected before_deployment policy.
After normal publication, record Code Index/documentation assertions with
code_index_maintenance deployed_job_id selecting that exact owned release.
The host selects its immutable source; the index is never rewritten. Call
finish_deferred_documentation with task_id, job_id and request_id after all
applicable reviews, including removed/renamed units, are complete. The host
projects fresh evidence in memory and persists an exact completion receipt.
Any later review mutation invalidates that receipt. Task acceptance/completion
and project finalization never wait for these reviews. Later builds reuse only
source/dependency-matching assertions; they cannot inherit stale approval.


## Cargo module source reservations

Known Cargo package directories receive their own source scopes:
`crates/modules/<name>/` uses `module:backend:<name>`, Diagnostics retains its
actual package path with `module:backend:diagnostics`, and other independent
packages registered as workspace members or local Cargo dependencies use
`module:crate:<source-directory>`. An unregistered nested manifest cannot take
ownership away from its parent. Root application Rust uses
`module:general`. Exact path claims remain mandatory. New extractions must
include a real Cargo manifest and verify their actual derived scope; a folder
name or a ledger entry alone is not an independent package.

Claims cover only the owners of the edited files; consumers are protected by the
build, synchronization and promotion gates, which rebuild and retest against the
current source before anything is promoted. Cargo manifests are read as the
checked patch leaves them, so a crate registered by the same patch reserves its
own module scope at the first apply. When Cargo metadata is missing, invalid or
cannot be reconstructed, the owner comes from the source layout instead
(`crates/modules/<name>/`, component crates, `crates/<name>/`, `src/`), never from
a global fence. Shared build files (`Cargo.toml`, `Cargo.lock`, `build.rs`,
`Dockerfile`, ...) and unknown top-level paths reserve `module:workspace`, which
conflicts only with other `module:workspace` claims. Compiler attestations decide
the release lane, not write claims. No path derives `module:engine` any more,
and a stored legacy `module:engine` claim or request is an ordinary token that
conflicts only with itself; acquisition always derives scopes from the paths being
edited and never replays stored requests.
No manifest field can declare itself exempt from conflicts.

This changes source editing reservations, not execution concurrency, resource
ceilings, promotion freshness, source attestation, acceptance or deployment gates.
Rust builds and checks serialize through the `execution:rust-build` execution
token, which never blocks source edits.
For each future module separation record: actual Cargo root and scope, dependency
contracts/consumers, independent-edit and contract-conflict tests, legacy overlap,
and the release/commit proving the installed classifier recognizes the boundary.

Unsplit backend application source uses General (`module:general`); exact UI,
asset, ability, broker and service reservations retain their established rules.
Shared workspace/build files and unknown paths use `module:workspace`; there is
no global source fence. Cargo ownership does not replace
explicit runtime/IPC component contracts or deployment lifecycle coordination.
