---
id: rust_quality
title: Rust quality and optimization
when_to_use: Use when writing, reviewing, refactoring, debugging, or optimizing WebAgent v3 Rust code.
default_mode: discoverable
examples: write Rust code for this feature; refactor this Rust module; fix a borrow checker error
keywords: rust, cargo, clippy
minimum_access: registered
---

# Rust quality and optimization

Supplement the standard Codebase Admin skill. Repository instructions, current
tool schemas, and live runtime policy remain authoritative. This guidance grants
neither tool access nor deployment authorization.

## Understand and preserve behavior

Read the relevant task source, callers, types, error paths, and tests, including
complete affected functions and sufficient surrounding context. Do not guess
signatures or assume v2 behavior applies. Identify the problem, intended behavior,
and invariants before editing; preserve unrelated work.

Rust owns the agent loop, providers, policy, approvals, recovery, storage, and
publication. Preserve the isolated worker boundary. Never bypass validation,
policy, approvals, cancellation, recovery, or publication checks for speed.
For affected behavior, test denial, revocation, cancellation, restart recovery,
uncertain effects, and duplicate publication prevention as applicable. Preserve
transactional and idempotency guarantees.

## Ownership, errors, and async design

Prefer references and slices when ownership is unnecessary, and moves when it
transfers. Before adding clone, Arc, Mutex, or RwLock, identify the required
lifetime, sharing, and synchronization behavior. Use proportionate abstractions
and explicit states when they prevent invalid transitions. Follow repository
formatting and lint conventions; avoid arbitrary line-count targets.

Return Result for expected failures, preserving useful context and existing
error conventions. Use typed errors when callers must distinguish failures.
Avoid unwrap, expect, and panic on production failure paths. An invariant-based
exception needs a documented justification and appropriate coverage.

Preserve secret storage and redaction. Do not expose credentials, private
messages, or tool payloads in logs or benchmark reports.

Avoid blocking async executor threads; use the established blocking-work
mechanism. Keep lock scopes short and avoid locks across await unless explicitly
required by the design. Bound queues, concurrent work, retries, and resource use.
Account for timeouts, cancellation, partial effects, and recovery.

Do not add unsafe solely to meet a performance target. Necessary unsafe needs
a specific safety rationale, documented invariants, and suitable verification.
Add dependencies only for a concrete benefit.

## Optimization experiments

For optimization tasks, define the workload, metric, correctness requirements,
regression tolerance, and experiment budget. Establish an unchanged baseline
before editing. Compare candidates with the original baseline and best accepted
candidate. Make one coherent change at a time so results remain attributable.

Investigate complexity, repeated I/O, serialization, allocations, cloning, and
contention before SIMD, parallelism, or specialized data structures. Compiler
success and code appearance are not performance evidence.

Cover representative small and large inputs, ordinary and adverse cases, and
relevant concurrency. Validate cases outside the optimization dataset to detect
overfitting. Compare outputs and observable effects with established behavior or
an independent reference; do not preserve known bugs merely to match a baseline.

Measure relevant end-to-end behavior as well as hot paths. Consider latency
distributions, memory, queue growth, and cancellation responsiveness alongside
throughput.

## Benchmark integrity

Use existing infrastructure; prefer Criterion for suitable Rust microbenchmarks
when a supported execution path exists. Run benchmark suites sequentially.
Coordinate other builds and record server contention; contended results are
provisional.

Keep workloads, assertions, toolchain, build profile, CPU settings, and compiler
flags equivalent between baseline and candidate. Use release builds for timing.
Never reduce work, skip checks, change semantics, or alter settings to claim a
speedup. Explain benchmark corrections and apply them equally to both versions.
Distinguish cold and warm caches; reset independent cases and measure intentional
cache reuse separately.

## Broker-scoped verification and acceptance

Use one isolated broker task and its current digest for source changes, following
the standard Codebase Admin file tools. Do not experiment in the live checkout,
frozen releases, or inspection databases.

Bind evidence to exact task inputs and environment. Capture relevant inputs,
including untracked files, and confirm they remain unchanged after checks.
Results that raced edits are stale. Preserve baseline evidence across candidates.

build_work check/test provide locked Cargo check and test in the pinned image;
they do not provide arbitrary benchmarks, rustfmt, Clippy, or profiling.
Confirm a permitted execution path against exact isolated inputs before additional
verification. Shell and Python are not automatically task-scoped. If unavailable,
report the gap; do not substitute live-checkout measurements or claim success.

Check coherent changes, then verify settled inputs. Add regression tests for
changed behavior and benchmark evidence for performance claims. Test outcomes
and invariants rather than mirroring implementation. Do not weaken assertions
or suppress valid diagnostics to obtain a pass.

Revert unsuccessful experiments only within the task using fresh digests.
Never reset shared source or overwrite another writer.

Accept repeatable gains only when they justify complexity, dependencies, and
maintenance cost. Stop at the agreed budget or when remaining gains are noise
or disproportionate to complexity. Report changes, checks, measurements,
tradeoffs, unsuccessful approaches, and limitations. An infeasible target is
a valid finding; never weaken correctness to meet it.

Testing does not authorize deployment. Follow standard review, preflight,
synchronization, and authorized finish. Inspect receipts and the existing task/job
after uncertain effects; never duplicate an uncertain write or deployment.

## Code standard

The applicable subset of the Google Rust style guide is the v3 standard. An
extraction brings its whole module to it; any other Rust edit brings the files it
touches. Never widen reservations for cleanup.

- `rustfmt` with `max_width = 100` on touched files; no new Clippy warnings.
- `// SAFETY:` on every `unsafe` block saying why safe code cannot do it; prefer
  `libc` over hand-declared `extern "C"`.
- Never read through a poisoned mutex; caches clear the poison and rebuild.
- No `.unwrap()` outside tests: pattern-match, `?` or `expect("why it holds")`.
  Never check-then-unwrap.
- No glob imports except `use super::*` in inline test modules.
- `Option` for obvious failures, `Result` when the caller needs the cause,
  `panic!` only for broken invariants. No `Result<_, String>`; typed errors at
  contract-crate boundaries; `anyhow` is fine inside the app.
- Take `&str`, `&[T]`, `Option<&T>`, `&T` rather than references to owning
  containers. Borrow before `.clone()`.
- Newtypes where values are easy to mix up, enums rather than unclear `bool`
  parameters, named fields rather than tuples.
- Not adopted: `#[non_exhaustive]` on workspace-internal enums.

## Toolchain and language lessons

- Pinned Rust 1.90 and `Cargo.lock`; Cargo with `--locked`. Broker recipe:
  `CARGO_INCREMENTAL=0`, `RUSTFLAGS="-C debuginfo=0 -C link-arg=-fuse-ld=mold"`,
  Mold 1.10.1. Confirm the linker from the ELF `.comment`.
- After extracting or moving code, run the new package's production check
  first: trait imports, `cfg(test)` fixtures and feature-gated paths break at the
  new boundary.
- Run default and live compile-check profiles for shared host code; a package
  passing alone does not prove the linked build.
- Do not mask async `Send` inference problems with `unsafe impl Send` or extra
  boxing; reuse an existing opaque Send-future identity.
- Parse untrusted JSON straight into its typed owner from the original bytes;
  going through `serde_json::Value` loses duplicate-field rejection.
- Validate generated `Cargo.lock`/TOML structurally before applying it, then
  prove it with pinned `cargo --locked`.
