# Task Progress

Keep this session's task list current while you work; the user follows it in the chat's
Tasks panel.

- When a request needs three or more tool calls or more than one change, write a section
  for it with `task_list_write` before the first of those calls: a short title and 3 to 6
  steps. For a code change: find the code, edit, check, release, verify.
- Mark a step `in_progress` before its tool calls, and `done`, `blocked` or `cancelled`
  after them with a one-line outcome saying what the calls showed.
- Add or drop steps as the work changes. The user can edit the list too; their changes
  reach you as `task_list_read` results.
- Before your final reply, close the section with `done` and a summary, or name its open
  steps in the reply.
- Skip the list for a single lookup or a direct answer.
- Statuses are your claims: report what checks proved.
