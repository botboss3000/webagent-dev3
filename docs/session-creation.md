# Session creation and retry recovery

POST /api/v3/sessions uses agent_id or shared_default when omitted. Reuse the
same client_request_id and exact body after interruption. A changed body under
that ID is HTTP 409; a new ID may intentionally create another session. Tenant,
agent admission and session limits remain host checks; callers never choose the
storage owner.

## Persistence and function connections

create_session locks the host gate and checks the owner-scoped receipt.
prepare_session/prepare_session_id validate agent access, title, limits and
initial participation/execution mode. Store::atomic_docs_event/atomic_admission
commit session, session-mode, request receipt and session_created event together
in the owner's encrypted user plane. Store::register_shared_session then projects
the immutable owner/agent into the application directory. These are separate
commits across distinct authorities, not one cross-database transaction.

finish_session_creation runs after new creation and every valid receipt replay.
It uses the saved session/agent IDs and authenticated store owner. Registration
inserts idempotently and shared_directory verifies correspondence; conflicting
owner/agent rows are never overwritten. Recovery returns the same session and
original creation time/title without another creation event. Shared-chat resolve
and API user wrappers still separate actor from storage owner and enforce access.

| Outcome | API behavior |
| --- | --- |
| Registration complete, including identical replay | HTTP 200 with the same saved result. |
| Request body changed under existing ID | HTTP 409 before finalization. |
| SQLite busy/locked registration | One retry after 20 ms; each operation retains its SQLite timeout. |
| Registration still fails after owner commit | HTTP 503 explains that the session is saved and requires the same client_request_id. |
| Conflicting directory ownership | Rejected without rewriting ownership or falsely reporting success. |

Warnings contain only the fixed operation and sqlite_busy/sqlite_database/storage
classification, never SQL text or session content; the content-free backend-error
counter advances. The finalization rule is an invariant, not a configurable bypass.
Named/default modes and initial participation remain agent-owned settings.
Config: data/config/chat.json -> /chat_common/execution_mode/default_mode and
/chat_common/execution_mode/confirm_auto_mode affect initial mode/confirmation;
they never authorize ownership changes. Recovery resets no Identity or connection.
Persistence and authority remain host-owned; no supporting pure planner changed,
so this fix adds no engine service, logical ID or history migration.

## Evidence and limits

The original HTTP 500 at 2026-10-19T14:11:03.415610Z occurred after the owner commit:
replay returned original session f3eaa303-1a03-41f9-8642-c4b82152a508, created_at
1792419063. The precise database error was not retained. Do not claim corrupt
agent settings or a proven lock cause. Fresh creation later passed both normally
and during context-component withdrawal.

Release 59ae0ffd-20b9-4adf-b870-5171d7d773ee, commit
ee812b6f6c8935bee1ccd750e73fc5ceb6993b18, changes only src/api/sessions.rs.
It passed 989 workspace tests, zero failures, five existing ignored tests, both
compile profiles and optimized linked/live checks. Failure injection verifies
repair after the owner commit, one session/event and preserved conflicting
directory authority. Evidence in build-jobs/checks/shared-default-session-create-20261019/:
investigation.json; focused-current.json; test-proof.json; live-acceptance.json;
release-commit.json; closure.json. Live acceptance covers original-request recovery,
implicit/explicit agent selection, identical/conflicting replay, creation during
withdrawal and unchanged Identity. Implementation is closed with zero claims.

Only provenance-bound empty test sessions were recycled; the original failed
fixture and other conversations remain. A cleanup helper initially selected an
Identity GET record as a session ID and stopped before modifying that record.
cleanup-correction.json records filtering by operation; accepted-before-cleanup.json
separates acceptance from cleanup. That correction changed no application source.
