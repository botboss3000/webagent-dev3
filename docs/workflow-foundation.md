# V3 workflow and storage decision (Phase 0)

Status: accepted for the in-process v3 foundation. Scope: the current administrator
and agent, with ownership rules that remain valid when more users and agents arrive.
This document defines contracts; it does not claim v2 feature parity.

## Tenant model-role records

<!-- TENANT-MODEL-OWNERSHIP: agent authority shares one atomic application catalog. -->
The encrypted `model_selection:catalog:v1` document adds `agent_models`,
keyed by agent ID. Each record binds the immutable directory owner, a model
roster, six role appointments with ordered backups, and an optimistic revision.
The application catalog is the atomic storage boundary; each agent owns its
logical roster and role configuration. Connection and entry owners alone
change sharing grants and credentials. API projections hide private tenant
records and preserve hidden records during a save.

Encrypted membership records add `agent_administrator`, limited to agent/model
administration. Owner, member, caller (visitor), and revoked keep their existing
tool-policy meaning. Membership writes check actor authority, target account,
directory ownership, and expected revision in one application transaction,
then rotate account access generation and quiesce affected work. Initial owner
creation advances the access revision but preserves sign-in generation; it grants
new ownership and must allow the same authenticated creator to configure models.

Legacy primary records remain for rollback. Reads project absent role maps to
Standard without appointing other roles; the next atomic save persists that
projection. `migration:model_roles_default_v1` separately imports a connected
default subscription once, retaining its credential in the secrets plane.
An explicit empty role map prevents resurrection. User-owned run documents pin
the chat plan and independent System plan and failover cursor. Role changes
affect new runs, while live revocation and account ceilings still stop dispatch.
No session database inspection or alternate state access path is introduced.


## Agent ability authority and migration

The application access-policy tiers add explicit `ability_permissions` keyed by
ability ID. Missing IDs deny; an explicit `*` opts in to future abilities.
Missing fields on legacy tier records project only `codebase_admin` and
`web_access` as Auto, preserving their old tool ceilings. This is a bounded
read migration; the next tier save persists the projection.

Agent metadata owns `capability_levels` (aggregate revision and keyed records)
and the base `ability_permissions` / `tool_permissions`. A membership in the
application plane references `capability_level_id`. Legacy memberships project
owner/delegated administrator to administrator, member to member, caller to
visitor. Built-in levels initially grant only the two legacy ready abilities;
missing tool settings inherit the existing ceilings. New custom levels have no
ability grants. Level names never confer administrative authority.

One live Rust access snapshot meets installation tool policy, account tier,
agent base policy and membership capability level before discovery, guidance,
schemas, approval and dispatch. Visibility cannot grant execution. Revisions
and resolved grants enter the context fingerprint; edits quiesce affected runs,
and recovery still rechecks live policy. Capability-level and base-policy CAS
updates commit encrypted agent-owned audit receipts with metadata in the agent
file. Membership assignment and its audit commit in the application file and
rotate the target account generation. No session database migration is used.

Agent owners, delegated administrators and explicit installation oversight use
tenant-scoped ability APIs after resolving directory authority. Nonmanagers
receive a sanitized effective view, without private policy maps or roster.
Prompt settings use the same authority. Installation tier edits remain separate.
Capability levels cannot be deleted while memberships may reference them.

| New record family | Owner/location | Migration/lifecycle |
| --- | --- | --- |
| Tier ability-ID grants | Installation access-policy configuration | Legacy field omission projects only ready adapters; CAS saves persist |
| Capability levels, base policies, encrypted ability audit receipts | Selected agent metadata and docs in agent data file | Defaults project on read; atomic revisioned writes persist |
| Membership capability-level reference | Application membership record | Legacy authority-based default; revisioned assignment invalidates sign-in |

## Fixed execution boundary

The Rust runtime owns admission, attachment handling, context assembly, model
dispatch and output, pre-tool authorization, tool-result handling, and final
publication, in that order. A model may return tools and repeat the context/model
portion of the loop. The Python tool worker executes only a dispatch authorized
by Rust. It cannot admit work, alter policy, approve itself, or publish chat.

