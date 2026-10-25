---
id: code_verification
title: Code verification
when_to_use: Use to review code or a diff, assess tests, diagnose failing checks without changing code, validate a fix or feature, audit quality, identify coverage gaps, or judge WebAgent v3 release readiness.
default_mode: discoverable
minimum_access: registered
---

# Code verification

Determine what the evidence actually establishes. Keep verification independent
from implementation assumptions, distinguish requirements from general quality,
and do not repair findings under a review-only request.

Supplement the standard Codebase Admin skill. Repository instructions, current
tool schemas, and live runtime policy remain authoritative. This skill grants
neither tool access nor deployment authorization. Work on v3 only; v2 is
read-only reference material and production requires a separate explicit request.

## Preserve verification independence

- Treat the user's explicit requirements and final decisions as authoritative.
- Default to read-only source inspection. Existing checks may create ordinary
  temporary build/cache output, but do not edit source, tests, configuration,
  lockfiles, or infrastructure without authorization.
- Do not install dependencies, alter external systems, deploy, commit, push,
  or fix findings unless those actions are separately in scope.
- Inspect repository instructions, current Git status, relevant existing diffs,
  and the exact target before drawing conclusions. Preserve other writers' work.
- Evaluate against requirements and independent invariants, not against the
  implementation's own structure or comments.
- Report adjacent findings without expanding the audit target.
- Rust quality guidance to add regression tests applies to implementation work.
  A review-only request authorizes inspection and existing checks, not new tests.

If asked to add tests, enter bounded test-writing mode: change only authorized
tests, fixtures, and indispensable test infrastructure through task file tools.
A failing test exposing a production defect is a valid result. Do not change
production code merely to make the new test pass without a separate instruction.

## Define the verification contract

Establish:

1. Target: files, diff, commit range, component, API, artifact, or running behavior.
2. Baseline to compare against, including the task's source baseline.
3. Explicit requirements and acceptance criteria.
4. Repository standards and supported environments.
5. Material failure modes and risk boundaries.
6. Allowed actions, resources, and effective tool availability.
7. Evidence necessary for a pass decision.

When specifications are incomplete, separate confirmed requirements from
assumptions. Ask only when ambiguity changes the verdict or makes a check unsafe.
Trace the actual v3 entry point, callers, worker boundary and consumers; file
presence, v2 behavior, and a Connected badge do not establish v3 execution support.

## Build traceability before execution

Create a compact working matrix:

| Requirement or risk | Observable behavior | Best evidence | Result | Gap |
| --- | --- | --- | --- | --- |
| Exact requirement or invariant | Public output or state transition | Named check and bound inputs | Pass/Partial/Fail/Inconclusive | Missing layer or environment |

Keep two independent review axes:

- Specification: does every requested behavior match, including edges and failures?
- Engineering quality: is it correct, understandable, maintainable, secure,
  efficient enough, operable, and consistent with repository conventions?

Passing one axis must not conceal failure on another. Do not average material
failures into a pass.

Prefer evidence in this order when applicable:

1. Direct observation in the required environment.
2. Executed behavior at a public boundary.
3. Contract or integration evidence.
4. Unit or property evidence.
5. Static analysis and source inspection.
6. Documentation or assertions without execution.

Lower-ranked evidence may be appropriate when execution is unsafe or unavailable.
State the limitation rather than overstating certainty.

## Use the available v3 tools

Check the current catalog, tool schemas, activation and effective policy first.
Manifest presence and historical tool guidance do not establish callability.
Use exact available ability/tool IDs; natural-language discovery can miss tools.
Tool settings never grant permission. Honor approvals, revocation, cancellation,
session mode, installation policy, account tier and membership.

### Source and diff inspection

Use file_list, file_read and file_search. For an existing isolated change, reuse
its exact task_id and read its source rather than a similarly named live file.
A task-scoped read can seed a missing task; this creates a snapshot, not authority
to edit or deploy. Do not create a replacement task just to inspect an existing
one. Live file reads identify live checkout evidence, not candidate evidence.

Task paths are source-relative, such as src/example.rs. file_list uses offset
from zero and limit at most 100; file_read uses one-based offset and limit at
most 200. Continue pagination until affected functions and context are complete.
Task file_search is literal across source, without regex/path/glob/comment
filters. Narrow searches and inspect matches; do not infer absence from a bounded
or truncated result.

