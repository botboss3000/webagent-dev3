---
id: engineering_lessons
title: Engineering lessons and release gates
when_to_use: Use when planning, implementing, verifying, releasing or recovering WebAgent v3 work through the broker.
default_mode: discoverable
examples: plan a v3 change; recover a failed release; check gates before finishing a broker task
keywords: broker, release, lessons, gates, recovery
minimum_access: registered
---

# Engineering lessons and release gates

The always-apply rules distilled from past incidents. Lesson IDs (BP-NNN) refer
to the operator's incident log. This skill adds no approval requirement; the
user's instructions, the checkout `AGENTS.md` and the broker's live access policy
win where they differ.

## Before you start

1. Check `git status --short --branch` on the checkout and inspect relevant
   diffs. Preserve other writers' changes and all runtime data.
2. Identify the exact target, the current container/image identity and whether
   the operation interrupts the live development app; explain any interruption
   first.
3. Confirm the release path (Rust source, worker, ability/plugin, UI asset,
   configuration, host-only) with the live_plugin_publication skill. Do not
   infer hot-update or compile requirements from file extensions.

## Implementation

- Use the broker's isolated task workflow and checked unified diffs: `git apply
  --check`, apply without staging, then `git diff --check`, `git diff`, `git diff
  --stat` and the relevant tests. No shell redirection, ad-hoc rewrite scripts,
  `--reject` or `--3way` unless the user asks for conflict recovery.
- Patch paths are V3-relative; a checked patch changes at most 64 paths (split
  and bind larger work, BP-034). New files need a `new file mode` header.
- Keep the pinned Rust toolchain and `Cargo.lock`; coordinate source writers and
  the shared `target` directory before Cargo work.
- Runtime code and UI use frozen release mounts: editing the checkout is not
  live until a supported publication.
- Verify runtime code modes and readability as the actual worker UID before
  activation; keep private source modes and never fix them with runtime `chmod`.

## Admission (before expensive work)

- Freeze the candidate: capture every input (including relevant untracked
  files) and confirm it is unchanged afterward. A build that raced an edit is
  stale even if it passed (BP-003).
- Prove test fixtures early with a thin real-boundary slice: actual image,
  parser, identity, socket, policy and response shape, not mocks alone (BP-033).
- Verify pinned image digests exist before every build; pruning removes them.
- Check disk before reserving: about 14 GiB free to compile, 16 GiB to package.
  Cancel an unstarted check and yield claims rather than holding writers.
- For shared host code, run default and live compile-check profiles on the same
  unchanged digest before release compilation.
- Settle state transitions (rename/deletion, resume, retry, concurrent
  ownership, rebase, publication, restart, rollback) before the full build
  (BP-001).

## Release, commit and push

- Reconcile the exact effects of a failure before any retry. Never retry an
  uncertain external write or rewrite a failed receipt as success.
- A passing compile/test check is not a release. Before committing, verify a
  healthy deployment, readiness 200 and completed source promotion. Then commit
  and, with the heartbeat running, send one `release_claims` with every held
  scope and the commit; it completes the task. Acceptance checks run afterwards
  without claims.
- Commit only the release's exact bytes and paths, bound to the release/job ID.
  No `git add -A`, runtime data, credentials or another writer's edits; no empty
  or duplicate commits.
- Push is a separate action needing user authorization. Scan the whole outgoing
  history with the installed credential guard first, bound to its exact HEAD and
  remote baseline; report only commit/path/line metadata, never matched bytes.
- Synthetic credential-shaped fixtures keep their value but avoid the literal
  shape (Rust `concat!` or the language's equivalent). Moving a test reintroduces
  its literals in the new patch. Never weaken detection or remove rejection tests.

## Ownership, claims and recovery

- Reserve only the demonstrated edit scope; narrow unused claims before freezing.
- Start the ownership heartbeat at the first apply and keep it through review,
  compilation, corrections and release (BP-036, BP-041). If an apply is blocked
  by another task, start the heartbeat anyway (BP-048).
- Use supported recovery operations (`recover`, `reconcile_acquisition`,
  `takeover_preview`) with the current revision/epoch. Never edit journals,
  receipts, ownership storage or policy, or bypass broker checks.
- A failed or cancelled release before any deployment effect is fixed on the same
  task (reopen, edit, apply, finish); abandon only when the work is dropped.
- Never pipe a step whose exit status gates the next; test it first (BP-050).

## Verification and evidence

- Run tests in the pinned image or deployed container, not the host. Run
  socket-identity checks in the deployed container as the real UIDs.
- Distinguish fixture failures (readiness, timing, stale stubs, hidden controls)
  from product failures; fix the fixture or mechanism, never force hidden
  controls.
- Report only what evidence proves: absent counters mean unavailable, not zero;
  contended runs are not benchmarks. Keep deployment, healthy runtime, tool
  journey and current guidance as separate outcomes with their own evidence.
- Recursive chown/chmod/setfacl/delete: list every owner and consumer under the
  path first (BP-051). The checkout uses POSIX ACLs: `setfacl -m g::rwx`, not
  `chmod g+w` (BP-052).

## Common failure → prevention

| Failure | Prevention |
| --- | --- |
| Registration and dispatch reviewed separately | Review a route end to end (BP-013) |
| Tested hot guidance without checking the live loop | Verify the loop actually adopted it (BP-014) |
| Assumed a build slice enables its controllers | Verify activation separately (BP-031) |
| Assumed filesystem confinement fences sockets | Probe the actual socket (BP-032) |
| Sync decision sent with nothing to synchronize | Send it only when preflight asks (BP-040) |
| Broker install reverted a concurrent broker release | Confirm task base equals checkout HEAD before installing (BP-042) |
| Readiness change broke a hard-coded catalog count | Run the root binary's `abilities::` tests before finish (BP-043) |
| rustfmt reformatted untouched files | Format only the task's files; check the patch file list (BP-044) |
| Ported ability still denied after release | Check per-tool `effective.reason`; new tools start `deny`; manifest access needs a restart (BP-046) |
| Live check fails on items the task added | Touch the changed `.rs` files and rerun; stale shared-target metadata (BP-047) |
| User-facing route works for admin only | Register it in the plain `api` router, test with a non-admin caller (BP-049) |
| Runtime preflight fails after a UID change | Scan the release as the new identity; set modes explicitly (BP-053) |

## Closure

There is no Code Index, documentation-maintenance or review requirement: do
those only when the user asks for them. Write the comments a change needs and
fix documentation the change makes wrong in the same patch.
