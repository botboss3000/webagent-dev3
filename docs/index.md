# WebAgent v3 documentation index

Authoritative map of this checkout's documents. **Edit a doc, update its row
here (status and last-updated) in the same patch.** Git history remains the
detail log; this index is the table of contents and staleness surface.

Deleted 2026-11-24 (v2-era or superseded): `ui-theming-design-history.md`,
`ui-toggle-list-design-history.md`, `app-guidance.md`,
`tenant-models-acceptance-2026-10-02.md`, `ability-port-ledger.md`
(replaced by `ability-migration.md`).

| Document | What it covers | Status | Last updated |
| --- | --- | --- | --- |
| [README.md](README.md) | Entry, run/deploy, API orientation | living | 2026-11-24 |
| [AGENTS.md](AGENTS.md) | Checkout workflow rules | living | 2026-11-24 |
| [docs/workflow-foundation.md](workflow-foundation.md) | Record ownership, workflow checks, storage | living | 2026-11-24 |
| [docs/context-assembly.md](context-assembly.md) | Model-call context assembly, compaction, dedup | living | 2026-11-24 |
| [docs/capacity.md](capacity.md) | Server capacity limits and how to choose values | living (maintained by codebase_admin skill) | 2026-11-25 |
| [docs/security-spec.md](security-spec.md) | Security requirements with IDs, threat model, accepted risks | living (cite requirement IDs) | 2026-11-25 |
| [docs/tool-policy.md](tool-policy.md) | Tool decisions, saved approvals, gates, history retention | living | 2026-11-24 |
| [docs/recovery-policy.md](recovery-policy.md) | Failure classes, retries, bridge/approval timers | living | 2026-11-24 |
| [docs/build-protocol.md](build-protocol.md) | Unified plan gates every new build is checked against | living gate | 2026-11-23 |
| [docs/session-messaging.md](session-messaging.md) | Native cross-session messaging (steer/now) | living | 2026-11-24 |
| [docs/session-creation.md](session-creation.md) | Session-creation contract and status codes | contract | 2026-11-20 |
| [docs/direct-messages.md](direct-messages.md) | Direct-message contract (limits, 409 reuse) | contract | 2026-10-18 |
| [docs/manager.md](manager.md) | Manager review states, claims, scheduler | product | 2026-10-30 |
| [docs/orchestration-api.md](orchestration-api.md) | Admin/orchestration API routes and auth | API reference | 2026-11-24 |
| [docs/ui-journeys.md](ui-journeys.md) | UI routes and acceptance journeys | living (route registry) | 2026-11-24 |
| [docs/ui-pages.md](ui-pages.md) | Page/descriptor authoring contract | contract | 2026-11-22 |
| [docs/chat-widget.md](chat-widget.md) | Website chat widget config and loader | spec | 2026-11-23 |
| [docs/desktop-app.md](desktop-app.md) | Desktop app packaging and bridges | spec | 2026-11-20 |
| [docs/feedback.md](feedback.md) | In-app feedback relay | spec | 2026-11-23 |
| [docs/drop-in-abilities.md](drop-in-abilities.md) | Plugin/ability package contract, publication lane | contract | 2026-11-23 |
| [docs/ability-migration.md](ability-migration.md) | v2→v3 ability migration inventory | inventory | 2026-11-21 |
| [docs/engine-modularization.md](engine-modularization.md) | Engine interfaces, boot/background-service extraction ledger | ledger | 2026-11-20 |
| [docs/boot-supervisor.md](boot-supervisor.md) | Boot supervisor contract and sandbox | design | 2026-10-18 |
| [docs/controlled-build-scheduler.md](controlled-build-scheduler.md) | Build scheduler actions and policy | runbook | 2026-11-01 |
| [docs/parallel-release.md](parallel-release.md) | Parallel release/task isolation rules | runbook | 2026-11-11 |
| [docs/v3-build-runbook.md](v3-build-runbook.md) | Host-side build/prune/opt-in procedures | runbook | 2026-11-11 |
| [docs/restore-development.md](restore-development.md) | Recreate the dev installation from Git | runbook | 2026-11-24 |
| [docs/ssd-restore.md](ssd-restore.md) | SSD restoration snapshot guide | historical runbook | 2026-11-20 |
| [docs/root-migration.md](root-migration.md) | v3/ + ui/ virtual archive-prefix rule | historical rule | 2026-11-06 |
| [docs/diagnostics-package.md](diagnostics-package.md) | Diagnostics native package model | product | 2026-11-08 |
| [docs/diagnostics-background-issues.md](diagnostics-background-issues.md) | Background-issue capture design (plus function inventory) | design | 2026-11-24 |
| [docs/diagnostics-parity.md](diagnostics-parity.md) | Diagnostics parity roadmap | roadmap | 2026-10-18 |
| [docs/outbound-notifications.md](outbound-notifications.md) | Outbound notifications contract | contract | 2026-10-18 |
| [docs/main-chat-main-notes.md](main-chat-main-notes.md) | Main chat / main notes settings | spec | 2026-11-20 |
| [docs/removable-modules.md](removable-modules.md) | Removable module lifecycle | product | 2026-10-18 |
| [docs/code-tour.md](code-tour.md) | Plain-language architecture tour | living | 2026-11-13 |
| [build-service/README.md](../build-service/README.md) | Broker operator guide (begin…close, lanes, exits) | living (release authority) | 2026-11-24 |
