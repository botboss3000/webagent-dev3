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
A deleted standalone plugin file uses its sealed task-baseline manifest to retain
ability ownership when its current manifest is also removed. Existing undeclared
Rust files and compiler-attested inputs retain conservative engine ownership.
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
Complete Code Index review mutations while the task is open, before finish closes
it; use a supported continuation or ledger publication for later review work.
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

## Explicit owner claim release

Operators may use `buildctl schedule --input request.json` with action
`release_claims`, task_id, an explicit scopes array copied from reservations,
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
