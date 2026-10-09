# V3 session manager — incoming-message gate

This is an independent V3 implementation. No V2 manager prompts, configuration,
contracts, or runtime code are imported.

The agent Identity page has Review incoming messages and Acceptance criteria.
Agent owners and delegated administrators edit these through the existing
revision-fenced Identity API. Review defaults off. Enabling requires nonempty
criteria and an explicitly assigned, authorized System model. Existing permissions,
account ceilings, provider ownership, CSRF and session ownership remain mandatory.

## Lifecycle

The chat API validates a submission and pins the agent run inputs and manager
criteria/model plan. With review enabled, one transaction stores the visible user
message, attachments, retry receipt, private manager_review document and
manager_pending session pointer. It creates no run or model-facing interaction.
The message keeps its actual user-owned session ID.

A separate manager scheduler in the existing loop process claims pending reviews.
There are at most two concurrent reviews, independent of agent run concurrency.
The tool-free System call sees only the submitted text and prepared attachment
context, with administrator criteria in trusted instructions. It must return
exactly {"verdict":"acceptable"} or {"verdict":"unacceptable"}.
It never receives the working agent's tools or publishes assistant text.

Acceptance atomically creates the queued agent run and its user interaction,
removes the pending pointer and records the verdict. Rejection preserves the
visible message without a run or interaction. Therefore rejection cannot enter
later history, compaction or recall derived from interactions. Public chat and
private chat search retain the submitted message as conversation evidence.

States: pending_review, reviewing, acceptable, unacceptable, review_error.
Public message snapshots expose only safe admission status, verdict, sanitized
error and provider-reported usage. Manager inputs and provider response text are
not public. The chat refreshes review badges while a review is pending.

Criteria/model assignment changes, revocation, global shutoff, malformed responses,
timeouts and provider failures do not admit a message. A claimed review expires
after 50 seconds, with a 45-second total operation deadline; interrupted calls become review_error and are never automatically
replayed because provider usage may already have occurred. A user may submit a
new message after an error. Retrying the same client_message_id returns the
current review outcome rather than creating another call.

A pending session blocks new submissions and recycling. Ordinary chat is unchanged
when review is off. Disabling review does not promote previous rejected messages
or bypass reviews already submitted under an enabled gate.

## Record authority

Agent metadata owns manager_incoming_message settings beneath the Identity
revision. An internal gate revision advances when its settings change; pending
reviews pin it, so toggling off and back on cannot resurrect an older verdict. User docs own manager_review:<review-id> (submission, pinned settings,
model plans, proposed run, state, generation, deadline and verdict) and
manager_pending:<session-id> (temporary queue pointer). Existing message retry
receipts, attachments, chat and admitted runs stay in their current user plane.
No new database file or runtime-data source snapshot is introduced.

Future publication and tool gates should get separate modules under
src/manager/gates with explicit event, evidence and effect contracts.