Each checkpoint records a typed, versioned input digest and a Pass or Reject
decision in the private run document. The receipt binds the run generation,
policy revision, turn, and call index. A receipt is evidence of a decision,
not an authorization token. Tool dispatch rechecks live permission, agent
visibility, session mode, cancellation, and shutoff. An approval is bound to
the relevant revisions; a changed restriction invalidates it. Recovery may
resume a safe read, but an interrupted write remains uncertain until resolved.

The installed module set is empty. Module configuration names only fixed
checkpoints; unknown checkpoints and uninstalled modules fail validation.
A later module adapter must accept a versioned checkpoint input and return
a bounded typed contribution or decision. It cannot skip or reorder checkpoints,
mutate another plane, lower a native denial, or publish directly. Module errors
must have an explicit fail-closed or optional-skip policy before installation.

Context contributions have History, Run, Turn, or Call lifetime. A History
contribution is committed by its owning producer before projection and is not
reinserted as an instruction. Run contributions last one run; Turn and Call
contributions expire at their respective boundaries. Only trusted Rust
producers can submit contributions. Model text, tool output, and client JSON
never become trusted instructions by default. The projection leaves durable
history intact. The current module list contributes nothing.

Final output is accepted by the Rust publication service only after nonempty
and size checks. The run state, private model interaction, and one terminal
assistant chat row commit in one user-plane transaction. A failed commit keeps
the caller's recoverable run unchanged. Drafts, tool results, failures, and
cancellations remain private.

Workflow defaults are embedded in the binary. An authenticated, validated
application-plane override may tighten limits within shipped ceilings. The
effective policy revision is a digest of resolved settings, not an independent
counter. Live changes are read at loop boundaries; the current limit is checked
again before publication. Tool denials remain mandatory. Named Auto mode waives
only per-tool Ask checkpoints after live denial checks; other modes retain
approval semantics. Model selection remains pinned at admission. Base prompts, saved agent slots,
selected guidance and permitted schemas resolve from current sources at each
model-call boundary; dispatch rechecks their source and authority fingerprints.
Permanent selection identities survive compaction and restart without freezing
their text. See [live model-call context](context-assembly.md). Live revocation
still applies before dispatch.

## Manager message admission

The independent V3 incoming-message gate stores submissions as visible user chat
before admission to the working agent. Agent metadata owns
manager_incoming_message enablement beneath the Identity revision; criteria live
in config/prompts.json under manager_incoming_message.<agent-id>. User-owned
manager_review: documents retain submissions, pinned criteria/model plans and
generation-fenced verdicts; manager_pending: documents are temporary per-session
queue pointers. Acceptance alone atomically adds the user interaction and agent
run. Rejected and errored submissions never enter model history. See manager.md.

## Authoritative record map

Each family has one owner. Cross-plane references are checked in application
code because separate SQLite files cannot enforce cross-file foreign keys.
All persisted sensitive values use the v3-only encryption key; identifiers and
indexes needed for lookup may remain clear. No v2 data or credentials are imported.

| Record family | Authority and current location | Sensitivity | Lifecycle |
| --- | --- | --- | --- |
| Accounts, profiles, role, status, tier assignment, agent catalog membership | Application; `db/app.db` | Private identity and access | Create or revise by administration; retain audit of changes |
| Global shutoff, workflow/tool policy, installation policy, model catalog and grants | Application; `db/app.db` | Security configuration | Revisioned; recheck at dispatch and recovery |
| Provider credentials, machine tokens, auth sessions, encryption key | Secrets interface backed by application documents and `v3/data/encryption.key` | Secret | Never expose through chat, receipts, logs, or migration from v2; rotate/revoke explicitly |
| Agent definition, prompt slots, ability visibility, connections and gate configuration | Agent; `agent_data/<agent>/<agent>.db` | Private agent configuration | Revisioned; delete/disable after reference checks |
| Conversation, public chat, run, private interaction, approval, receipt, context checkpoint, attachment, memory, user artifact | User; `user_data/<user>/<user>.db` and bounded workspace/upload storage | Private user content | Owned by one user; preserve ordering and encryption; apply explicit retention |
| Operational events, diagnostics and measurements | Telemetry; existing app/user event streams and runtime logs | Redacted operational metadata | Bounded retention; no credentials, raw tool arguments, or provider payloads |