Use build_work(action="review", task_id=...) for the task diff and current digest,
and status for lifecycle/job state. Review baseline, tracked changes and new
files; a truncated diff is incomplete evidence. Obtain repository Git status and
baseline evidence through the permitted inspection path or authorized bounded
shell; file listings alone do not prove Git state.

### Code Index navigation and freshness

Load Code Index and its comment_maintenance guidance when relevant. Use lookup
and trace to locate source, then read both ends of important relationships.
Static edges, lexical coverage and parser fallback spans are navigation evidence,
not proof of runtime behavior or semantic completeness. For deployed-source
inspection use root="published"; private workspace roots describe their own
snapshots. Do not confuse either with an isolated broker task.

Inspect coverage pages, exclusions and parser limitations. Require zero gaps,
stale files and unindexed supported files for a freshness claim. Reads do not
silently rescan. Derived scans may be used when permitted by the review scope;
published snapshots are immutable and cannot be rescanned in place.

Code Index coverage and maintenance reviews are not release-readiness
requirements; do not report their absence as a release blocker.

Distinguish derived indexing from human maintenance certification. A review-only
request does not authorize source comments, feature-map edits, claims or ledger
completion writes. Inspect existing maintenance evidence and report missing
reviews. When documentation maintenance is explicitly authorized, follow
comment_maintenance: task sync, bounded claims, verified connections/configuration,
all five checks, current hashes/revisions, and child completion before file
completion. Scanning alone never certifies documentation accuracy. Report exact
unfinished affected units rather than silently completing them.

### Existing checks and evidence identity

Use build_work(action="check", task_id=..., expected_inputs_sha256=...,
check="check" or "test", timeout_seconds=...) against exact isolated inputs.
The digest must come from a fresh task read/list/review; timeout is 1-600 seconds.

- check="check" runs cargo check --locked in the pinned Rust image.
- check="test" runs cargo test --locked -- --test-threads=1 there.
- Task source is read-only during these checks and Cargo output is separate.
- This interface does not offer arbitrary commands, targeted test filters,
  formatting, Clippy, standalone doctest selection, benchmarks or profiling.
  Do not invent actions or unsupported parameters.

Record task_id, baseline, input digest, command/check type, environment/toolchain,
completion status, exit status when exposed, and material results. Include
relevant untracked inputs. Confirm source/build inputs remain unchanged after
checks; a run racing edits is stale even when green. Inspection database snapshots
are separate inputs requiring their own attestation, not application build inputs.

A returned job/task handle or pending receipt is not completed verification.
Inspect the existing status/job and completion evidence. After uncertainty,
inspect receipts and effects rather than blindly repeating the operation.

### Shell, Python and additional checks

shell and python start from /live-checkout, subject to current policy and OS
mount permissions. They are not automatically scoped to a broker task and are
not isolated from sibling v3 workspaces. Inspect proposed effects and coordinate
shared outputs before running them; never use them to bypass task editing,
approval or broker restrictions.

Discover check commands from repository documentation, manifests, CI and nearby
conventions. Confirm toolchain availability and a permitted execution path bound
to exact isolated inputs before running additional checks. Do not install tools
merely for a review or substitute live-checkout results for task verification.
Report unavailable formatting, linting, targeted tests, worker suites, profiling,
coverage or other required layers as gaps.

### Independent packages and release identity

Load live_plugin_publication before evaluating ability/plugin guidance, JSON,
UI, Python or other non-Rust changes. Select verification and publication by
actual consumers and dependencies, not filename extensions. Protocol-compatible
Codebase Admin package updates can use plugin_deploy without replacing core
containers; new tool/schema interfaces or selectable catalog IDs require
coordinated installation under the current loader.

Live file results identify implementation_version, implementation_sha256 and
plugin_revision. Compare the executable hash to establish which code executed.
A changed immutable package revision may contain only guidance changes and does
not prove a new executable. API and loop adopt existing published guidance at
model-call boundaries; each worker invocation pins its package revision.

Bind evidence separately to task source, executable implementation, guidance/
catalog revision and deployed release. Verify the actual API and loop consumers,
not only the host file or content endpoint. For an authorized no-compile
publication, confirm the broker lane and retained image/container identities.
Do not force a lane with incidental edits, bypass the broker, or implement a
loader migration during an unrelated verification request.

### Runtime and browser acceptance

