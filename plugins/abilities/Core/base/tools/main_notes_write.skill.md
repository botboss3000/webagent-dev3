# Write Main Notes

Create or update a topic entry in the current session agent's Main Notes. The host determines agent scope and enforces tool permissions and the configured Notes write-approval mode. The optional Main Chat coordination toggle does not grant or withdraw this tool's access.

First read the relevant topic and current revision with main_notes_read. Supply a stable request_id, expected_revision and contribution. The contribution contains topic_id, title, scope and an entry with id, text and state. Reuse existing topic and entry IDs for updates. Optional fields include groups, sources, subtopic, timing, condition, progress and outcome. Keep action intent in text, current progress in progress and observed results in outcome.

Supported entry states are open, waiting, blocked, scheduled, in_progress, done and cancelled. Supply timing only when requested by the user, using the supported once, delay or cron shape; preserve the original request time for relative timing. Sources should identify verifiable supporting context without copying credentials or private material from another agent.

A write may produce a pending proposal rather than a committed change. Do not report a proposal as saved or approved. For every-write mode, new topics or expanded scope, follow the returned approval status. Reuse an identical request ID and body only for an exact uncertain retry; after a revision conflict reread and reconcile before sending a new request.

This tool records notes. It does not execute actions, create schedules or grant authorization. Any later action or scheduling call requires its own current permission and approval checks.
