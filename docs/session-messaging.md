# Native session messaging

Base exposes two distinct native host tools. They reuse Rust session preparation,
permission/approval checks and the durable follow-up queue; they are not HTTP
loopback tools or executable ability plugins. Admission does not promise a
recipient answer, a particular response, or immediate completion.

## Tool contract and recipient behavior

| Tool | Inputs | Outcome |
| --- | --- | --- |
| message_other_session | target_session_id UUID, nonempty text up to 65536 UTF-8 bytes, delivery queue (default), steer or interrupt | One attributed message admitted to another owner-private session. |
| start_session | text; optional agent_id (sender agent by default), optional title | One new owner-private session and its first attributed message committed together. |

Both return session_id, message_id, submission_id, delivery_state queued,
run_id null and session_created. The recipient resolves its own model, abilities,
policy and execution mode. New sessions start with fresh context and a separate
workspace identity: no parent transcript, credentials, loaded abilities or
approval grants are copied. An omitted title uses normal session preparation.

Queue waits for ordinary run completion; steer is consumed at the next safe
model boundary. Interrupt requires a currently running recipient bound by exact
run identity/generation in the admission checks. A pending approval is not a
running native interrupt target. Existing interruption machinery retains paired
tool evidence and uncertain effects behind recovery controls. Human queue choices
remain revision-fenced. Eligible queue or steer submissions are consumed as one combined
input; interrupt handoff consumes one selected interrupt at a time. Review, participation and recovery
holds can postpone consumption. Explicit Stop/failure holds queued work until
Resume queue under the existing queue contract.

The public bubble carries message_kind agent_message, source session/agent and
sender title. Model context uses the attributed envelope from context(), saying
that delivery is owner-authorized agent content, not a direct human instruction
or a permission grant. Acknowledgement instead of following the body is allowed.
No automatic transcript merge, reply routing, side-agent loop, or inferred
message_main_chat behavior is implemented by these two tools. Cross-session
forwarding always needs fresh owner approval.

## Authority and approval

Sender and recipient must belong to the authenticated storage owner. Reject self,
foreign, deleted/recycled sessions, inactive recipients, sharing-enabled agents,
admin-view/admin-join agents and participant-bearing shared sessions. Live access,
mode, capability catalog and session lifecycle revisions bind the approval and
remembered route. A changed binding invalidates approval; it does not silently
broaden the old grant. Ordinary Base/tool visibility and explicit Deny remain
in force. The dedicated read-only sender exception applies only to these two
write-classified tools; it grants no other write tool.

Every new session, interrupt and onward agent-origin message requires exact owner
approval even in Auto mode. Queue/steer may use an explicitly remembered directed
route, otherwise exact owner approval is required. The approval card displays
source, destination, delivery and recipient mode. Its opt-in checkbox remembers
only eligible queue/steer routes. Polling preserves the explicit checkbox choice;
interrupt/new-session/onward requests cannot grant a reusable route.

Config: plugins/abilities/Core/base/base.json -> /tools registers the native tools.
Tool schemas live in session_messaging::schemas; discovery is filtered by live
ability access and policy. Agent policy rules and runtime access assignments are
revisioned host state, not a new source JSON switch. policy_defaults fills absent
rules without replacing explicit denials. These existing policy defaults are not
an authority bypass or a migration requirement for future work.

Config: ui/chat/chat.json -> /chat_limits/attachments/max_per_message
bounds combined queue attachments; combined context is capped at 65536 bytes.
An oversized batch is held for the owner to reduce rather than silently split.
Native messages add no attachments, but can share a batch with human submissions.
No message-routing preference JSON is consumed. Authorization, atomicity, origin,
and revision fences are invariants. Optional future UI presentation labels or
placement could be configurable; they are not implemented routing settings.

## Persistence, retries and recovery

All records use the owner's existing encrypted user plane and retention model;
there is no plaintext queue sidecar or new database migration. followup_queue:
contains the durable delivery choice, origin envelope, template, review, revision,
interrupt target and consumption identity. message_meta: carries the public origin
and context eligibility. messaging_route:<source>:<target> stores a directed,
revisioned grant. receipt:agent_message: binds sender run/call identity and exact
arguments; receipt:messaging_route: binds revocation request identity and revision.

messaging_commit serializes admission with a SQLite writer reservation, validates
sender generation/revision/participation/lease, inserts any new session documents,
creation event and notification, recipient bubble, queue entry, optional route and
receipt in one transaction. A failed initial message leaves no orphan new session.
An identical successful retry returns the original receipt before capacity checks;
conflicting input identity fails. It does not create another bubble or session.

Application-directory registration follows the owner transaction. Its failure
cannot turn a committed admission into an unknown result. A retry and delivery
revalidation repair the directory before consumption; conflicting directory
ownership is never overwritten. Typed AuthorityChanged cancels pending deliveries
when access/lifecycle/mode/capabilities/route authority is confirmed withdrawn.
Transient I/O errors defer verification and preserve pending work. The consuming
writer transaction repeats owner-plane access, mode, lifecycle and route checks.

Agent-origin input never counts as a new human origin for duplicate-action
approval, including mixed consumed batches. Undo/Fork preserve historical origin
metadata but inherit no executable route/queue authority. Permanent deletion
removes session-owned messaging receipts and grants involving that session.
No automated pruning of a still-needed admission receipt is added here.

## Owner routes and UI connections

