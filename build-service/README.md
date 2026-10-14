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
