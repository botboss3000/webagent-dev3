---
id: code_verification
title: Code verification
when_to_use: Use to review code or a diff, assess tests, diagnose failing checks without changing code, validate a fix or feature, audit quality, identify coverage gaps, or judge WebAgent v3 release readiness.
default_mode: discoverable
examples: review this diff; why is the test failing; check if the fix is ready to release
keywords: review, verify, tests, audit
minimum_access: registered
---

# Code verification

Determine what the evidence actually establishes. Keep verification independent
from implementation assumptions, distinguish requirements from general quality,
and do not repair findings under a review-only request.


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

## Evidence identity and boundaries

- Check the live catalog, activation and effective policy before relying on a
  tool; manifest presence and old guidance do not prove callability. Each
  tool's own skill gives its actions and limits.
- Read an existing isolated change from its own work item, not a similarly
  named live file. Live reads are live evidence, not candidate evidence.
- Record the work item, baseline, input digest, check, environment and result.
  Confirm inputs stay unchanged; a run racing edits is stale even when green. A
  pending job is not completed verification.
- Bind evidence separately to task source, executable, guidance/catalog revision
  and deployed release. Runtime evidence validates the deployed release, not an
  undeployed candidate. For non-Rust changes load `live_plugin_publication`.
- Verification never authorises finish, restart, deployment or synchronization.
  Never duplicate an uncertain finish. Honour API-only validation when asked;
  report inaccessible coverage instead of forcing a browser journey.
- Never bypass bounded inspection tools, read application databases or print
  credentials.

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

## Parallel checks

Parallelism is optional and grants no new tools. Run checks in parallel only
when they have no ordering dependency, no shared mutable state or output races,
and each result stays attributable; serialize shared databases, ports, services,
accounts and live-environment checks. Aggregate centrally, resolve
contradictions against primary evidence, and run required integrated checks on
the final state. Agreement between workers is not proof.

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