Runtime evidence validates the deployed release, not an undeployed task candidate.
Record the release identity and reconcile it with the reviewed inputs before
claiming runtime acceptance of a change. Verification alone does not authorize
build_work finish/decide, restart, deployment or synchronization writes. When the
larger request authorizes release, follow standard review, unchanged-input checks,
preflight, synchronization and healthy-release verification. Inspect the same
durable job after interruptions; never duplicate an uncertain finish.

Use Diagnostics read_diagnostics for bounded sanitized current-session/run
checkpoints. It is not evidence of raw model text, tool arguments/results or
private receipt reasons. state_inspect supplies bounded current-session metadata,
not arbitrary sessions or unrestricted databases. Never bypass these boundaries
or print credentials. Read-only broker inspection snapshots may be used only
through their supported bounded text exports; never patch them.

For browser validation, load Diagnostics and applicable journey guidance and
resolve the registered v3 route first. login_as_current_user uses the current
caller's private authenticated profile. dev_browser_url/dev_browser_action are
limited to accepted routes at the fixed development origin and current authority;
do not assume arbitrary API, other-session, external-origin or JavaScript access.
Read before each click/type and after observation invalidation. After uncertain
input, inspect current state before another action. Screenshots are bounded masked
PNG evidence; dev_browser_record produces bounded PNG frames, not a video.
Close the browser and stop recording after use.

Honor API-only validation when requested. Use an available authorized API path;
the private browser's bounded API routes are not a general API client. Report
inaccessible coverage rather than forcing a browser journey.

## Review in evidence-producing passes

### 1. Establish the changed surface

- Resolve the exact baseline and final state.
- Inspect changed callers, consumers, schemas, configuration, generated artifacts
  and public interfaces.
- Identify indirect effects, error paths and state transitions.
- Load rust_quality for relevant Rust quality analysis without expanding edit scope.

### 2. Inspect tests before trusting them

Map tests to requirements and important risks. Prefer public behavior or stable
seams. Expected values must derive from a specification, invariant, standard,
known example or independent calculation.

A strong test has a justified risk, an independent oracle, meaningful positive,
boundary and failure cases, controlled time/randomness/network/concurrency,
deterministic cleanup/isolation, and a useful failure message.

Flag tautological assertions, excessive mocking, missing negative cases, ignored
failures, repeated production algorithms as expected values, unnecessary private
call assertions, mocking the exact boundary being verified, semantically unreviewed
snapshots, oversized end-to-end tests, high coverage missing critical invariants,
and retries hiding nondeterminism.

For regressions, seek evidence that the test detects the prior defect using a
safe baseline or deterministic reproduction. A test that passes before the fix
may never expose the claimed defect. Do not modify shared/live source to prove
baseline sensitivity. Coverage measures execution, not assertion quality.

### 3. Review implementation quality

Check correctness first, then readability, maintainability, architecture,
security, performance, operability and repository consistency. Focus on observable
consequences; do not inflate personal style preferences into findings.

| Axis | Question | Useful evidence |
| --- | --- | --- |
| Specification | Every requested behavior matches? | Acceptance examples and boundaries |
| Correctness | Valid/invalid inputs produce wrong state? | Invariants, negative tests, error paths |
| Readability | Intent and failure behavior understandable? | Focused source review |
| Architecture | Responsibilities/dependencies coherent? | Caller-consumer and contract tracing |
| Security | Trust boundaries or secrets can be abused? | Negative authorization tests |
| Performance | Representative workload meets limits? | Repeatable benchmarks/profiles |
| Operability | Failures detected, contained, recovered? | Retry, restart, migration, rollback |
| Test quality | Meaningful regressions detected? | Requirement coverage and sensitivity |

For affected v3 boundaries, consider policy denial, approval and denial,
session-scoped revocation, cancellation, stale revisions, worker identity,
uncertain effects, restart recovery, transaction/idempotency guarantees, and
exactly one terminal publication. Distinguish approval, denial and cancellation.
Keep test mutations in authorized development sessions/workspaces; do not restart
the live service or perform external writes under a read-only review.

### 4. Execute proportionate checks

Prefer this sequence when supported and safe:

1. Focused reproduction or affected test.
2. Affected unit, integration and contract suites.
3. Type checking, linting, formatting and build validation.
4. End-to-end/runtime checks at real boundaries.
5. Security, performance, migration, concurrency and recovery checks for present risks.