Current schema declarations and document routing are mapped below. "Canonical"
means the authoritative mutable record; "projection" can be rebuilt from it.
Encrypted content and sensitive metadata follow the sensitivity noted above.

| Table or key family | Plane and authority type | Sensitivity | Lifecycle |
| --- | --- | --- | --- |
| `user_accounts`, `user_profiles` | App; canonical identity/profile | Private | Revisioned account lifecycle |
| `entitlement_tiers`, `agent_memberships` | App; canonical grants and caller relationships | Private security configuration | Revisioned; changes rotate account generation and recheck running work |
| `agent_directory` | App; canonical agent ID and owner index | Private identity metadata | Add after validated agent storage; check each referenced agent file |
| `agents`, `agent_connections`, `agent_prompts` | Agent; canonical configuration | Private | Revisioned agent lifecycle; nondefault agents use separate validated files |
| `prompt_slots` | Agent; legacy compatibility source | Private | Retain for rollback; active reads use `agent_prompts` |
| `chat` | User; canonical accepted publication | Private content | Append once per accepted message |
| `interactions` | User; canonical private execution history | Private content | Append by run step; retain for replay |
| `run_index` | User; projection of run documents | Private metadata | Update with run; rebuild or verify against it |
| `events` | App or user; operational event stream | Redacted metadata | Append; bounded retention |
| `legacy_event_map` | App or user; migration mapping | Private metadata | Preserve through rollback window |
| `docs`: `manager_review:`, `manager_pending:`, `session:`, `session_mode:`, `session_chat_controls:`, `session_ability_state:`, `run:`, `run_view:`, `attachment:`, `promotion:`, `receipt:session:`, `receipt:message:`, `receipt:run:`, `receipt:approval:`, `receipt:chat_controls:` | User; canonical state and idempotency receipts | Private | Revisioned or immutable by key; user ownership check |
| `docs`: `agent:`, `agent_abilities:` | Agent; compatibility/configuration document | Private | Migrate to typed agent tables when replaced |
| `docs`: all remaining keys, including `workflow_policy`, provider tokens, shutoff, auth, model catalog, `storage:split_migrated`, `storage:access_v1`, and `storage:agent_split_v1` | App; canonical policy, secret, control, or migration marker | Security sensitive or secret | Revisioned, revoked, or versioned by family |

| `docs`: `user_agent_page_preferences:` | User; canonical ordered Agent page favorites | Private UI preference | Revision-fenced writes; caller identity selects user file; retained with account |

The `docs` routing is implemented in `Store::pool_for`; new prefixes must be
declared here and routed deliberately. A key without a declared owner must not
be treated as user or agent state merely because the generic app fallback can
store it.

The table declares ownership for both implemented and planned records. Confirm
runtime wiring and migration markers before treating newly added accounts, tiers,
or agent stores as live capabilities. Inline text attachments now use encrypted
`attachment:` user documents and a same-transaction interaction copy. They are
scoped to the owning session, retained with its user history, and never treated
as trusted instructions. Binary attachments, memories, and optional modules
remain planned. For any new table or document, add its owner, authority,
sensitivity, retention, and cross-plane checks here before migration.

## Conversation history operations

Undo atomically archives removed public chat and private interactions inside the encrypted user-owned `receipt:session:history:` record, removes them from active history, resets compaction context, and fences retained run documents against further control. Run and attachment audit records remain retained; Undo does not reverse external tool effects. Outstanding runs, reviews and undelivered background work block rewind. Fork copies bounded chat, complete tool pairs and attachment records with new identifiers into the same caller-owned plane, retaining the source agent but inheriting no executable runs, approvals, workspaces, credentials or activated capabilities. `chat_context_boundaries` is an additive user-plane projection binding each publication to its durable interaction cursor. Legacy final replies use whole-run boundaries; legacy progress without an exact boundary fails closed. History archives inherit session receipt retention.

## Migration and rollback contract

Migrations are numbered and idempotent. Stage new schema without deleting the
old representation; copy and validate record IDs, ordering, encryption, counts,
and references; then write a completion marker only after all records verify.
Retry must detect conflicting destinations instead of overwriting them. Back
up the whole v3 data directory, including its key, before a deployment that
migrates live state. A compatible rollback may read the preserved old records;
an incompatible rollback requires restoring that whole backup. Never copy v2
accounts, tokens, credentials, keys, or session data into v3 as part of this
phase. Existing split-storage migration uses `storage:split_migrated` version 2;
future schema changes need their own versioned marker and tested retry path.

