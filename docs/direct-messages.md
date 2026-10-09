# Single-call first-message API

POST /api/v3/messages uses existing user cookie authentication and X-CSRF-Token.
It is not exposed through the messaging bearer router or the event-reader token.

Required client_message_id (existing 128-byte bound) identifies the entire operation
within the authenticated user's storage. text is optional only with valid inline
attachments; text has the existing 65,536-byte bound. Optional agent_id defaults to
shared_default. Omitted title preserves automatic naming; a supplied nonempty title
locks naming as in POST /sessions. attachment_ids, attachments, model_entry_id and
the existing Standard-only model_role use normal send validation. Unknown fields,
including user identity, are rejected.

HTTP 202 returns session {id,title,agent_id}, run_id and attachment metadata.
Incoming-message manager review instead returns run_id:null, review_id and admission.
The caller can read the returned session to observe review and run progress.
The immutable receipt replays the original response even after naming or review
changes. Equal key/body digest returns that response; a changed body gets HTTP 409
"Request identifier reused with different input". JSON key order does not change
the existing digest; omitted fields and explicit defaults are distinct input.

Session, mode, attachment records, chat, run or manager review, retry receipt,
private events and session.created/message.received notification stages commit in
one user transaction. Invalid admission or rollback leaves none of them.
The original two-step endpoints and browser New Chat flow retain their contracts.
Uploaded IDs are session-bound; a fresh session cannot take files belonging to
another session. Inline attachments support single-call attachment-only admission.
No new upload staging, authentication or event types are introduced.

Tests cover retries and concurrent duplicate admission, immutable replay, manager
review, atomic rollback, admission failures, attachment provenance, route auth/CSRF
and two-step regression. Release uses the protected development broker's locked
suite and build, then live authenticated session/run/outbox acceptance.
