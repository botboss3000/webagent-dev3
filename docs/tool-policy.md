# Tool policy: how a tool call is decided

One place for the whole path a tool call takes, from the model's tool list to execution.
Field formats live in [drop-in-abilities.md](drop-in-abilities.md) (`<tool>.tool.json` and
ability manifests); how the tool list reaches the model is in
[context-assembly.md](context-assembly.md).

## 1. Is the tool in the model's list?

The tool definitions sent with a call are every installed tool of an ability that is not
Off, minus tools switched Off, abilities removed from the session and tools the caller's
permissions deny (`crates/modules/engine/src/runtime/run.rs`, schema gating;
`crates/modules/abilities/src/catalog.rs` `tool_gates`). Only a direct change alters it:
a user turning an ability or tool Off or on, removing an ability from the session, a
permission change, an administrator installing, enabling or disabling an optional package,
or a release or live edit that changes a tool definition.

- An ability's discoverable (`on_request`) or visible (`loaded`) mode decides only whether
  its skills are in the instructions, never whether its tools are listed.
- Optional packages (Automation, outbound notifications, MCP, Optimization) count while
  installed and enabled, whatever their health; the catalog's host interface has no health
  input. A call made while a package is unhealthy fails at execution (step 5).

## 2. May this caller use it?

`live_tool_decision` (`crates/modules/engine/src/runtime/policy.rs`) resolves Allow, Ask
or Deny from the caller and the agent: Administrator tools serve only administrator
accounts; `owner_tier` and `role_access` limit who may ever have the tool; the capability
level's saved permission applies, else the tool's `default_policy` for the role, then the
next role down, then the effect (a read allows, a write asks). Core tools skip ability and
tier gating but keep their own policy. A tool switched Off is denied as if it did not exist.

## 3. Execution mode and arguments

The session's execution mode adjusts the decision (`for_execution_mode`): Auto runs calls
that would ask, Plan keeps changes back and requires the agreed plan to leave it. Denied
calls and invalid arguments return a result to the model without running. Arguments are
checked against the tool's `parameters` (`crates/modules/engine/src/tools/arguments.rs`).

## 4. The task gate

A tool call the gate covers runs only when the session task list accounts for every user
request and a step is in progress (`Engine::task_gate` in `run.rs`, also on the
parallel batch path in `batch.rs`; rules in `crates/modules/chat/src/task_list.rs` `gate`).

- `task` in `<tool>.tool.json` is `required` or `exempt`. Absent: a core tool is exempt,
  otherwise a write is required and a read exempt. Coordination and housekeeping tools
  (asking the user, messaging sessions, Main Notes, the agent's own prompt and skills, model
  switching, opening a terminal, presenting a sandbox) are marked exempt.
- Every user request gets a session label `u1`, `u2`, ... when its message is stored: typed
  messages, messages sent while the agent works, other participants' and other sessions'
  messages, and an automation's trigger. Approvals, background notices and the agent's own
  messages are not requests. Labels live in `base_tasks:{sid}:labels`, apart from the list,
  so a new message never makes the agent's list write stale.
- A request is accounted for when a section or step cites it (`requests`) or it is set
  aside with a reason (`set_aside`). Answering a question does not file it.
- With no open section, the gate opens one itself: titled from the first unfiled request,
  citing every unfiled request, with a placeholder step in progress; the call runs and the
  agent receives the new section as a `task_list_read` row on its next call. With a section
  open, a call that finds unfiled requests or no step in progress is refused
  (`error_kind: task_required`) with the requests named and what to do.
- A `task_list_write` in the same model step counts as made, whichever order the calls run
  in, so a step can file its work and make its change in parallel; a write the rules would
  refuse, or one citing an unknown request, counts as not made.
- What it covers is the agent's Task Progress setting `task_gate`: `all_tools` (the
  default: every tool call except core tools, the task list, skill, time and mode basics,
  whatever `task` says; a newest request set aside as needing no tools refuses the call,
  so a request that gets tool calls is always cited), `changes` (only tools whose `task`
  is required) or `off` (the per-step and end-of-turn reminders stay).
- Plan mode is independent: approving a plan still seeds its section.

## 5. Messaging, the action ledger and the repeat guard

Private messaging tools pass their own destination and authority preflight. Tools with
`duplicate_protection` or `critical` go through the action ledger
(`app.actions.preflight`), which refuses a repeat inside its window or records the call.
The repeat guard (`repeat_guard`, writes default to high risk) holds or asks for an
identical repeat within one run. Calls that would ask wait for the user's approval card;
after an approval the step runs again and passes every check a second time.

## 6. What `effect` drives once a call runs

`effect` (`read` or `write`) never decides access. It drives recovery (an interrupted
write is uncertain and never rerun; a read is retried), parallel batches, background work,
history retention of old results, the repeat guard's default and, absent a `task` value,
the task gate.
