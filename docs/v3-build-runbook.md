# V3 Build runbook (operator material)

Host- and broker-side material moved out of the V3 Build skill (`plugins/abilities/Administrator/v3_build/v3_build.skill.md`) on 2026-10-08 (guidance survey F5/F8). The skill keeps the agent workflow; this file is for operators and for tasks that explicitly call for host or broker work.

## Storage checks and artifact prune

Check development storage after each terminal build, including a successful UI release. From the server use `df -h /`, `docker system df`, and `python3 -B /home/ar/webagent-dev2/scripts/dev_artifact_prune.py --dry-run`. After a healthy completed release, run `sudo systemctl start webagent-dev-artifact-prune.service` and check `df -h /` again. This service also runs when build receipts change and about every ten minutes. It clears completed or superseded development rollback tars, old stopped V3 containers and images, finished check caches, aged V2 temporary builds, unused development volumes, Docker build cache above 50 GiB root use, and protected root-owned rollback copies after one hour. It retains the current mounted release, open check tasks, active compiler output, application state, workspaces, and unresolved newer job receipts. If root use stays above 50 GiB, inspect the measured development directories and report the remaining owners; do not run a host-wide prune or operate on production containers.

Caution: the prune command above cites a path under `/home/ar/webagent-dev2/` — the V2 checkout — carried over from earlier guidance. Verify the script path against the dev3 checkout before running it.

The dry run cannot see the root-owned protected copies. Include `sudo du -sh /home/ar/webagent-dev3/rollback/administrator` in the storage check, and inspect the service result with `sudo systemctl status webagent-dev-artifact-prune.service` if cleanup fails.

## Broker internals

Compatible queued full-build tasks from one caller may be merged before compilation, up to eight tasks per image build. Each retains its own durable job ID and receives the shared tested-build result. A started image build stays frozen. If a UI release finishes during it, the broker checks that non-UI inputs are identical, then rebases its candidate onto the new UI snapshot before deployment. A same-file conflict fails the conflicting task; use a new task from the current release to revise it. A task ID is single use.

The API serves UI assets and seeds new coding workspaces from a broker-owned, versioned source snapshot behind a stable read-only mount. The broker atomically selects the complete snapshot and verifies the served UI. Runtime code, config and abilities stay pinned to their tested container release. Edits in another task workspace or the writable File Manager checkout cannot change mounted release assets. Only the broker chooses development targets; production is never a target. An uncertain external effect blocks subsequent releases until reconciliation.

After both replacements are healthy, the broker promotes changed release files
into the shared checkout if those paths still match the prior or new release.
A conflicting direct checkout edit holds subsequent releases for reconciliation.

Codex uses the same broker through server-side `buildctl.py`: `begin` defaults to an isolated workspace, `inputs --task ID` reads its digest, `finish ID --expected DIGEST` queues one release, and `wait ID` polls without creating a job. The V2 Python bridge uses the same task registry and its own authenticated caller identity. Legacy shared-checkout work must not overlap isolated tasks.
