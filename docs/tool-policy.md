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
which stale reads history retention drops (section 7), the repeat guard's default and,
absent a `task` value, the task gate.

## 7. History retention and the action ledger

Every call's full arguments and result are saved in the session's history. Once the
conversation has moved past a call (behind the compaction point, or in a session idle for
`storage.trim_idle_days` in `config/access/policy.json`), the cleanup
(`crates/modules/files/host/storage/history_retention.rs`) trims the stored row to the
tool's `retention` level in its `tool.json`:

| Level | Tools | What the stored row keeps |
| --- | --- | --- |
| `disposable` | reads and lookups | the outcome fields only |
| `log` | commands and edits | the outcome, short fields and the end of long text; listed inputs become their size |
| none (`record`) | everything else, and every critical tool | the whole call |

- A trimmed row records the fingerprint and size of what it replaced
  (`original_sha256`, `original_bytes`), so it stays checkable.
- Calls the compacted conversation still carries word for word (`checkpoint_input`) are
  never trimmed, even behind the compaction point: a background compaction can finish after
  calls that its summary does not cover.
- The duplicate-result check (`dedup_prepare`, see context-assembly.md) uses the
  conversation's copy when the stored original was trimmed and the trim's fingerprint matches
  it (or predates fingerprints); any other mismatch still pauses the run.

`critical: true` marks significant actions: payments and store changes, messages sent,
infrastructure and access changes, and settings and agent changes. A critical call is never
trimmed and always goes through the action ledger (section 5). Its ledger record keeps the
full arguments, the result, and the user requests it answers (`requests`, `request_link`):

- `cited`: the requests the in-progress task step cites (its section's when the step cites
  none). Request labels (`u1`, `u2`, ...) are counted whatever the task gate setting.
- `inferred`: no step cites one, so every request labelled since the session's previous
  ledger record (`action_link:{sid}`).
- `none`: no request to link; the Actions panel flags the record.
- `automated`: the run was started by an automation or another agent, not a user message.
- `guard`: the critical tool guard (section 8) matched the call to this request.

Each request's text is cut to `ledger.request_text_max_chars` (default 1000) in
`config/access/policy.json`. Ledger records (`action:{id}`) are kept when their session is
deleted.

## 8. The critical tool guard

Before a critical call reaches the action ledger, one model request asks whether the user
asked for this action and which of their messages did
(`crates/modules/engine/src/runtime/critical_guard.rs`, rules in
`crates/modules/engine/src/tools/critical_guard.rs`).

- **What it sees:** the user's newest requests by label (`u1`, ...; for a scheduled task's
  run, its stored instruction as `task`, shown as "The user set up this scheduled task and
  asked: ..."; never another agent's message) and the call's
  short arguments. A string longer than `guard.argument_max_chars` is shown only as its
  size, so text the agent read (a page, an email body) cannot argue for the call. It never
  sees tool results or the agent's reasoning.
- **Rule:** one universal rule. A tool's `tool.json` may add a `guard` object: `check`
  (details that must match, at most 300 characters), `show` and `hide` (parameters always
  shown in full, or never shown). Only a critical tool may have one, and it cannot loosen
  the rule.
- **Models:** the agent's Decision model (a System One decision model such as TypeSafe
  Jev; the request goes to `{base_url}/v1/systemone` from its provider file), then that
  model's aliases in order, which may be decision or text models. An agent with no
  Decision model uses its System model and that model's aliases. A text model answers in
  one line (`requested u3`). Each model gets `guard.model_seconds`, all together
  `guard.total_seconds`. Every question carries an `instructions` text.
- **Local models:** self-hosted decision models (Laya, Kev on the sidekick) are the `local`
  provider ("Local models"). The containers cannot reach the private network, so the
  request goes over `/runtime/local-models/relay.sock` to the host's local models relay
  (`build-service/local_models_relay.py`; registry, keys and install in
  `build-service/README.md`), which holds each model's address and key. The administrator
  adds them in the Model Catalog from the relay's list (`GET
  /admin/models/catalog/local-models`, no key) as decision-only entries; once published,
  each is offered on every agent's Models page and is stored for an agent when its saved
  roster uses it. Such a model reads only `max_input_tokens` (from the relay's list), so
  the guard shows it the newest messages whose whole request still fits (about four
  characters a token). The `decide_with_jev` tool stays TypeSafe-only.
- **Outcome:** a `requested` answer at or above `guard.pass_confidence` (a text model's
  answer has no confidence and counts at face value) lets the call continue. Anything
  else, including no model answering, holds the call for the user's approval
  (`pending.guard_hold`): Auto mode does not convert it and only a human approval
  dispatches it. The approval card shows the verdict line. An approved call is not checked
  again.
- **Record:** the verdict, confidence, linked request and model are kept on the pending
  call (`pending.guard`) and in the ledger record (`guard`, `request_link: guard`).
- **Mode:** the agent's Task Progress setting `critical_guard`: `enforce` (default),
  `record` (checked and recorded, never held) or `off`. It is changed by whoever may change
  the agent's ability settings.
- **Settings:** the `guard` section of `config/access/policy.json` (defaults: 0.85, 12
  messages, 300 characters each, 120 characters per argument, 5 s per model, 10 s in total).