The Phase 3 agent split keeps `agent_data/shared_default/shared_default.db` as the
default agent store and preserves legacy nondefault agent rows there as rollback
material. The additive `storage:agent_split_v1` marker binds a digest of those
source rows; an initial copy or interrupted retry must match every destination
record before the marker is written. After completion, each nondefault agent's
database is authoritative. Later configuration edits go only to that database,
so the preserved legacy rows become stale and cannot provide a current rollback.
Rolling back after such edits requires restoring a whole-v3-data backup captured
at a compatible point or explicitly reconciling agent changes. The runtime must
fail on changed legacy source rows, missing agent databases, ownership conflicts,
foreign rows, or malformed encrypted records rather than silently choosing one
copy. This migration never opens or copies user session databases.

## Acceptance evidence

Phase 0: review this ownership map against every new record and verify the
versioned migration, conflict rejection, and encrypted reopen tests. Phase 1:
run locked Rust tests for checkpoint contracts, live policy validation,
context lifetimes, publication idempotency, approval invalidation, cancellation,
shutoff, and uncertain-write recovery. Then deploy the tested image to
`webagent-dev-v3` and exercise authenticated chat, read tool, approval gate,
reload, and recovery in the v3 development UI. A healthy container or unit-test
pass alone does not meet the Phase 1 exit condition.

## Tool-execution policy and background record ownership

| Record or resource | Owner and authority | Sensitivity and retention | Cross-plane contract |
| --- | --- | --- | --- |
| Agent `tool_execution` settings | Revisioned agent ability configuration; shared Rust resolver validates mode, concurrency and run-end behavior | Agent configuration; existing encrypted agent-record retention | Scheduling only, never a grant, capability escalation or approval |
| Effective execution flags and semantics | Trusted model-call runtime guidance from `src/tool_execution.rs` | Call-scoped contribution; not durable user instructions | Bundled skill text and tool outputs cannot override effective flags |
| Active tool counters and installation resource lock | Process-wide Rust OnceLock limiter; per-agent/tool counters across users/sessions, total bounded by loop-runtime tools.max_parallel_calls (maximum four), installation-wide shared-read/exclusive-write guard | Transient identity keys only; removed when active count reaches zero; no per-resource registry | Applies to actual authorized foreground/background execution, not submit/task-wait controls; no database reads or persisted semaphore state |
| Background task handle and completion envelope | Parent authenticated runtime task service, not low-level worker or model arguments | Session/user-bound tool arguments/results are sensitive untrusted evidence; use runtime’s existing encrypted run/event ownership and retention, never a plaintext sidecar | Deliver results/completion at a model-call boundary; stop cancels, silent notifies without a loop and carries leftovers to next user turn, wake has a read-only ceiling with no escalation, background spawn or inherited approvals |
| Durable V3 broker job and finish receipt | Protected build broker plus original Rust call/recovery watcher | Existing caller/session-bound broker receipts and retention | Not a generic background task; no duplicate uncertain deployment/finish and no implied cancellation/replay by generic run-end policy |

The module adds no storage schema or migration and does not establish a new durable background-record format. The parent service owns task persistence, cancellation, authorization rechecks, completion publication and recovery; audit its exact record names and existing retention when those contracts change. OnceLock coordinates one executor process only; independent executors require installation-wide coordination before aggregate concurrency can be claimed. Do not inspect encrypted session database files for this verification.

Background capability and concurrency capability are separate. In addition to the six background-vetted tools, get_time, get_date, calculate, web_search, get_weather and maps_geocode allow foreground concurrency up to four and use the shared installation read guard, but cannot background or silent/wake. Other stateful discovery/native controls stay concurrency one and exclusive. The API can report max_supported_concurrency(name); omitted/null tool_execution maps or null tool entries resolve to defaults.

## Background read tasks (additive, integration acceptance pending)

