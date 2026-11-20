# Write the session task list

`task_list_write` adds or updates one section of this session's task list. A section is
one piece of work: a user's request (`"origin":"user"`) or your own investigation
(`"origin":"agent"`, the default). Each call names the list's current revision in
`expected_revision`; a stale revision is refused, so read the list and write again.

- New section: `{"expected_revision":4,"section":{"title":"Fix login redirect",
  "origin":"user"},"steps":[{"label":"Reproduce on phone"},{"label":"Patch cookie path"}]}`.
  The result gives the new revision and the section and step ids (`s3`, `s3.1`).
- Update: give the section's `id`, and each step to change by its `id`; steps left out
  are untouched, and steps without an `id` are added. Start a step with
  `"status":"in_progress"` and finish it with `done`, `blocked` or `cancelled` (to drop
  it); add a one-line `outcome` when it finishes. Tool calls made while a step is in
  progress count as its evidence; `evidence` may narrow that to the result ids that matter.
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
  `cancelled` drops the whole section. Reopen a closed section with `"status":"open"`
  when a follow-up needs it.

Track every multi-step piece of work here as you go; a plan the user approved already has
its section, and that section's progress also updates the plan's checklist. The user can tick, add and remove steps in the chat's task panel; each change reaches
you as a `task_list_read` result from the user, and the list's revision moves with it.
If you end a turn while steps are still open you are reminded once; answer without a tool
call to keep your earlier reply, or keep working. Statuses are your claims, not verified evidence: report what checks proved.
Anything another session or a later day needs also goes in Main Notes.
