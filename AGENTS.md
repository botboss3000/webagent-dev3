# V3 development checkout

This independent checkout is `/home/ar/webagent-dev3` on the development server.
V2 is `/home/ar/webagent-dev2`. Production is outside the scope of this checkout.
Use SSH to `ar@100.112.23.15` for source, container and repository operations.

Start with `git status --short --branch` and inspect existing diffs. Preserve
other writers' work. Do not create branches or worktrees without explicit user
authorization. The main branch contains extracted V3 history; configure a
separate V3 Git remote before attempting any push. Never point this repository's
main branch at the V2 remote for a push.

Read `README.md`, `docs/root-migration.md` and the relevant implementation before
changes. For workflow/storage changes read `docs/workflow-foundation.md`.
Development troubleshooting may inspect session databases directly. Isolated
broker tasks receive read-only SQLite-backup snapshots of all admin/ and
shared_default/ data, including their database files. Other users, the encryption
key and unrelated runtime databases remain outside these inspection snapshots.
Never patch task inspection data.

In-app agents use Codebase Admin file tools with one stable `task_id` for
source changes intended for a build; a missing task is created automatically.
Read the task before editing, and use its latest input digest for task writes.
Use file tools without `task_id` for explicit direct/live edits and runtime
configuration. `build_work` reviews/checks tasks and explicitly finishes an
authorized release; task creation and edits never deploy.

Operators use the same protected broker through
`python3 -B /home/ar/webagent-dev3/plugins/abilities/Administrator/v3_build/buildctl.py`.
Begin an isolated task before source changes. Its snapshot contains stable
virtual `v3/` and `ui/` archive paths; the live checkout root contains V3 directly.
Apply standard unified diffs through `git apply --check` and then the exact same
patch with `git apply`, anchored at this checkout with the task directory prefix.
Do not edit frozen release snapshots. Inspect the diff and run `git diff --check`.
Capture all source inputs before and after tests/builds; coordinate writers and
Cargo output. A check that raced edits is stale. Finish only when development
deployment is authorized; it queues one tested release. Inspect and resume the
same durable job after interruptions; never duplicate an uncertain deployment.

Runtime containers are `webagent-dev-v3`, `webagent-dev-v3-loop` and
`webagent-dev-v3-gate`; preserve their separation and stopped rollback snapshots.
The API listens at `100.112.23.15:8081`; the public app is
`https://dev.webagent.live/app`. Explain the interruption before replacement.
The writable `/live-checkout` is only this V3 root. Shared V2 UI assets are fixed
read-only build inputs. Never restore a writable V2 mount to a V3 container.

For UI work read the Codebase Admin and Diagnostics skills in
`plugins/abilities/Administrator`, plus `docs/ui-journeys.md`; navigate directly
to registered routes. Check Code Index availability and report unavailable
coverage rather than claiming it passed. Keep comments at non-obvious authority,
worker, recovery and publication boundaries; update `docs/code-tour.md` when the
execution route changes.
