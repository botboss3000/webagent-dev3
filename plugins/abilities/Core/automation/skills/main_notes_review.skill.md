---
id: main_notes_review
title: Review Main Notes
when_to_use: Coordinating Main Notes changes and follow-ups in Main Chat.
default_mode: discoverable
minimum_access: registered
---

Main Notes is durable cross-session coordination evidence. Keep temporary plans,
minor edits and intermediate results local. Read current notes before contributing;
reuse topic and entry IDs. Preserve sources and user-requested timing. Never invent
when an action is due. Approved topic scope bounds automatic updates; expansion
requires a proposal in new-topics mode. Note writes never authorize execution.

Main Chat owns coordination. For each changed action choose: execute an already
authorized action under current tool permissions, schedule a reminder/follow-up,
ask for user input/approval, or wait. Other sessions contribute intent and timing;
they must not independently schedule the same Main Notes action.

For relative timing, retain the original request time and use an absolute once
trigger when the due time is known. A late review must not move the due time.
For scheduling use existing Automation tools with main_notes_action containing
topic_id and entry_id, the exact saved trigger, and the entry text as the prompt.
Include this binding when updating its automation. If a topic scope changes,
review the other actions in that topic and reconcile their schedules too. Do not create an unbound second
schedule. Reconcile an existing schedule and uncertain effects before changing or
retrying it. Execution/automation approval applies independently of note approval.

Record progress and outcomes in the same entry's progress and outcome fields;
preserve text as its action intent. Main Chat state-only
updates remain quiet; changed intent, timing, sources, scope or reopened actions
request another review. Surface meaningful results, reminders and approval/input
requests. Routine bookkeeping need not produce chat messages. Respect the overall
Identity toggle; disabling retains data and suspends new coordinated admissions.

Before any action, including a timed follow-up, read the current entry and verify
its conditions against current evidence. Confirm execution authorization from
source references or existing permission/approval records. When authorization or
a condition is unclear, ask or wait; topic approval never supplies it.

Use schedule_task for note-owned timed work, including reminders, so a Main Chat
run rechecks conditions, delivers the result and records progress and the observed
outcome. Apply the Notes write policy to that record; propose it when approval is
required. A direct reminder is not a substitute for executing a conditional action.
