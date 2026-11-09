# Native session messaging

Agent Management exposes these native host tools. They reuse Rust session preparation,
permission/approval checks and the durable follow-up queue; they are not HTTP
loopback tools or executable ability plugins. Admission does not promise a
recipient answer, a particular response, or immediate completion.

## Tool contract and recipient behavior

| Tool | Inputs | Outcome |
| --- | --- | --- |
| message_session | session_id UUID of a session in the agent's scope, nonempty text up to 65536 UTF-8 bytes, delivery steer (default) or now | One attributed message admitted to another owner-private session. |
| start_spawn | text; optional agent_id, optional title | One sub agent session (parent_session_id set, shown as a tab of the sender) and its first message. Its run ends send the sender a notice. |
| start_session | text; optional agent_id, title, notify (default false) | Without agent_id (or with the sender's agent): a fork carrying the sender's history through its last completed reply. With another agent_id: a fresh session of that agent, only when the sender's `allow_delegation` setting is on; when it is off the model's schema omits `agent_id` and preflight refuses one with a fixed reason. started_by_session_id is set. |
| delegate_to_agent | text, agent_id, optional title | A fresh session of that agent and a browser handoff request. |

Session scope (`agent_management.session_scope`): `started` reaches the sender's
sub agents and the sessions it started; `owned` adds every owner-private session.
The Default Agent defaults to `owned` and to delegation allowed.

Background notices: when a run of a sub agent (or a start_session session with
notify) ends or waits for an approval, the run save stages
`background_notice:<parent>:<event>` (source `sub_agent` or `session`) in the same
transaction. Broker builds (source `build`: job ended, stage slow or stalled) and
tool calls that kept running in the background after a user message (source
`tool`) stage the same records from the engine. The follow-up scheduler delivers
all staged notices for a conversation as one steer message (`platform_notice`,
framed as a background report, never user instructions), committing
`receipt:background_notice:<event>` with it so a retry never repeats one. The message
wakes an idle agent and joins a running one at its next step without cutting off
its work. A parent
that is archived or deleted drops its notices. Archiving a session archives its
sub agents; deleting it deletes them and cancels automations delivering into it.

All return session_id, message_id, submission_id, delivery_state queued,
run_id null and session_created. The recipient resolves its own model, abilities,
policy and execution mode. New sessions other than forks start with fresh context and a separate
workspace identity: no parent transcript, credentials, loaded abilities or
approval grants are copied. An omitted title uses normal session preparation.

Steer is consumed at the next safe model boundary; a pending steer left when a
run ends starts the next run. Now is a steer that also cancels the recipient's
current model call (its partial response is discarded and the run continues with
the message) and detaches a running tool call without grace. Human queue choices
(Send now, Cancel) remain revision-fenced. Eligible steers are consumed as one
combined input. Review, participation and recovery holds can postpone
consumption. Explicit Stop/failure holds queued work until Resume queue under
the existing queue contract.

The public bubble carries message_kind agent_message, source session/agent and
sender title. Model context uses the attributed envelope from context(), saying
that delivery is owner-authorized agent content, not a direct human instruction
or a permission grant. Acknowledgement instead of following the body is allowed.
No automatic transcript merge, reply routing, side-agent loop, or inferred
message_main_chat behavior is implemented by these two tools.

## Authority and approval

Sender and recipient must belong to the authenticated storage owner. Reject self,
foreign, deleted/archived sessions, inactive recipients, sharing-enabled agents,
admin-view/admin-join agents and participant-bearing shared sessions. Live access,
mode, capability catalog and session lifecycle revisions bind the approval and
remembered route. A changed binding invalidates approval; it does not silently
broaden the old grant. Ordinary ability/tool visibility and explicit Deny remain
in force. The dedicated read-only sender exception applies only to these two
write-classified tools; it grants no other write tool.

Approval follows the ordinary tool policy and session mode: Auto proceeds, and a
tool set to Ask asks in Ask mode. When asked, the approval card displays source,
destination, delivery and recipient mode. Its opt-in checkbox remembers only
eligible steer routes; a remembered route binds later sends to its
revocation. Polling preserves the explicit checkbox choice; now,
new-session and onward requests cannot grant a reusable route.

Config: the Agent Management ability (plugins/abilities/Core/agent_management) holds the tool folders and settings.
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
contains the durable delivery choice, send-now flag, origin envelope, template,
review, revision and consumption identity. message_meta: carries the public origin
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
| private_session, recipient_capabilities, binding | Resolve private owner access and capture exact source/recipient/mode/catalog authority without effects. |
| route_key, route_scope, route_for | Identify directed grants and require exact saved authority scope equality. |
| preflight, preflight_inner | Heap-back the security checkpoint, invalidate stale decisions and calculate exact approval/remember eligibility. |
| context, stamp | Build attributed model evidence and preserve origin on prepared message/run/manager-review records. |
| execute, reconcile_directory | Validate running sender and pending call, replay known receipts, atomically admit new work, repair directory and wake scheduler. |
| revalidate, revalidate_inner | Heap-back boundary validation; cancel only typed withdrawn authority, defer transient failures. |
| routes; api/session_messaging::list, revoke | Owner-only grant projection and strict revision/idempotency-fenced revocation. |
| Store::messaging_commit, validate_agent_delivery_tx | Atomic queue/session/receipt admission and repeat authority validation inside the consuming writer transaction. |
| Store::cancel_agent_delivery, revoke_messaging_route | Revision-fenced authority withdrawal and durable revocation retry. |
| followup_queue::prepared_entry, submit, scheduler_tick | Share queue construction, retain ordinary human admission, revalidate before scheduler consumption. |
| Store::legacy_message_meta, followup_consume, followup_choice | Preserve origin, batch eligibility and owner choice; consumed agent entries mark the run as agent-origin. |
| live_tool_decision, filter_ability_entry, effective_policy_for_mode | Keep ordinary policy/visibility checks while allowing only the dedicated read-only delegation exception. |
| execute_run_inner; execute_parallel_batch; call_raw_inner; native_schemas | Apply the same preflight in single/batch paths, bind call-scoped delegation guidance and execute the native adapter. |
| api/chat::approval; shared_projection::messages | Recheck the recipient binding and expose durable origin to public transcript projection. |
| Store::action_origin, conversation_history, delete_session | Exclude agent input as human authority; preserve fork evidence and remove session-owned messaging state on deletion. |
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
and terminal uniqueness.
The future inline-size regression is not proof of maximum polling-stack size;
heap-backed preflight/revalidation also passed the ordinary-stack manager test.
Prompt-restoration test drop restores original bytes, permissions and mtime so
later compilation does not mistake fixture activity for changed build inputs.

Evidence: build-jobs/checks/base-session-messaging-20261018/ includes
release-commit.json, live-acceptance.json, acceptance-harness-corrections.json,
stack-baseline-measurement.json, stack-repair-measurement.json and the immutable
failed-job lineage. Original implementation remains closed; later documentation
and Code Index review uses separate activity identities. No push is implied.