`background_task:` documents belong explicitly to the authenticated user plane.
Arguments, results, origin references, lease state, and delivery references are
always AEAD-encrypted, including when administrator plaintext storage is enabled.
`background_task_index` is a bounded pending-work projection in the same user
file; it holds only task/session identifiers, revision, and status.
`background_task_delivery` holds task ID and consuming run ID. A consuming run's
input and task delivery receipt commit in its existing save transaction; a
competing consumer fails instead of duplicating evidence. Cross-plane agent and
account references are checked through live authorization before dispatch and
again before result delivery. Reads never confer new authority.

Admission permits at most 256 active or unconsumed completed tasks per user;
consumed completion documents remain retained for stable-call idempotency and
status lookup, excluded from pending scans. Cancelled/failed records are also
retained but excluded from pending quota. This implementation does not yet
supply archival pruning of retained task documents. Admission fails closed when
the pending quota is full. Schema creation is additive and idempotent and does
not read or migrate live session database files outside the Store APIs.

The scheduler starts an independent background service with at most four local
inflight reads. Shared execution limits remain enforced by call_foreground.
Leased interrupted safe reads may be reclaimed after expiration. The renewable lease lasts thirty seconds and is refreshed every ten seconds; a lost claim cancels the local operation before another executor can publish its result. Completion evidence stays untrusted.
Postrun classification, durable notices and read-only wake admission are owned
by background_followup; its delivery claims must share the delivery receipt
transaction. No deployment or end-to-end acceptance is implied by these modules.

## Live diagnostic plugin archive

`plugins/diagnostics/logs.db` owns fixed-admin operational evidence and sealed
context snapshots. The standalone diagnostics process writes and prunes it; the
trusted core bridge provides authenticated read-only projections without creating
archive files. Existing seven-day metadata and 24-hour snapshot defaults remain.
No plugin reads session databases through the protocol, and credentials/keys are
never transmitted. Installed executables run as administrator-trusted application
code, not as untrusted tool workers. The archive and operational config travel with the installed package.

The installed package owns diagnostics.json and logs.db (including SQLite WAL sidecars). Removing the folder removes archive access and capture; moving it preserves its settings and evidence. The host does not recreate it. Ephemeral socket/PID/ready files remain under /state/diagnostics. Snapshot ciphertext still requires the originating installation key to decrypt; a package alone does not carry that key.

## Session naming configuration

The mutable data-root config/prompts.json owns session_naming: prompt,
model_role (system, standard or premium), attempts_per_job (1–10),
retry_delay_ms (0–60000), request_timeout_seconds (1–300),
recovery_retry_delay_seconds (1–86400), and max_job_attempts (1–20).
The shipped defaults are System, 3 calls per job, 400 ms between calls,
45 seconds per call, 60 seconds recovery cooldown, and 3 job claims total
(at most 9 provider calls, fewer when an interrupted claim consumes a slot).

Startup adds a missing section under the existing prompts lock without
replacing other fields. Invalid live configuration uses shipped defaults with
a fixed diagnostic; invalid content is preserved for correction. A prompt-only
string remains readable, and omitted object settings use shipped defaults.

The first successful chat turn queues naming and snapshots these settings in
encrypted session auto_naming.config. Edits apply to newly queued jobs; retries
and restart recovery retain their snapshot. System uses the first message's
independently pinned System selection. Other supported roles resolve and pin
their own agent assignment at first dispatch, persist that plan, and never
alter chat or compaction selections. Live authorization is still rechecked.
Exhausted jobs leave the New: fallback title and leave the discovery queue;
manual titles remain locked against late results. Existing pending jobs receive
a settings snapshot on their first new claim and honor the configured cap.

## Outbound notification ownership (Phase 1)

The reviewed [outbound notification contract](outbound-notifications.md) adds
application-owned notification_outbox, notification_relay and notification_retention
tables plus sealed notification_reader: credential documents. User-owned
notification_stage and notification_emitted tables commit metadata projections
with lifecycle changes. The relay watermark makes acknowledgement retries safe
after canonical pruning. Exported IDs, timestamps and fixed status metadata are
plaintext and installation-admin-only; no conversation content or secrets leave
the user plane. Reader credentials bind live administrator authority, generation
and expiration and grant only event reads. Schema creation is additive and
idempotent; retain rollback data and follow the existing backup contract.
Canonical history expires after 30 days or one million rows; source markers
follow source-resource retention and staging is acknowledged after relay.

