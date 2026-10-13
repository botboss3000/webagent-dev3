# Controlled V3 builds and source ownership

The protected broker separates source ownership, attributable execution and short serialized publication. Independent isolated ability packages can compile in parallel; the monolithic engine and its actual compiler inputs remain one component. Core image builds share an engine artifact component even when a task edits only packaging or broker code. No manifest may hide a compiled dependency.

## Source lifecycle

Use one task ID and inspect reservations before editing. build_work reservations exposes module/path scopes, waiting requests, epochs and whether this caller owns a task. reserve takes source-relative paths with an expected ownership revision/epoch and stable request ID. It derives component claims and acquires the entire expanded scope atomically. Conflicts return concrete blockers; unrelated writers can proceed. A queued unchanged draft refreshes to current published code when acquired. Reread its task digest before patching; preserve and replan a waiting draft that already contains changes.

Source file writes without a task create/reuse an isolated draft and return its ID. Continue with that ID and the latest task digest. Explicit runtime/workspace paths retain their domain tools; data, generated output and inspection snapshots are excluded from source writes. Shell/Python commands run in the session workspace with read-only source access. They cannot modify source or sibling workspaces. Use checked broker file tools or operator buildctl apply for source edits.

Cargo checks with a stable request ID return a durable queued receipt. Poll the same request or its job ID. Checks use the same component admission queue as release builds; queued checks do not occupy execution workers. A restart interrupts a check rather than replaying it. Reuse the request ID to reconcile its outcome, then use a new ID for a deliberate retry.

finish freezes one generation and schedules its supported checks/build/publication. It never marks the logical task complete. continue reopens a completed release for corrections, preserves the old generation and invalidates acceptance. It pins a new baseline and uses a recovery journal. Exact-request retry resumes an interrupted transition.

After healthy synchronized promotion and the exact release commit, record acceptance with the release job, commit and successful bounded checks performed by the agent. The broker verifies promoted bytes, commit history, current dependency closure, latest generation, release checks and healthy consumers. These machine checks do not replace behavioral acceptance. complete rechecks evidence and explicitly releases claims. Build-complete badges, stopping a session or hiding a receipt cannot release ownership.

handoff atomically transfers claims to an empty caller-owned followup, which refreshes to current publication. The old epoch is fenced and its draft preserved. Administrator takeover_preview and takeover require an explicit reason and no unresolved execution/effects. Cancel safe jobs and reconcile publication effects first. Unpublished edits in the old draft are preserved for reviewed recovery rather than silently merged.

Heartbeats retain activity without extending tool authority. Orphaned claims remain held until explicit recovery or administrative transfer. Valid bounded delegated jobs keep ownership active. At enforcement cutover the broker derives claims for unfinished legacy work. Historical immutable completed generations remain classified separately; successful builds are not newly certified agent completions.

## Resources and execution

Protected scheduler settings override reviewed build-service/scheduler-defaults.json. Operator lifecycle reconfiguration adopts policy by restarting only the broker while its exact maintenance drain is idle and the queue empty. Application images remain unchanged. Observe mode retains the legacy build path; enforce mode admits measured phases and enforces claims.

Cold estimates guide admission rather than impose hard per-task RAM limits. Successful measured peaks inform later estimates. Admission accounts for host reserve and admitted phases' remaining peak demand. CPU quotas share a protected aggregate pool. Compilation, tests, dependency fetch and dedicated BuildKit packaging are identified separately. Each process group is bound to an exact generated Docker identity and cgroup inside the fixed instance slice. Packaging profiles distinguish API and loop contexts. Each context contains only artifacts declared by that runtime stage and streams from an owned temporary file; retained test executables are verification inputs rather than image inputs.

The controller samples RAM, swap, peak memory, CPU, I/O and pressure. Under memory pressure it may freeze only a compiler phase, confirm the kernel acknowledgment and attempt bounded reclaim/swap. It credits measured headroom rather than requested bytes. Tests, package publication, deployment and external effects cannot be automatically parked. CPU contention reduces fair quotas instead of freezing jobs.

A manually parked compiler waits for explicit resume. Automatic resume requires stable measured headroom. Insufficient reclaim waits for intervention; it never automatically cancels or restarts a build. Frozen intervals are excluded from execution timeouts while authorization expiration and cancellation remain effective. Broker restart fences orphan phases until exact-container reconciliation. Failed cleanup retains recovery state and blocks admission.

## Interfaces and verification

Use the existing administrator Builds page at /app?tab=builds. Unfinished tasks remain visible independently of receipt filters. Resource cards expose opaque phase IDs and bounded pause/resume/priority/reconciliation controls. Completion belongs to the actual owner; administrative takeover is explicit.

Operators use buildctl schedule --input request.json for structured actions with revisions, epochs and stable request IDs. Agents use build_work. Model-supplied owners, host commands, PIDs and cgroup paths are never accepted.

Publication retains synchronization checks, dependency-aware rebasing, live authority, frozen input attestations, paired Code Index, healthy adoption and rollback. Privileged host edits are outside agent locks; checkout drift is detected before promotion/completion. Logical reservations cannot prevent a privileged SSH operator from changing files.

Acceptance must cover concurrent unrelated compilation, same-component serialization, corrections before completion, fresh queued baselines, duplicate requests, handoff fencing, orphan/restart recovery, resource controls, source-write confinement and authenticated Builds journeys. Retained test and release receipts provide evidence; this document alone is not deployment evidence.
