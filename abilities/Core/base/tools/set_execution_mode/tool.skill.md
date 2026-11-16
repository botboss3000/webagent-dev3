# Change the session mode

Call with `{"mode":"<id>","expected_revision":<current mode revision>}`. The current revision is in the session mode note of every step. Use a short `reason` when it helps the user.

## Leaving Plan

Leaving Plan for any other mode needs `plan`, the plan the user agreed to. Without it the call is refused before the user sees anything. `plan` holds `title` (up to 3 words), `goal`, `criteria` (how you will know it is done), `notes` (decisions and constraints worth keeping in view) and `checklist`: 1-32 steps of `{"label":"..."}`, each optionally with `verification`.

Request `mode: "auto"` to start executing. The user sees the plan with three choices:

- **Authorize Auto and continue**: the session enters Auto; execute the plan.
- **Continue in Ask mode**: the session enters Ask instead. The result carries a `note` saying so: execute the plan, and tools that need approval ask the user.
- **Keep planning**: the call is denied and the session stays in Plan. Ask what is still open, then request again.

Once the user lets the work start, the plan is saved as the session's plan artifact; the user opens it from the Artifacts chip in the chat header. Track progress with `update_task_checkpoint`.
