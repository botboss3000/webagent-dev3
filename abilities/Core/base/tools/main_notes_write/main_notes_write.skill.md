# Write cross-session notes

## Usage

- Contribute durable cross-session goals, decisions, blockers or actions; reuse topic and entry IDs.
- Policy may save a proposal for approval.
- Never invent timing.
- This does not execute or schedule actions.

Create or update a topic entry in the current session agent's cross-session notes. The host determines agent scope and enforces tool permissions and the configured Notes write-approval mode; by default writes save without approval. The optional central chat coordination toggle does not grant or withdraw this tool's access.

Supply a stable request_id, expected_revision and contribution. For expected_revision use the newest revision you have: the notes header in your instructions, the latest update from another session, or your last write's result (each committed result returns the new revision). Write directly; do not read first just to confirm the revision. A stale revision is safe: the write is refused with a revision conflict and changes nothing, and only then read with main_notes_read and retry. Read first only for entries the notes in your context do not show. The contribution contains topic_id, title, scope and an entry with id, text and state. Reuse existing topic and entry IDs for updates. Prefer updating or closing an existing entry (state done or cancelled) over adding a similar one, so the notes every session carries stay short. Optional fields include groups, sources, subtopic, timing, condition, progress and outcome. Keep action intent in text, current progress in progress and observed results in outcome.

Supported entry states are open, waiting, blocked, scheduled, in_progress, done and cancelled. Supply timing only when requested by the user, using the supported once, delay or cron shape; preserve the original request time for relative timing. Sources should identify verifiable supporting context without copying credentials or private material from another agent.

A write may produce a pending proposal rather than a committed change. Do not report a proposal as saved or approved. For every-write mode, new topics or expanded scope, follow the returned approval status. Reuse an identical request ID and body only for an exact uncertain retry; after a revision conflict reread and reconcile before sending a new request.

This tool records notes. It does not execute actions, create schedules or grant authorization. Any later action or scheduling call requires its own current permission and approval checks.