## Tool-output retention

Live app defaults in data/config/tool-output-retention.json and revision-fenced Identity overrides reload each working model call. App-level data/config/prompts.json owns tool_output_retention guidance and placeholders. Expiration defaults off. Only successful native-designated perishable reads are eligible. A metadata-only ledger belongs to user-owned session/run documents, commits with existing run fences, and inherits at execution. Active context can replace expired output with bounded recall receipts; original encrypted interactions remain intact. Promotion has a separate character budget and never grants read permission. Compaction still applies; original source evidence must be reread after edits. Disabling expiration restores un-compacted available replay, with bounded recall for older segments.

### Retained build task reminders

The protected V3 broker owns reminder metadata in its existing
build-jobs/coordination/items.json task registry. Session storage is not a
second authority. Each task keeps last_activity_at and an optional
leftover_notice; its revision includes task identity, outcome, activity and
operator timing policy. Reviewed revisions persist across restarts.
Completed, cancelled, abandoned or operator-reconciled work withdraws its
reminder. A later unresolved revision can create a new reminder.

A bounded post-process reconciles every minute. Open work becomes eligible
after 24 hours without broker-recorded edits, state changes or Cargo checks;
failed, interrupted, uncertain and stale terminal jobs qualify immediately.
Polling does not reset activity. Disk accounting is advisory, capped at four
two-second inspections per cycle, and cannot authorize cleanup.

The authenticated session notice route projects only tasks with the same user
and session and current Codebase Admin access. Rust checks access again after
broker I/O; the browser discards switched-session responses. Administrator Builds
provides the fallback for operator tasks without a chat session identity.
Review acknowledges an exact revision and preserves source, receipts and
uncertain effects. There is no model wake, automatic retry, chat-history
publication or deletion.

## Optimization experiment ownership

Agent metadata ability_settings.optimization owns encrypted, revision-fenced
preferences. User-owned run documents pin optimization_request settings and
explicit command guidance; session_ability_state owns dependency activation.
These use existing Store routing and retention without a database migration.
The protected broker owns durable build-jobs/performance reservations, caller
scope, frozen application-only source, pinned image/harness identities and
bounded profiling artifacts. Plugin folders cannot grant execution or erase
these records. Ephemeral data/optimization readiness and sockets are shared by
API and loop; current policy and renewable host leases govern execution.
Interrupted workloads retain charged reservations and never replay. Profiling
outputs remain untrusted evidence rather than instructions.

## Shared conversations and participation
The application plane owns shared session directory, participants, hashed single-use
invitations, invitation signing secret, bounded observation audit and requester run
reservations. Reservations atomically enforce account run ceilings across canonical
owners, reconcile legacy active work, and release only after a durable terminal
outcome. Stable retry IDs retain uncertain admissions without freeing capacity
merely because a canonical write is absent. Canonical chat,
runs, attachments, shared help, per-message author/eligibility metadata, participation
state, durable follow-up queue and mutation receipts remain in the original owner's
user database. Personal participant read markers remain inside that canonical session.
Existing conversations are indexed additively through authenticated Store operations.

Shared access never changes authentication identity. Actor, storage owner, run requester
and credential owner are separate values. A session invitation only supplies a scoped
visitor relationship when general agent membership is absent; explicit revocation,
account ceilings, agent restrictions and tool denials remain authoritative.
Administrative viewing and joining are independent agent Identity policies.

Inactive human messages and queue withdrawals are public human transcript only:
they are never added to interactions, compaction or recall input. Participation
generations fence claims, model/tool results, publication, background completion and
recovery. Reactivation does not replay inactive transcript or resume stale work.

Restricted tool credentials use app/secrets-plane tool_credential:<owner>:<kind>
documents, bound to the authenticated requester. Authorization records bind account,
credential version, arguments and current execution restrictions. No invitation,
observer role, agent mode, or model-selected user ID delegates that authority.
Tool results remain internal execution records and are not automatically published
as chat messages. Owner-only approval projections expose actionable detail only
to the credential owner.

Help request/cancel/resolution transitions and retry receipts commit with staged
metadata-only outbound events in the canonical user database. The existing outbox
relay exports no conversation text, credentials, tool arguments or approval contents.

