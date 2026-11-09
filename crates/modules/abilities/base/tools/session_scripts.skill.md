# Session scripts

A session script is a named list of tool calls saved for this conversation tree: the
top-level conversation and every sub-agent session started from it, at any depth. Save
one when you or your sub-agents will run the same sequence more than once, for example
the same set of browser screenshots for several design options.

- `script_save {name, description, steps:[{tool, arguments}], params:[names]}` stores it
  for the tree; saving the same name again replaces it. Up to 20 steps and 100 scripts per
  tree (live limits in `loop-runtime.json`).
- `script_list {}` shows the tree's scripts: name, description and params.
- `script_run {name, args:{param: "text"}}` runs the steps in order and returns each
  step's result. It stops at the first step that fails; screenshots from steps attach as
  they do for direct calls.

`{{param}}` placeholders inside argument strings are replaced with the run's `args` text.
Arguments only fill text: they cannot add steps or choose tools.

Steps may be read-only tools and Browser Control tools (`browser_read`, `browser_input`,
including its `script` action). Saving refuses a step whose tool is a write outside that
set or would need approval here. Each step still runs through the normal permission
checks as if you called it: a script never gains permission, so a sub-agent whose
permissions differ may see a step denied.

Writing a script:

- Make anything that differs between callers a param: URLs, session or conversation IDs,
  labels, names. Never hard-code your own session or context.
- A session's browser can open only its own conversation. A script that views a conversation
  takes it as a param, and each caller passes its own: `/app?session_id=<your session id>`.
- Wait for what each step depends on, for example a `wait_for` visible step before reading
  or clicking something the page draws later.
- Keep it small and single-purpose; describe each param in the description.
- Test it once with `script_run` before telling sub-agents or siblings to use it.
- If a shared script does not fit your case, pass different args or save a variant under a
  new name; do not overwrite a script others use.

Sub-agents: the parent saves the script before starting them and names it in each
brief, with the args to pass. A sub-agent runs it with `script_run`; it does not need to
save its own copy. Scripts are private to this user and agent and to this tree; other
conversations never see them, and they go away with the top-level conversation.
