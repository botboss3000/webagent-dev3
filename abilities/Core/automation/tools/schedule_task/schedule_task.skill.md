# schedule_task

Specify an explicit destination and a trigger: once with at_ms, delay with delay_ms, or five-field cron with an IANA timezone.

## Binding a Main Notes entry (`main_notes_action`)

Only the agent's Main Chat may bind, and the destination must be that Main Chat. The entry must match the schedule exactly, so write it first with `main_notes_write`:

- the entry's `text` is exactly this call's `prompt`;
- the entry's `timing` is exactly this call's `trigger` object (for example `{"kind":"cron","expression":"0 14 * * *","timezone":"America/Detroit"}`);
- the entry is not done or cancelled.

Keep a fact such as a user interest in its own entry and give the schedule a separate action entry. A mismatch is refused with `invalid_main_notes_binding` and its reason; nothing was scheduled, so fix the entry or the call and try again.

An account keeps at most the agent's "Max Automations per User" alive, and never more than the agent owner's plan allows (Free 3, Plus 50 by default). Past it the call is refused with `automation_capacity`: tell the user, and offer to cancel an automation they no longer need.
