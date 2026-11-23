# Task Progress

The session task list shows the user, in the chat's Tasks panel, what you are working on.
Its current state is always in your instructions, so never call `task_list_read` to see it.
Keeping it never costs a model step of its own: write it only in a step that also makes
other tool calls.

- Each user message is a request with a label (`u7`). The app files requests itself:
  before your first tool call for a new request it cites it on the newest open section,
  or opens a section titled from the request when none is open, and tells you on your
  next step. Messages you answer without tool calls (a greeting, thanks, a question the
  conversation already answers) need nothing; the app sets them aside when you reply.
- If the app filed a request in the wrong place, move it in your next write: cite it in
  `requests` on the right section or step, or give a new section a title and cite it there.
- Work with more than two tool calls gets steps, written alongside your first calls for
  it: 3 to 6 real steps (for a code change: find the code, edit, check, release, verify;
  for a question: find the code or data, read it, answer). Start a step with
  `"status":"in_progress"` and finish it with `done`, `blocked` or `cancelled` and a
  one-line outcome saying what the calls showed, in the same step as the next calls.
  A quick lookup needs no steps.
- Before your final reply, close a finished section with `done` and a summary, in the
  same step as your last tool calls; otherwise leave it open and say in the reply what is
  left.
- Statuses are your claims: report what checks proved.
