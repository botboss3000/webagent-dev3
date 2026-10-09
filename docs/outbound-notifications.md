# Dev 3 outbound notifications — Phase 1

Owner-reviewed Phase 0 contract, revised 2026-10-09. Development only.
Widget engagement is excluded. Phase 2 supplies webhook subscriptions,
dispatcher, signing, delivery receipts and Identity UI. Native channels and
the assistant-side monitor are later work.

## Event contract

The canonical application-owned notification_outbox has an increasing global
seq, stable event_id, type, session_id, optional run_id, agent_id, actor,
payload JSON, and created_at (server epoch seconds). Internal source_user and
source_seq identify the transactionally staged source. schema_version is 1.
Payloads are plaintext metadata: schema version and a fixed lifecycle status.
No message text, tool names/arguments/results, approval contents, URLs,
credentials, model outputs, raw errors or user-entered strings are exported.
IDs and metadata are administrator-only operational data; file permissions and
HTTP authorization protect them. No payload encryption is required.
Credential records remain sealed and token values are never stored or logged.

Supported events:
- session.created: committed API session creation, once per session.
- message.received: committed user chat insertion, including manager submissions,
  once per incoming message. Receipt retries and manager acceptance do not repeat it.
- run.started: first successful execution claim, once per run; resumes do not repeat it.
- run.completed: committed final publication, once per run.
- run.failed: committed terminal failure, once per run.
- run.waiting_approval: committed wait, once per distinct approval checkpoint.

Source transactions add notification_stage metadata and a notification_emitted
deduplication marker. A failed transaction emits nothing. The API-owned relay
uses Store methods to read bounded staging batches for known users. Canonical
inserts and the source watermark commit in one application transaction before
source acknowledgement. The watermark survives retention, preventing a crash
retry from resurrecting pruned events. Source deduplication markers are retained
with the source resource lifetime. Staging is capped at 10,000 rows per user;
capacity exhaustion fails the originating transaction rather than silently
dropping its event. Global ordering is relay order, not cross-user execution
order; created_at preserves the source timestamp. There is no historical
reconstruction from existing conversations.

## Retention and backfill

The outbox retains history for cursor backfill for up to 30 days from relay
ingestion, capped at 1,000,000 rows installation-wide. It is immutable except
for automatic prefix pruning. Overflow removes oldest sequences, potentially
shortening backfill below 30 days. The durable pruned_through watermark never
decreases. SQLite reuses freed pages; this bounds retained rows but does not
shrink an already larger database file. No automatic VACUUM or session
database filesystem operation is introduced.

An expired cursor gets HTTP 410 with error=cursor_expired, pruned_through,
earliest_seq, latest_seq and resume_since. Clients must acknowledge the gap
explicitly before resuming at resume_since; do not silently skip history.
A connected SSE stream that falls behind emits cursor_expired and closes.
The dispatcher in Phase 2 must reconcile retention with pending deliveries;
a slow consumer cannot claim unlimited historical retention.

## HTTP API

Paths use the existing /api/v3 API prefix. The two read routes also have
/admin/events and /admin/events/stream aliases with identical authentication.
- GET /api/v3/admin/events?since=0&limit=200&wait=30
- GET /api/v3/admin/events/stream?since=0&limit=200

since is a nonnegative signed 64-bit cursor; limit is 1–200; wait is 0–30
seconds. Unknown parameters and malformed bounds fail validation.
Long-poll returns events in ascending sequence with next_seq, latest_seq,
earliest_seq, pruned_through, has_more and retention_seconds. An empty timeout
preserves the cursor. Reading never consumes or acknowledges shared events.
SSE sends event type, sequence id and the same event envelope; Last-Event-ID
resumes from the greater of the supplied cursor and header. Heartbeats are
15 seconds; streams reconnect after at most one hour. Server shutdown closes
pollers and streams. Reconnect with the last fully handled sequence.

Browser access requires a current installation administrator cookie. Machine
access uses a separate events:read bearer credential, bound to its issuing
administrator and account generation, with an explicit expiration. This
credential cannot read conversations, submit messages, mutate configuration,
issue another token or approve tools. Role loss, account revocation, generation
changes, expiry and explicit token deletion stop access. Waiters and streams
recheck authorization while connected.

Browser-admin credential management (existing CSRF rules apply):
- POST /api/v3/admin/event-readers with expires_in (60 seconds–90 days):
  return a random token once, ID, expiry and events:read scope.
- GET /api/v3/admin/event-readers: redacted credential metadata only.
- DELETE /api/v3/admin/event-readers/{id}: idempotent revocation.

At most 64 issued reader records are retained; revoke unused records to free
capacity. Each credential has at most four combined event-feed connections;
each account at most eight and this installation's single API process at most
64. Excess requests receive HTTP 429. Disconnect, cancellation and stream drop
release capacity. Multiple API replicas would require shared admission before
these installation-wide bounds could be claimed.

## Storage ownership and rollback

notification_outbox, notification_relay, notification_retention and
notification_reader: belong to the application plane. notification_stage and
notification_emitted belong to the originating user plane. The source session
supplies the immutable agent binding. Cross-plane replication exports only the
fixed safe projection. Version-1 schema creation is additive and idempotent;
no existing encrypted event/chat/run tables are replaced or backfilled.
Existing private events and chat SSE retain their original contracts.

The protected broker captures source, runs locked tests, builds and deploys
the candidate. Preserve stopped rollback containers and required pre-migration
backups. Rollback leaves additive notification tables intact; an older binary
does not emit notifications while running. Never use older binaries' absence
of emission to claim continuous notification coverage. No live session database
is opened, queried, copied or decrypted outside authenticated Store/API paths.

## Acceptance

Disposable Rust tests cover transactional rollback, lifecycle emission,
duplicate receipts/publication, crash acknowledgement, retention and reopen,
paging/backfill, long-poll wakeup, SSE framing/reconnect, authorization,
read-only bearer scope/revocation and connection release. The full pinned
suite and broker image build must pass on unchanged inputs. Live acceptance
uses disposable development sessions, verifies persisted final replies and the
matching events, tests long-poll and SSE, rejects unauthenticated reads and
confirms connection caps and reader revocation through authenticated APIs.

Automation event_subscribe and delivery-channel tools remain unavailable
until Phase 2 has subscriptions and delivery. Do not mark the whole Automation
ability ready because this read/emission foundation exists. Event Runtime's
outbound foundation is partial; inbound event sources and triggering agents
remain outside Phase 1.