Authenticated routes are GET /api/v3/sessions/{sid}/messaging-routes and
POST /api/v3/sessions/{sid}/messaging-routes/{target}. POST accepts only
expected_revision and client_request_id; it revokes rather than creates grants.
Browser writes require CSRF. Exact revocation retry returns the original result;
conflicting revision/input is rejected. Approval uses the existing
POST /api/v3/approvals/{id}/decision with optional remember_messaging_route.

panel mounts mountMessagingRoutes in the notification host, opens it only for
owner-manageable sessions, disconnects before switching sessions and destroys it
with the panel. load ignores stale generation/session responses. Revoke retains
one request ID across transport failure and removes it only after confirmed
success. chat-action-panel shows approval binding and bounded checkbox state;
message-view renders the agent-origin label rather than calling the sender You.
These views do not grant authority; the API always rechecks it.

## Function guide

| Source and symbols | Purpose / important constraint |
| --- | --- |
| session_messaging::owns, schemas | Native dispatch membership and exact model-facing argument contracts. |
| policy_defaults, queue, checked_text, stable_id | Preserve explicit policy rules, default delivery, reject blank/control/oversize bodies, derive stable per-call identities. |
| AuthorityChanged::fmt, authority_changed | Type confirmed revocation separately from transient errors. |
| private_session, recipient_capabilities, binding | Resolve private owner access and capture exact source/recipient/mode/catalog/interrupt authority without effects. |
| route_key, route_scope, route_for | Identify directed grants and require exact saved authority scope equality. |
| preflight, preflight_inner | Heap-back the security checkpoint, invalidate stale decisions and calculate exact approval/remember eligibility. |
| context, stamp | Build attributed model evidence and preserve origin on prepared message/run/manager-review records. |
| execute, reconcile_directory | Validate running sender and pending call, replay known receipts, atomically admit new work, repair directory and wake scheduler. |
| revalidate, revalidate_inner | Heap-back boundary validation; cancel only typed withdrawn authority, defer transient failures. |
| routes; api/session_messaging::list, revoke | Owner-only grant projection and strict revision/idempotency-fenced revocation. |
| Store::messaging_commit, validate_agent_delivery_tx | Atomic queue/session/receipt admission and repeat authority validation inside the consuming writer transaction. |
| Store::cancel_agent_delivery, revoke_messaging_route | Revision-fenced authority withdrawal and durable revocation retry. |
| followup_queue::prepared_entry, submit, scheduler_tick, interrupt_requested | Share queue construction, retain ordinary human admission, revalidate before scheduler consumption or interrupt observation. |
| Store::legacy_message_meta, followup_consume, followup_choice | Preserve origin, batch eligibility and owner choice; consumed agent entries mark the run as agent-origin. |
| live_tool_decision, filter_ability_entry, effective_policy_for_mode | Keep ordinary policy/visibility checks while allowing only the dedicated read-only delegation exception. |
| execute_run_inner; execute_parallel_batch; call_raw_inner; native_schemas | Apply the same preflight in single/batch paths, bind call-scoped delegation guidance and execute the native adapter. |
| api/chat::approval; shared_projection::messages | Recheck exact owner approval binding and expose durable origin to public transcript projection. |
| Store::action_origin, conversation_history, purge_session | Exclude agent input as human authority; preserve fork evidence and remove session-owned messaging state on deletion. |
| mountMessagingRoutes; load; open/disconnect/destroy | Mount disclosure, fence late reads, reset session state and dispose UI callbacks. |
| mountChatActionPanel; showTool; destroy | Display exact destination/approval, retain eligible explicit remember choice across polling and clear it on disposal. |
| createMessage; panel::open, runs, disconnect, destroy | Show origin labels and connect owner route UI and approval callbacks to selected-session lifecycle. |

## Verification and limits

Implementation release 16a339d4-d673-55b7-bcc3-6f549fcf2120, commit
c4acf8e411a3094edd42a9126eb97e68de67310d, promoted exactly 29 paths.
Both default/live compile profiles, optimized linked/live equivalence and 983
workspace tests passed (five existing ignored); 11 native messaging cases and
17 focused frontend checks passed. Native tests cover exact approval/idempotency,
new-session atomic rollback, directed route/onward rules, combined batching and
reopen, read-only/Deny boundaries, CSRF, deferred directory repair, transient
verification and recipient authority changes. UI fixtures cover checkbox polling,
retry identity and late responses. prepared/execute_run helpers establish the
native authorized pending state rather than mocking recipient authority.

Live API acceptance proved first approved delivery, route reuse without another
approval, exact stable revocation retry, fresh approval after revocation with idle
steer, and one newly created session with its initial delivery. Each had one
persisted attributed input and one nonempty completed assistant reply. The harness
initially used a missing approval path (404, no effect) and an inappropriate exact
response marker; its correction preserves those failures and checks exact body
and terminal uniqueness. No live actively running recipient interrupt was tested.
The future inline-size regression is not proof of maximum polling-stack size;
heap-backed preflight/revalidation also passed the ordinary-stack manager test.
Prompt-restoration test drop restores original bytes, permissions and mtime so
later compilation does not mistake fixture activity for changed build inputs.

Evidence: build-jobs/checks/base-session-messaging-20261018/ includes
release-commit.json, live-acceptance.json, acceptance-harness-corrections.json,
stack-baseline-measurement.json, stack-repair-measurement.json and the immutable
failed-job lineage. Original implementation remains closed; later documentation
and Code Index review uses separate activity identities. No push is implied.
