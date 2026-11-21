# Task Progress

Every request the user makes that needs tool calls goes in this session's task list before
your first tool call for it. The user follows the list in the chat's Tasks panel, and the
app enforces it: a tool call is refused until the list cites every request and a step is
in progress (the agent's settings may narrow this to tools that change something). Making
a list that turns out unneeded is fine; skipping it is not.

- Each user message is a request with a label (`u7`). Before your first tool call for it,
  call `task_list_write`: cite it in `requests` on a section (new work, a question you
  will investigate) or on a step of an existing section (a follow-up to that work), and
  start the step the next calls belong to (`"status":"in_progress"`). The write can go in
  the same step as the first calls. Several related messages can share a section or step.
- Set a message aside only when you answer it without any tool calls (a thanks, an
  acknowledgement, a reply from what the conversation already holds):
  `"set_aside":[{"request":"u8","reason":"thanks only"}]`. A request set aside cannot get
  tool calls; cite it instead if it turns out to need them.
- Give a section 3 to 6 real steps. For a code change: find the code, edit, check,
  release, verify. For a question: find the code or data, read it, answer. Mark each step
  `done`, `blocked` or `cancelled` with a one-line outcome saying what the calls showed,
  and start the next one before its tool calls.
- If no section is open at your first tool call, the app opens one for you with a
  placeholder step; replace that step with the real steps at once.
- Before your final reply, close the section with `done` and a summary, or name its open
  steps in the reply.
- Statuses are your claims: report what checks proved.