| Risk surface | Minimum useful evidence | Broader evidence when material |
| --- | --- | --- |
| Calculation | Unit/property test | Boundary/randomized cases |
| API/event contract | Contract test | Consumer integration |
| Database/migration | Real storage integration | Restart, partial failure, rollback |
| User workflow | End-to-end behavior | Browser/accessibility/platform variants |
| Concurrency | Deterministic scheduling test | Stress after deterministic coverage |
| Security | Negative/authorization tests | Dependency/configuration analysis |
| Performance | Controlled benchmark | Profile, variance, resource ceiling |
| Recovery | Controlled failure/fault injection | Restart/data integrity |

Use repository-defined commands, not invented stack-specific procedures. Match
evidence to the risk even when the available broker command runs a broader suite.
A green command proves only what it exercised.

Investigate failures sufficiently to classify them:

- Implementation defect: requirement or invariant violated.
- Test defect: invalid oracle, setup, isolation or assertion.
- Environment limitation: dependency, permission, platform or service unavailable.
- Flaky result: identical runs disagree without an understood cause.
- Pre-existing failure: reliable baseline evidence proves it predates the target.
- Unresolved: available evidence cannot distinguish causes.

Do not call failure pre-existing without baseline evidence, flaky merely because
a retry passed, or silently retry until green. Do not automatically fix the cause.

## Use adaptive parallelism

Parallel execution is optional and grants no new tools or authority. Discover
available task-execution support, workers, CPU/memory and isolation; retain a
sequential fallback. Do not invent reviewer agents or delegation tools.

When supported and within scope, start with two to four independent workers and
adapt to contention/project guidance. Assign orthogonal read-only questions and
raw target context; do not supply another reviewer's conclusion as the expected
answer. Use only effective Rust execution flags and consume actual completion
evidence. Diagnostics remains foreground/stop with concurrency one.

Parallelize only if every answer is yes:

1. No ordering dependency?
2. No shared mutable state, or proven isolation?
3. No races on outputs, caches, fixtures, snapshots or generated files?
4. Each result attributable to a command and environment?
5. Coordinator responsible for aggregation and contradictions?

Serialize shared database, port, service, filesystem fixture, rate-limited API,
device, account and live-environment checks unless isolation is proven.
Broker scheduling remains authoritative; separate output directories alone do
not establish concurrency safety.

Aggregate centrally, deduplicate findings, resolve contradictions against primary
evidence, and run required integrated checks on the final state. Parallel agreement
is not proof; one direct reproducible result outweighs unsupported summaries.

## Grade evidence and findings

- Pass: fresh direct evidence covers every required criterion; no material
  unresolved finding remains.
- Partial: relevant evidence passes but an important criterion/environment/layer
  is missing.
- Fail: direct evidence shows a material requirement or risk is not satisfied.
- Inconclusive: conflicting or insufficient evidence prevents a defensible decision.

For each finding provide severity, confidence, precise location/behavior, violated
requirement/invariant/risk, reproduction/evidence, consequence and affected users
or systems, and the smallest remediation direction without implementing it.

Order findings by severity. Estimate severity from impact, likelihood, reach,
detectability and recoverability:

- Critical: immediate severe compromise, irreversible loss or unsafe release.
- High: major requirement failure, exposure, corruption or widespread outage risk.
- Medium: material bounded malfunction or maintainability risk with realistic trigger.
- Low: limited impact or uncommon trigger.

State confidence separately:

- High: directly reproduced or established by strong evidence.
- Medium: multiple indicators support it but execution is incomplete.
- Low: plausible inference needing further verification.

Label inference and distinguish confirmed, likely, historical and unverified
information. Harmless preferences are not findings. Place non-blocking observations
after material findings.

## Report the verdict

Lead with material findings. If none exist, say so plainly and name residual gaps.
Then report:

- Specification verdict and engineering-quality verdict.
- Requirement-to-evidence summary.
- Exact commands/tool actions, bound inputs, environments and completed results.
- Coverage gaps, environment limitations, flaky or pre-existing failures.
- Actions deliberately not performed because they were outside scope.

Scale the report to the change without dropping material evidence. Do not issue
a pass based only on code reading when executable verification was available,
an old test run, coverage percentage without behavioral mapping, or another
worker's unsupported conclusion.
