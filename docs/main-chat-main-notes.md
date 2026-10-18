# Main Chat and Main Notes

Main Chat is the permanent private coordination conversation. Main Notes is a
user-scoped cross-session document stored independently of transcript compaction.
Both names are editable on Identity. The canonical session ID and protected
session metadata establish identity; a display name never does.

## Settings and permissions

GET/PUT `/api/v3/main-chat` reads or changes account-wide `enabled`, `chat_name`,
`notes_name`, `context_limit_bytes`, `write_mode` and `reporting`. Defaults are
false, Main Chat, Main Notes, 262144 bytes, new_topics and meaningful. Names are
bounded to 200 bytes; context accepts 1024 through 4194304 bytes and remains
subject to the existing workflow context ceiling. There is no clearing action.
Disabling retains the session and document; ordinary Compact remains available.

Settings live in encrypted `module_state:main_chat`, not a JSON config file.
Automation has separate effective settings/ceilings, declared in
`plugins/abilities/Core/automation/automation.json` under `config.settings`.
Installation, tier, agent, tool, approval and execution permissions still apply.
A successful Notes write or approval authorizes neither execution nor scheduling.

Write modes are `every_write` (agent contributions become proposals), `new_topics`
(new topics and changed approved scope need approval), and `automatic`. Human
contributions commit directly and approve their supplied topic scope. Proposed
changes are approved/rejected through the dedicated Notes UI, with topic revision
checks preventing approval of stale content. Temporary plans and minor changes
remain session-local; contribute ongoing goals, decisions, blockers and actions.

## Durable data and API contracts

GET `/api/v3/main-notes` reads private topics, entries and proposals. POST to that
route contributes a complete topic scope and entry; POST
`/api/v3/main-notes/proposals/{id}` decides an exact pending proposal. Mutations
carry `expected_revision` and a stable `request_id`; settings renames also carry
`expected_session_revision`. Retrying the same ID with different content fails.
Browser mutations use existing cookie authentication and CSRF checks.

`src/storage/main_notes.rs::main_chat_change` reserves the SQLite writer before
reading, compares revisions/fingerprints, and commits encrypted notes, chat
initialization/rename documents and committed-change notifications together.
State is bounded to 3 MiB, 256 topics, 256 entries per topic, 256 proposals and
4096 receipts/change records. Capacity failures preserve existing data.

Topics retain approved scope, optional groups and entries with action text, state,
optional subtopic, source references, requested timing/condition, progress and
outcome. IDs and matching normalized titles merge topics. Matching unfinished
intent/timing/conditions fold duplicate entries, preserving source evidence.
Canonical entry IDs derive stable note-owned Automation IDs. No timing is invented.

The native `main_notes_read` tool pages topic summaries or entries, at most 20 per
page. `main_notes_write` uses the same durable approval pipeline. Shared-chat
membership alone does not grant access to someone else's private Notes.

## Review and scheduling

Approved committed changes advance a Notes version and transactionally emit
`main_notes.updated`. Pending proposals do not advance that version. The fixed
`main-notes-review` subscription bootstraps through `src/modules.rs::tick_package`
when enabled and permitted. It persists across restarts without duplicate IDs.
The Automation reducer consumes changes with a durable cursor and deduplicated
version occurrence IDs. New evidence or intent still wakes review; Main Chat's
progress-only writes remain observable without triggering a review loop.

The review skill is
`plugins/abilities/Core/automation/skills/main_notes_review.skill.md`. It chooses
an already authorized action, a permitted schedule/reminder, an input/approval
request, or waiting. Main Chat owns note-bound scheduling. The host verifies its
stable ID, destination and current intent again before admission. A retimed or
terminal entry cannot reuse an obsolete schedule. Time-based work wakes through
the existing Automation scheduler; update events alone cannot wake a due action.

At original acceptance the account's Automation capability tier denied the
ability. The subscription/model/restart journeys were therefore not enabled or
claimed passed. Do not treat installation or a Notes approval as a tier override.

## Backend function reference

These contracts supplement the source's transaction, privacy and wake-boundary
comments. They describe implementation responsibilities rather than new authority.

| Function in src/main_chat.rs | Purpose and boundary |
| --- | --- |
| Settings::default / validate | Create opt-in defaults; reject invalid names, context bounds and modes. |
| empty | Construct the initial private aggregate without creating a conversation. |
| identifier | Bound durable IDs and receipt keys to portable characters. |
| Contribution::validate | Bound IDs, text, optional timing, references and states before any write. |
| target | Resolve an existing topic by ID or normalized exact title. |
| apply | Merge approved intent, preserve duplicate evidence, assign stable scheduling IDs and append a change version. |
| contribute | Apply Notes write-mode rules; either retain a proposal or commit the contribution. |
| action_intent | Hash the current scope, action, timing and condition for schedule admission. |
| scheduled_entry_current | Reject redirected, terminal, retimed or otherwise obsolete schedule bindings. |
| public | Strip private idempotency receipts from the API projection. |
| request_fields | Require the global revision and stable request identity on every mutation. |
| error | Map state/revision errors to the existing HTTP conflict response. |
| read_api | Project private state with the canonical session's current name and review status. |
| configure_api | Initialize once or rename with aggregate and session metadata fences; disabling retains identity. |
| write_api | Commit an authenticated human contribution without executing its action. |
| decide_api | Decide the exact pending proposal, rejecting stale topic revisions. |
| owns / schemas | Identify and describe bounded native Notes adapters; grant no tool permission. |
| tool | Enforce private caller identity, page reads and submit policy-gated agent contributions. |
| contribution (test helper) | Build a small actionable fixture with an explicit source reference. |
| modes_scope_merge_and_self_loop | Check approval scopes, duplicate merging and progress-only wake suppression. |
| new_evidence_from_main_requires_review_but_progress_does_not | Distinguish new evidence from quiet coordinator progress. |
| storage_receipts_isolation_events_and_restart | Exercise encrypted durability, exact retries, isolation, events and store reopening. |
| identity_names_stable_session_and_exact_proposals | Check persistent identity, naming and exact revision-fenced proposal decisions. |

