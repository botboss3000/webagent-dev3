# Prompt slot read

Use `action: list` to see ordered agent instruction slots and their revisions. Use `action: get` with `slot_name` for one slot. This reads only the current session-bound agent slots, never raw database records or another session. Full content may be returned; keep it out of public chat unless needed for the user’s request.

## Execution

Read-only: runs in the foreground, the background or by model choice, up to four at once. A background call returns a handle; its result arrives as a notice.
