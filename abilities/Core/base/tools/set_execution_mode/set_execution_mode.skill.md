# Change the session mode

## Usage

- Change this session to an agent execution mode ID (ask, plan, auto, or custom); legacy read_only/write are supported at a revision checkpoint.
- Entering Auto needs human approval.
- Leaving Plan needs plan: the agreed plan, shown to the user on the approval card and saved as the session's plan artifact when they let the work start.

Call with `{"mode":"<id>"}`. The current mode and its guidance are in every step's guidance message. If the user changed the mode since your last step, the call is refused and says so; ask the user before changing it again. Use a short `reason` when it helps the user.

## Leaving Plan

Leaving Plan for any other mode needs `plan`, the plan the user agreed to. Without it the call is refused before the user sees anything. `plan` holds `title` (up to 3 words), `goal`, `criteria` (how you will know it is done), `notes` (decisions and constraints worth keeping in view) and `checklist`: 1-32 steps of `{"label":"..."}`, each optionally with `verification`.

Request `mode: "auto"` to start executing. The user sees the plan with three choices:

- **Authorize Auto and continue**: the session enters Auto; execute the plan.
- **Continue in Ask mode**: the session enters Ask instead. The result carries a `note` saying so: execute the plan, and tools that need approval ask the user.
- **Keep planning**: the call is denied and the session stays in Plan. Ask what is still open, then request again.

Once the user lets the work start, the plan is saved as the session's plan artifact (the user opens it from the Artifacts chip in the chat header) and its checklist becomes a user-requested section of the session task list; the result names it. Track progress in that section with `task_list_write`.
