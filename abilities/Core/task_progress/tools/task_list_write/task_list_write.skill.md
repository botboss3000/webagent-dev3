# Write the session task list

`task_list_write` adds or updates one section of this session's task list. A section is
one piece of work: a user's request (`"origin":"user"`) or your own investigation
(`"origin":"agent"`, the default). Make it in a model step that also makes other tool
calls, never on its own. A write changes only what it names and applies to the current
list, so `expected_revision` is optional and never refused.

- New section: `{"section":{"title":"Fix login redirect","origin":"user","requests":["u3"]},
  "steps":[{"label":"Reproduce on phone","status":"in_progress"},{"label":"Patch cookie path"}]}`.
  Leave out `id`: the result gives the section and step ids (`s3`, `s3.1`) and the revision.
- Update: give the section's `id`, and each step to change by its `id`; steps left out
  are untouched, and steps without an `id` are added. Start a step with
  `"status":"in_progress"` and finish it with `done`, `blocked` or `cancelled` (to drop
  it); add a one-line `outcome` when it finishes. Tool calls made while a step is in
  progress count as its evidence; `evidence` may narrow that to the result ids that matter.
- `requests` on a section or step cites the user requests (`u7`) it answers; citations
  only add. The app already cites each new request on the newest open section (or a new
  one) before your first tool call for it, so cite only to move a request somewhere else.
  `set_aside` (a list of `{"request","reason"}`, no section needed) is rarely needed: the
  app sets aside requests you answered without tool calls when you reply.
- `notes` keeps the section's decisions and constraints.
- Fold earlier work when a new task begins: on the write that starts a section's first
  step, set `"fold_earlier":true` if this task does not need the details of the work before
  it (a separate request, not a follow-up or the next step of the same work). Everything
  before the task folds into a summary in the background and this task's messages stay word
  for word; `fold_focus` names what the summary must keep (decisions, file paths, open
  questions). Folded messages stay recallable with `recall_compacted`. Leave it out to keep
  everything; automatic compaction still folds old work when the conversation grows. The
  result's `fold` says whether a fold was requested or why not; never retry it.
- Close a finished section with `"status":"done"` and a `summary` of what was done,
  decisions and where results live; every step must be done, blocked or cancelled first.
  `cancelled` drops the whole section. A section's status is only `open`, `done` or
  `cancelled` (steps take `in_progress`). Reopen a closed section with `"status":"open"`
  when a follow-up needs it.

When to keep the list and how is in the Task Progress skill. A plan the user approved
already has its section, and that section's progress also updates the plan's checklist. The user can tick, add and remove steps in the chat's task panel; each change reaches
you as a `task_list_read` result from the user, and the list's revision moves with it.
Statuses are your claims, not verified evidence: report what checks proved.
Anything another session or a later day needs also goes in Main Notes.