In `src/storage/main_notes.rs`, `main_chat_state` loads the encrypted aggregate
or returns an initial value without writing. `main_chat_change` acquires the writer
reservation before reading, validates request fingerprints/revisions and commits
notes, initialization/rename documents and notifications atomically. Its receipt
retry returns the original result; stale revisions, capacity failures and reused
IDs with different content abort the transaction. Post-commit cache invalidation
does not make note content an execution authorization.

## Integration function reference

| Owner and function | Verified feature responsibility |
| --- | --- |
| ability_plugins::automation_contract | Accept the legacy native package or its exact Notes-tool/skill extension; keep native identity validation strict. |
| agent_loop/run::execute_run_inner | Apply the permanent chat context ceiling, load the review skill and contribute requester-specific policy only for the current model call. |
| tools/dispatch::call_raw_inner | Dispatch Notes through the native adapter after existing tool authorization. |
| tools::is_read / native_schemas | Classify the bounded read and register both native tool schemas. |
| api/auth::auth | Apply the existing chat-page permission boundary to Main Chat and Notes routes. |
| api/chat::events / opening_main_owner | Refresh the private widget without transcript messages or revealing Notes revisions to shared observers. |
| api/user::router | Register the account-scoped settings, contribution and proposal-decision endpoints. |
| app::policy_default | Give Notes read the existing read default and Notes write the independent approval default. |
| modules::tool_call | Restrict note-bound schedules to Main Chat and stamp current host-derived intent and destination. |
| modules::binding_valid / admit | Recheck enabled state and current schedule intent before admission or further tool effects. |
| modules::package_service | Retain wake generations received during an in-flight reducer; same-second writes are not lost. |
| modules::tick_package | Bootstrap one fixed review subscription and supply up to 128 ordered changes after its durable cursor. |
| storage/session::public_session | Expose role and deletion capability independently of the conversation title. |
| storage/session::lifecycle_session / purge_session | Reject recycle and purge of the permanent Main Chat. |
| storage/session::prepare_session_document | Preserve the immutable role across partial checkpoint and compaction writes. |
| storage/module_records::module_events | Resolve committed Notes notification versions back to their bounded change metadata. |
| storage/notifications::stage_notification | Stage the recognized Notes update kind in the caller's transaction. |
| automation/service/engine::dispatch / reduce / tick | Validate reducer context; bootstrap fixed identity; consume Notes versions and queue bounded deduplicated review occurrences under current settings. |

The integration modules also retain their pre-existing unrelated responsibilities.
The ordinary storage/module declarations wire the feature into the existing crate;
no additional runtime configuration or execution authority comes from a declaration.

## UI ownership

Identity's `renderMainChat` owns account preferences and exact uncertain-write
retries. The Notes element owns its private SSE connection, edit revision fences,
lossless untouched fields, proposal controls and teardown. The panel mounts it in
the measured header extension; embed disables it because embed authority is
session-scoped. The dropdown and Sessions page guard deletion independently of
visibility. Backend recycle and purge remain authoritative protections.

Automation management recognizes fixed review IDs and note-managed action IDs.
It retains Pause/Resume/History, hides generic Edit/Run now for managed intent,
and prevents fixed-subscription recycle/cancel/delete. Change intent or requested
timing in Main Notes. Individual tool policies remain on Abilities.

## Release evidence and maintenance

Originating chat: 01a1357b-0ff4-7201-99ba-c1aaffdbbab9. The backend release is
446bf9c3-5b10-42af-b516-e8b38ad4a9b9 (commit f973f745); Identity is
da1f5864-539c-46c3-8e73-df56b73c0c1f (277f9405); Notes UI is
77a53f63-1eb3-40c5-b8c0-1a6c9a93e44d (df36c17a); session protections are
e3ee88a7-2754-4d30-969d-db1edbe406c6 (0ba5edb3); final Automation UI is
456e5ed4-f5c4-45a7-be5a-4e0551ef5ec4 (9441c786).

Backend acceptance passed 859 tests with five ignored, four focused Main Notes
tests and default/live compile profiles. Separate frontend fixtures covered
responsive layout, exact retries, proposal rendering, lossless timing, revocation,
renamed session protections and teardown. Live served bytes and healthy retained
API/loop identities were checked. Interactive live-browser/model/restart journeys
under an allowed Automation tier remain separate from those fixture results.

Release-bound maintenance receipts and remaining-work records live under
`build-jobs/checks/main-chat-maintenance-20261018/`. A scan does not certify a human
review. Final maintenance status must come from the current source/index bindings
and named review records, not this prose. The maintained execution tour is the
control project's local docs/code-tour.md; do not update the historical server copy.