## Durable followup queue

Encrypted user-owned followup_queue:<session> version-1 documents are the
canonical bounded session ledger, including submissions, attachment references,
review decisions, delivery choices, interruption target, batch membership and
consumption receipts. receipt:queue: is user-owned idempotency state. The ledger
and retry receipts share existing user retention; terminal entries remain for
stable bubble association. Each pending session permits at most 256 submissions.
No application-plane fallback or plaintext queue sidecar is used.

SQLite writer reservation serializes submission, choice changes, review results,
batch consumption and final publication across API and loop processes. Individual
public user bubbles commit with submission; only approved consumed batches enter
private interactions. A continuation inherits retained private execution context,
while unknown effects remain behind existing recovery controls. Crash recovery
uses the same ledger and committed run identity; explicit Stop/failure holds
ordinary messages until Resume queue. New document families are additive, versioned
and do not copy or rewrite historical messages.

Session recycling cancels pending queue entries inside its lifecycle transaction,
advancing revisions so restore and late review results cannot revive them.
Permanent deletion also removes queue retry receipts and queued-message
attachment metadata.

History actions check the queue in the same writer transaction. Pending entries
block Undo. An individual followup belongs to a combined input; Undo requires
its original message rather than rewriting part of that input. Ordinary Undo
archives and removes affected queue entries. Fork includes only batches admitted
before the chosen context boundary, with remapped attachments and immutable
delivered status; it inherits no executable queue or review authority.

## Duplicate-action protection
Rust owns requester-scoped version-1 `action:` documents in the requester's user
database, independent of the canonical conversation owner. They are always
AEAD-encrypted, including administrator plaintext mode. The additive
`action_index` stores only IDs, resource identifiers, hashed intent lookup values and state;
`action_lock` reserves the SQLite writer before lookup, registration and claims.
Arguments, origin message references, claims, attempts, provider keys, results and
receipts remain sealed. Agent tool-execution settings own the revisioned
`duplicate_protection` override; settings never grant permissions.

Initial protected native adapters are prompt_slot_write, edit_own_prompt,
save_own_skill and remove_own_skill. Canonical effect identity excludes retry
preconditions; different effects remain separate. Equivalent prompt effects share
a family. Matching additional actions bind a later admitted human origin and
specific requester approval, including in Auto mode. Provider adapters must use
the native stable provider key and supply authoritative reconciliation; this
release adds no purchase or scheduling service.

Native prompt/self-edit receipts commit with the agent-plane mutation and may
resolve a running or uncertain requester-plane record after interruption, including
when cancellation already held the owning run for recovery. Missing evidence
never authorizes replay. Expired run ownership does not reclaim a consequential
action. Late results are attempt-fenced. Action records survive compaction,
Undo/Fork, and conversation deletion; no automatic expiry is introduced.
Fork inherits no executable claims. Cross-plane reads and claims recheck live
requester, resource, policy, participation and credential-owner authority.
Bounded action statuses and saved-result lookup use get_task_checkpoint and
call-scoped native guidance. Saved results remain untrusted evidence.

## Optional service module ownership

The authenticated user owns always-AEAD module_state:<module> documents and
receipt:module:<module>:<occurrence-hash> admission receipts. State includes
definitions, pending work, remembered state, event cursors, delivery reservations,
history and immutable mutation receipts. CAS revisions serialize updates; admission
receipts commit with the ordinary message/run or manager-review transaction.
They retain the user's existing history lifetime and explicit bounded quotas.

Application-owned module_install:<module> binds enabled state and installation
generation. Package files are explicitly administrator-installed code, not tool
worker instructions. Packages use bounded host protocol frames and have no direct
database/key interface. Ordinary run policy, requester quota and approval authority
remain core-owned. Package removal retains user state but deactivates it on a new
generation. Pending and uncertain effects are reconciled without silent replay.

Ephemeral module lifetime and scoped request locks live beneath the state-root
modules directory. Content-free, peer-checked dispatcher wake hints use the
runtime control socket. Neither grants permission or replaces durable recovery.
The independent outbound package consumes canonical lifecycle metadata through
the existing administrator read contract; no conversation text is exported.
