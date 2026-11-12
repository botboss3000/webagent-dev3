---
id: broker_monitoring
title: Broker monitoring and build performance
when_to_use: Use when watching a broker job, telling a stuck build from a slow one, or measuring and improving broker build performance.
default_mode: discoverable
examples: is this build stuck; why is the release slow; prove a build-service speedup
keywords: broker, monitor, build, performance, stalled
minimum_access: registered
---

# Broker monitoring and build performance

The canonical release and monitoring procedure is "Agent guide: releasing and
monitoring" in `build-service/README.md`; `buildctl monitor <job>` is the
monitor. This skill covers judgement: stuck vs slow, and proving a gain.

## Stuck, starved, slow or avoidable

| Question | Signal | Threshold |
| --- | --- | --- |
| Stuck? | build CPU accumulating, receipt log growing, phase advancing | no progress for 15 min |
| Starved? | compiler runnable but under 5 % CPU; PSI cpu/io/memory | PSI over 10 % |
| Slow? | stage elapsed vs its class p50–p90 | past p95, then check-ins |
| Avoidable? | stage reproduces a known failure class | open the log |

Never call a build slow from wall time alone: compare against the class range
(`buildctl progress <job>`) and check whether CPU was accumulating. Classes
reflect work done (compile vs reused artifacts, tests run vs reused); a stage
with fewer than 5 jobs shows `n=` and gets only the no-progress check.

## Pitfalls that have cost time

1. A failure reason is not a cause: `fixed command failed: <output>` starts with
   whatever the compiler printed first, often a warning. Open the log.
2. Label an interval with the phase entered at its start, or baselines shift.
3. RSS deltas across a build prove nothing; sample one stable PID.
4. `/proc/<pid>/io` resets when a container is recreated; `/proc/<pid>/status`
   has no command line (match `cmdline` first).
5. `orphaned` does not mean failed: check the receipt and `git status`.
6. A slow start is often queue wait, not build time.
7. A stale claim frees its scopes after missing heartbeats; never start work on
   paths a deployed-but-uncommitted release touches.
8. Ownership claims do not queue: a blocked apply needs a retry once the
   blocker releases. Never release another writer's live claim.

## Where to implement

| Concern | File |
| --- | --- |
| phase definition and cost wrapping | `build-service/executor.py` (`self.phase(...)`) |
| test stage, linked/live equivalence | `build-service/executor.py` |
| packaging, rollback capture, publication | `build-service/executor.py` |
| lane selection, failure reason capture | `build-service/host.py` |
| queue, orphans, leases | `build-service/scheduler.py`, `ownership.py` |

## Rules

- Artifact and test reuse already work; do not optimise what is cached.
- The linked/live recipe pair is a deliberate equivalence gate; reducing it is
  a product decision for the user.
- Do not raise concurrency to improve a number; oversubscription turns CPU-bound
  work into swap-bound work.
- An improvement counts only with a class-matched before/after (same compile and
  test reuse), deployed and healthy, test counts equal, inputs unchanged and the
  commit bound to the release.
