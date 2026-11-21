# Live model-call context

The context release resolves permanent instruction identities against current
sources at every model-call boundary. Prompts, skill bodies and tool schemas
participate in that refresh; selecting a skill does not freeze its text.

## At a glance: what is in a model call and when it changes

A request is read in this order; a provider reuses its cache from the first byte
up to the first byte that changed.

| Part | What it holds | When it changes |
| --- | --- | --- |
| Tool definitions | Schemas of every installed, enabled tool the agent may use | Only a direct change: Off, session removal, permissions, or an install or release that changes a tool (a major reorder); see **What the tool definitions depend on** |
| Instructions | Base instructions and prompt slots, skills not in the history, the skill menu, Main Notes guidance and snapshot, the session task list snapshot | A block merge or major reorder; a changed live source (prompt, skill or menu file) |
| Retained rows R1, R2, ... | Per fold: loaded skills (`load_skill` results), the latest `set_execution_mode` result, one combined Main Notes change, one combined task list change | Written once by their fold, then unchanged until a merge or major reorder |
| Summary blocks B1, B2, ... | One summary per fold | A merge rewrites the oldest ones into one |
| Verbatim tail | Recent turns word for word, with any skills, notes changes and mode changes made since the last fold | Grows each call; a fold summarizes its older part |
| Per-step guidance message | The mode's turn guidance, the one-line session task state and, once per new run-health issue, a review prompt | Sent after the history for one call only, never saved, so it never breaks the cache |

Four events reshape the request:

- **Fold** (built): summarizes history after the newest block into a new block and
  keeps that range's retained rows just ahead of it. Nothing before the new rows
  changes.
- **Merge** (built): once the blocks together pass `merge_tokens`, folds the oldest
  blocks and their rows into one block; their skills and notes go into the
  instructions.
- **Major reorder**: after the cache timeout or when the tool definitions
  change (a direct change only), moves every retained row into the instructions, rebuilds
  the Main Notes snapshot and summarizes only if over the trigger.
- **Keep-alive** (experimental, off by default): resends the last request
  before the timeout so the cache stays warm; the context does not change. It
  carries on after restarts, and opening a session earns it one ping.

Details: **Compaction layout and cache reuse** below.

## Execution route and ownership

The normal loop in src/agent_loop/run.rs hydrates native selection identities
through src/context/assets.rs::hydrate, resolves current catalog and live policy,
and assembles the permitted schema set. prepare_capability reads current prompt
defaults and saved agent slots, then calls the capability-context planner.
project obtains safe placements and schema order from context-assets.
The host validates both results before context assembly and provider dispatch.
Dispatch fingerprints and a synchronous source fence recheck source/authority
after credential retrieval and before the network call. Repeated changes are
bounded and pause for review rather than dispatching stale context.

src/live_assets.rs::resolved_prompts_at assembles the live Markdown prompt files under
config/configuration/prompts (`name.md` text, `name.json` settings, folders
nest) for features such as session naming. There is no base prompt file: an agent's
instructions are its prompt slots (Provider::instructions_for_snapshot only strips old
compiled seeds), and general rules live in the core Base skill. Agent prompt slots are
host-owned encrypted records, read again by prepare_capability. The legacy last-good provider helper
is not the strict dispatch freshness authority. There are no saved overrides.

src/storage/context_assets.rs keeps encrypted conversation selections scoped by
agent and requester in the canonical conversation owner's user plane. Services
receive validated snapshots and compute plans; only the host commits them with
run, lease, participation and revision fences. Selections grant no permission.
Loading through native ability/tool/skill controls stages an explicit selector;
automatic dependencies are resolved from current authority, not made permanent.
The native Base context_assets tool lists selectors and their current revision.
Unload requires the exact selector, expected_revision and stable request_id.
It changes selection only; configured visible guidance can remain present.

## Placement, compaction and cache behavior

Permanent selections store identities, layout anchors, schema order and bounded
idempotency receipts. They do not store frozen instruction bodies. Current full
skill bodies are retained without preview truncation. Temporary menus and trusted
Call/Turn contributions are rebuilt at their lifetime boundaries.

For append profiles, surviving definitions retain safe anchors; new definitions
append after a completed exchange. Temporary native context follows the latest
safe boundary and does not inherit permanent placement. Schemas retain surviving
order and append new permitted names deterministically. Non-append profiles use
their declared instruction prefix. The host accounts for serialized synthetic
growth once, including provider-specific representation.

Synthetic instructions are a model-call projection. They never enter durable
chat history or the history summarized by compaction. Compaction and Undo reset
layout metadata while keeping selectors and receipts. A fold does not change the
instructions: the skills, Main Notes changes and mode it keeps stay in the history
ahead of its new block (see **Compaction layout and cache reuse**). Unload is
revision-fenced; stale saves or retries cannot resurrect a removed selector.

Stable unchanged bytes and ordering can preserve a provider's reusable prefix.
Changed live sources deliberately change those bytes. Actual cache hits, billed
tokens and provider latency are not guaranteed by placement tests. Tool schemas
remain in each endpoint's native request field; this feature does not move them
into durable transcript text.

## Compaction layout and cache reuse

Providers cache a request from its first byte up to the first byte that changed,
so compaction keeps everything before its newest summary block byte-identical.
Agreed design, 2026-11-19; status per part below.

A request reads: tool definitions, instructions (base, skills, Main Notes
snapshot), then history: R1, B1, R2, B2, ... and the verbatim tail. Bn is a
summary block; Rn is the retained rows its fold kept ahead of it, as the original
tool-call exchanges.

**Fold (built, commit 4802868).** Fold n summarizes the history after the last
block and keeps, just ahead of its new block: each `load_skill` exchange still
carrying its text (a skill whose text is in the history stays out of the
instructions); the latest `set_execution_mode` exchange unless the tail has a
newer one; and every Main Notes change from the summarized range and the verbatim
tail (the server's `main_notes_read-…` rows and the session's own
`main_notes_write` results) combined into one `main_notes_read` exchange. Skills
loaded in the tail stay where they were loaded. Summaries leave all of these out.
`summary_blocks` and `leading_blocks` in context-contracts compaction find the
blocks through the rows kept ahead of them, for the planner, the external plan
validator, the merge cut and the fork summary wording.

**Merge (built).** Once the blocks together pass `merge_tokens`, the oldest are
summarized into one block, together with the rows kept ahead of them; the newest
two blocks keep theirs. The merge rewrites the start of the history anyway, so
its rows' skills join the instructions and the Main Notes snapshot is rebuilt:
the snapshot is keyed by `notes_basis` (history revision and `notes_front`,
which a merge's `context_compacted` event increments), not by every compaction.

**Folding folded blocks.** When an automatic compaction is due, the planner first
checks for a merge: at least three blocks whose summaries together pass
`merge_tokens` (default 50k), and a merge source within the summary source limit. The cut falls just after the third-newest block, so
the newest two blocks keep the rows ahead of them. Everything before the cut,
blocks and rows, is the merge source; the summary leaves out skill text, Main
Notes changes and mode results as it does for a fold. The result is one block
followed by the untouched rest. Retained rows that were in the merged range are
gone from the history: their skills are no longer in the history, so the skill
selector adds them to the instructions; `notes_front` increments, so the next
call rebuilds the Main Notes snapshot; and if the merged rows held the only
`set_execution_mode` result, the server adds a fresh one at the end of the
history.

**Skills through their tool results.** A loaded skill's text first reaches the
model as the `load_skill` tool result (status `loaded`, with `text`). While a
result with its text is in the history, the run's skill filter
(`skills_in_history` in the engine run loop) keeps that skill out of the
instructions, so loading a skill never changes the instructions. A fold keeps
the exchange, text included, just ahead of its new block; a skill loaded in the
verbatim tail stays where it was loaded. Only when the result leaves the history
(a merge, or the major reorder) does the selector place the current skill text
among the instructions' skills.

**Main Notes through their changes.** The instructions carry the notes guidance
and one snapshot, keyed by `notes_basis`, which stays frozen between merges.
Changes reach the session as tool exchanges at the end of the history: another
session's write arrives as a server-added `main_notes_read-<id>` row with the
diff for that notes version; this session's own write is its `main_notes_write`
result, with the entry's line before and after. A fold combines every change in
the summarized range and in the verbatim tail, oldest first, into one
`main_notes_read` exchange placed with the fold's other retained rows, and drops
them from the tail. A merge or major reorder drops the diffs and rebuilds the
snapshot from the current notes.

Every committed Main Notes change returns the new notes `revision` (also kept in its
request receipt), and a server-added update row names it ("Main Notes update from
another session (v3; notes now at revision 120):"). So the agent always has the
newest revision for `expected_revision` without a read: the snapshot header, a later
update row or its own last write result. The write and read tool skills say to write
directly and read only after a revision conflict or for entries not shown (commits
c56c939, 1409289).

**Session task list.** Its snapshot, step line, reminders, change rows and task folds
follow the rules above; see **Session task list in context** below.

**Execution mode.** The mode arrives as a `set_execution_mode` result: at session
start, when the user changes it, and when the agent changes it. A fold keeps the
latest one from its range unless the verbatim tail already has a newer one; the
server adds a fresh one whenever no result in the history shows the current
revision. `prompts/mode_guidance.json` (`placement`) decides where the mode's
text goes: `history` puts the full guidance in the results only; `both`
(default) also puts the mode's turn guidance in each step's guidance message;
`per_step` puts the full guidance in each step's guidance message and leaves the
results with only the mode and revision.

**Per-step guidance message.** Call-lifetime contributions (the mode's turn
guidance, and a one-call review prompt when the run has a new issue group to
explain) are joined into one message in the profile's instruction role and
appended after the history for that call only. It is never saved, so the next
call's request differs from the previous one only from that point on: the small
change the cache estimate reports at the end of each call.

**Worked example.** One session through three folds, a merge and a major reorder
(H = history items; bold marks what is new and uncached on the next call):

| After | Tools | Instructions | History |
| --- | --- | --- | --- |
| Start | T | base, notes v12 | mode, user message |
| Skill and notes changes | T | base, notes v12 | mode, user, **load_skill A, notes diff v13, own write v14, more turns** |
| Fold 1 | T | base, notes v12 | **R1 (skill A, mode, notes v13-v14), B1, tail** |
| Fold 2 | T | base, notes v12 | R1, B1, **R2 (skill B, mode, notes v15), B2, tail** |
| Fold 3 | T | base, notes v12 | R1, B1, R2, B2, **R3, B3, tail** |
| Merge | T | base, **skill A, notes v16** | **M (B1 and R1), R2, B2, R3, B3, tail** |
| Major reorder | **T** | **base, skills A and B, notes v17, mode** | **M, B2, B3, tail** |

Measured on 2026-11-19 (session 588ba58a): after fold 1 the first change was
history entry 0, with all 240,568 bytes of tools and instructions reused (63,488
tokens cached by the provider); after fold 2 the first change was entry 5, right
after R1 and B1 (252,932 bytes, 66,176 tokens).

**Major reorder (built, commits 4b118d5, f9b7c50).** Triggered by the cache
timeout (idle time since the session's last model call) or by a change to the tool
definitions, which only a direct change makes (see below; agents have no activation
tool).
Retained rows move into the instructions and Main Notes becomes one current
snapshot with the diffs dropped. The current mode arrives as one fresh
`set_execution_mode` exchange at the end of the history, as later mode changes do. Skills in history that is not summarized
stay where they were loaded. Blocks keep their order unless they are over the
merge limit. A summary is made only when the history is over the trigger. An idle
session reorders at its next call; a running one at its next model call, with the
new tools, pausing only when a summary is needed. The loop keeps each session's
last call time in memory; after a restart the first pass of a run uses the
session's latest stored run update instead. Each reorder logs `context reordered`
with its reason (`idle` or `tools`).

**What the tool definitions depend on.** The tool list sent with each call is every
installed tool of an ability that is not Off, minus tools switched Off, abilities removed
from the session and tools the caller's permissions deny, with fixed per-agent schema
edits (web search providers, `start_session` without delegation). Nothing else changes
it, so it changes only when a user or administrator does something: switches an ability
or tool Off or back on, removes an ability from the session, changes permissions or
those settings, installs, enables or disables an optional package, or releases a changed
tool. An ability's discoverable (`on_request`) or visible (`loaded`) mode decides only
whether its skills are in the instructions, never whether its tools are listed.
Optional packages (Automation, outbound notifications, MCP, Optimization) count as
present while they are installed and enabled, whatever their health: the catalog's host
interface (`module_enabled`, `mcp_installed`, `optimization_installed` in
`crates/modules/abilities/src/ports.rs`) has no health input. A call made while a package
is unhealthy or not ready fails with an error at execution instead
(`crates/modules/extensions/host/modules.rs` checks the module health receipt, the MCP
bridge its readiness marker, `performance_work` the optimizer's). Module health probes
run every `PROBE_INTERVAL_SECONDS` (60) and stay valid for 90 seconds
(`crates/modules/extensions/src/health.rs`). Before 2026-11-21 the catalog offered a
package's tools only while its health was current, and a 15-second health window against
the 60-second probe dropped Automation's 14 tools for most of each minute, so most model
calls started with a `tools` reorder.

**Skills stay through transient failures.** A visible or loaded skill's text stays in
the context on every call; only the same direct changes that change the tool list, or an
edit of the skill file, change it. A `load_skill` result is never shortened to the
tool-result preview (`crates/modules/context/src/retention.rs`), since the instructions
leave that skill out while its result is in the history. A skill file read that fails
mid-write reuses the last text read from that path (`catalog.rs` `last_skill_text`). An
ability turns absent only when two checks at least 2 seconds apart both miss it
(`catalog.rs` `present`). A failed or racing comparison of the live checkout keeps the
previous drift (`src/ability_drift.rs`). Optimization's guidance and the catalog's live
version follow its installed package, not its readiness marker. A failed own-skill read
retries and then pauses the run instead of sending a call without the agent's own skills.

**Cache timeout (built).** `cache_timeout_minutes` per model or provider in
`config/models/providers/<provider>.json` (60 minutes when absent), and
Identity → Context compaction → Cache timeout per agent (0 uses the model's).
The provider files are read live, so a change needs no rebuild. The OpenCode bridge
file sets 15 minutes for its models (commit 50191a3); Claude bridge models use the
60-minute default.

Measured lifetimes (2026-11-19). Each probe started its own harness session with a
unique ~30k-token message, resumed it after a set wait and read how much came back
from cache; a hit restarts the provider's clock, so a resumed session can be waited
on again. Claude Code writes every cache entry with the 1-hour lifetime (1,796 of
1,796 bridge calls), and Opus, Sonnet and Haiku hit 60 minutes after their last call
and missed at 61; a hit restarts the hour (each session hit again 60 minutes after a
58-minute hit).
OpenCode models differ widely: qwen3.8-flash about 5 minutes, glm-5.3 10-20,
big-pickle inconsistent (hits at 13 and 20, misses at 8 and 30), deepseek-v4.1-flash
on Go hit through 20 and sometimes at 45 and 62 but missed at 30 and 40,
deepseek-v4.1-flash-free hit at 57 and missed at 75, beam-free held to 62, and
kimi-k3 mostly reports no
usage. The OpenCode file sets per-model values from these (commit e636f9b):
qwen3.8-flash 5, glm-5.3 10, deepseek-v4.1-flash 20, deepseek-v4.1-flash-free and
beam-free 60; the rest keep the provider's 15. Real sessions cannot
measure this: every session of an agent starts with the same instructions and tool
definitions, so one session's call warms the cache for the others.

**Keep-alive (experimental, off by default; commits f2df9cf, 2f29738, 37d78ec,
34adcd1).** Turned on per agent under Identity → Context compaction: one expandable
"Keep cache warm (experimental)" row whose switch turns it on and whose panel holds
"Stop after (minutes)" and the four state switches. While it is on, each main model
call saves its request in the session owner's database
(`crates/modules/engine/src/runtime/keep_alive.rs`): `keep_alive:{sid}` (run, agent,
timeout, last call, last real call, a pending visit) and `keep_alive_request:{sid}`
(the request). Both hold `session_id`, so deleting the session erases them, and pings
carry on after a restart. A loop service checks every account's records every 30 s.
Two minutes before the cache timeout (at most half of it) it resends the saved
request with one short user question appended, so every earlier byte matches what
the provider cached; the answer and any tool call in it are dropped and nothing is
saved to the history. A bridge answers from a fork of the chat's harness session.
A fork shares the chat's cache only when its request starts the same way: Claude
forks run in the chat folder, and since commit 11f8e16 OpenCode forks do too (on a
separate fork server whose tool files stay in the chat's `.fork` folder), because
OpenCode puts the working folder in its system prompt. Before that an OpenCode ping,
and an OpenCode background summary fork, read none of the chat's cache and kept only
the fork's own copy warm. Measured after the fix: a new chat's first ping read 99.5%
from cache, and a real call 26 minutes after the chat's last real call, past
deepseek-v4.1-flash's ~20-minute lifetime with only pings between, read 99.6%. A
ping that fails because the bridge cannot fork the chat (no harness session yet, for
example after a stopped first turn, or a changed request) stops that session's pings.
A forced tool call was not used: on Anthropic a `tool_choice` change invalidates the
cached message blocks (Anthropic's prompt-caching documentation, "What invalidates
the cache"), and bridges do not pass it on.

Each session state has a switch: on by default for completed and for needs input or
an approval, off for stopped and for error (a run that failed, or a closing flag of
`failed`); a live run is skipped, since its own calls refresh the cache. Pings stop
at the limit (default and maximum 240 minutes after the last real call). Opening a
session in the app (`POST /api/v3/sessions/{id}/visit`, sent once per open, not on
reconnects) writes `keep_alive_visit:{sid}`; while the cache is still warm it earns
the next due ping even past the limit or in a state switched off. A dismissed flag
stops pings for good, a visit included. Nothing pings once the cache is already gone
(after a long restart, for example); stopping marks the timing record stopped and
empties the saved request. Each ping logs `keep-alive ping` with its input and
cached tokens and whether a visit earned it. Measured on the OpenCode bridge, pings
read 98-99% of the request from cache.

**Measurement (built, commits 9a6c1a6, eff85a9, d555f4a).** At each main model
call the engine fingerprints the tool definitions, instructions and each history
entry and compares them with the session's previous call
(`crates/modules/engine/src/runtime/cache_estimate.rs`, kept in memory for 256
sessions, so the first call after a restart has no comparison). It records the
unchanged leading bytes, the bytes sent and the first change in
`usage.cache_estimate`, and logs one `model call cache` line per call with the
provider's cached and cache-write tokens beside it. The composer's context
statistics panel shows both. The OpenCode bridge reads usage from its server's
session messages when `opencode run` prints no step totals. Each model call also
has an activity row ("Model call · 102.0k in · 99.7% cached"), and a reply's
activity group head sums them ("2 model calls · 49.9% cached"). That percentage is
the provider's reported cached tokens over its input tokens, as the bridge read them
(OpenCode's session messages, Claude Code's session log), so it shows 0% once the
cache has really expired. The estimate appears only in the row's copied details and
the context statistics panel. A bridge's figures describe the bridge's own request,
which adds its harness prompt and tools, so they can differ from the estimate.

## Session task list in context

The session task list (Task Progress ability) is one list per session, split into
sections of steps. This section is the whole path from storage to the model, for API
providers and for bridge harnesses.

### Data and tools

| Part | Where | What |
| --- | --- | --- |
| Rules | `crates/modules/chat/src/task_list.rs` | Sections (`s1`) and steps (`s1.2`) with server-assigned ids; statuses `open`, `in_progress`, `blocked`, `done`, `cancelled`; section notes, closing summary and plan flag; step outcome, evidence ids and `started_at`/`finished_at` (ms); one write adds or updates one section (`apply`), with the change lines and the sections it `created`, `started` and `closed` (`marks`); snapshot, step line and reminder texts |
| Storage | `crates/modules/chat/host/storage/task_list.rs` | Current list `base_tasks:{sid}` and every revision `base_tasks:{sid}:r{n}` in the session owner's user plane, erased with the session; revision-fenced writes with a receipt per tool call (`task_list_write`) or user request (`task_list_user_write`); the user's changes wait in the current document until delivered; the snapshot view `base_tasks:{sid}:view` |
| Tools | `abilities/Core/task_progress/tools/` | `task_list_read` (overview, one section, an old revision, or the changes between two) and `task_list_write` (one section and its steps, `expected_revision` required); both core and allowed for every role |
| Plan seeding | `set_execution_mode` | Leaving Plan with an approved plan adds a user-requested section from its checklist (plan flag set, no step started) and reports it as `task_list` in the result; the plan artifact's checklist follows that section's progress, from agent writes and panel edits |
| Panel | `ui/chat/controls/tasks-chip.js`, `GET/POST /api/v3/sessions/{sid}/tasks` | The Tasks chip (done/total) and panel with History; everyone who can view the chat reads it, anyone who can send edits it (observers read-only); every write adds a session event so open chats refresh |
| Texts | `config/configuration/prompts/task_list.json` | Snapshot heading and rule, empty text, step line, end reminder, unfiled reminder (with its `{calls}` count) and its `unfiled_after_calls` threshold (3), user change heading; read live, no compile |
| Skill | `abilities/Core/task_progress/skills/task_progress.skill.md` | When to keep the list and how; loaded by default (`task_progress.skill.json`), so it is in the instructions whenever the ability is on |

### What reaches the model

| Item | Placement | Lifetime | Changes when |
| --- | --- | --- | --- |
| Snapshot | Instructions, right after Main Notes (definition `session-tasks-{revision}`) | Frozen per `notes_basis` | A rewind, a merge or a major reorder rebuilds it from the current list |
| Agent writes | The agent's own `task_list_write` call and result in the history (new revision, change lines, `created`/`started`/`closed`) | Durable | Each write |
| User edits | A server-added `task_list_read-<id>` exchange (`changed_by: the user`, the revision and its change lines) at the end of the history before the next call | Durable, delivered once each | Each panel edit |
| Step line | Per-step guidance message: `Session tasks: s1 "Title", 2 open, active s1.2 "Label", next s1.3 "Label".` (each open section with unfinished steps) | One call, never saved | A step starts or finishes; it carries no revision |
| Unfiled requests | Per-step guidance message, after the step line: `Unfiled requests: u7 "first words", ...` (requests neither cited by a section or step nor set aside) | One call, never saved | A user request arrives, or the agent cites or sets one aside; the task gate refuses changes while any are unfiled (docs/tool-policy.md) |
| Gate section | A section the task gate opened reaches the history as a `task_list_read-<id>` row under `gate_change_heading` | Durable, delivered once | The gate opens a section because none was open |
| Unfiled reminder | Per-step guidance message, after the step line | One call, never saved | `unfiled_after_calls` (3) tool calls since the user's last message with no `task_list_write` among them; its text carries the count, so it changes every call |
| End reminder | One extra call: the history (ending with the draft reply) plus a user message naming the open steps and unfiled requests | One call, never saved | The first final reply of a run while open sections have open or in-progress steps, or requests are unfiled |

The list belongs to the session: every participant's runs get the snapshot, step line and
reminders, and the agent's tool calls write the session owner's list.

**End-of-turn reminder.** When the agent's final reply leaves open steps, the run saves
`task_reminder` (`draft`, `message`, `pending: true`) and makes one more call instead of
publishing. The run is reloaded before a response is handled, so the response is recognised
by the saved `pending` flag, which it clears. An answer that calls no tool is dropped and
the draft publishes (the closer, when enabled, then runs on the draft); an answer that calls
tools continues the run, and its later final reply publishes. It fires at most once per run.

**Compaction.** A fold keeps every task list change from its range and the verbatim tail
(agent writes and user-edit rows) as one combined `task_list_read` exchange among its
retained rows, after the Main Notes one; summaries leave them out. A merge or major reorder
drops them and rebuilds the snapshot. With Identity's **Fold boundary** set to tasks
(`compaction_thresholds.fold_tasks_retained`, 0 to 9; absent is the verbatim limit), a task
starting (its first step begins) folds, in the background, the task `retained + 1` behind it
and everything before; `plan_tasks` cuts at the first user message after that task closed,
never past the turn that created the oldest task kept, so a task still being discussed or
planned keeps its predecessor verbatim. The trigger and ceiling still fold by size. Panel
edits and plan seeding carry no marks.

**Agent fold at a task start.** In any fold mode, the `task_list_write` that starts a
section's first step may set `fold_earlier` (and `fold_focus`): the agent judges the earlier
work unrelated. `task_fold_cut` cuts at the first user message after the last closed
section, never past the turn that created the new one, and the tool stores a
`compaction_request` naming that message; the next model call summarizes it in the
background like `/compact`, with no user approval. The result's `fold` says whether it was
requested. While a section with steps waits to start after a closed one, the step line adds
`fold_hint`. This replaced the `compact_context` Compact/Keep panel; automatic compaction
stays the fallback when the agent keeps too much.

### API provider profiles

The host assembles one request per call and the provider profile places it
(**Provider-specific projection**): OpenAI subscription sends Responses items with the
instructions, snapshot included, in the developer role; DeepSeek and other compatible chat
endpoints send chat completions with them as leading system messages.

- The per-step guidance message, with the step line and the unfiled reminder, is appended
  after the history in the profile's instruction role for that call only, so the cached
  prefix ends just before it.
- The end reminder call replaces that message with the user-role reminder after the
  history, which already ends with the draft; nothing of it is saved, and the next call's
  history again ends where the draft left it.
- Agent writes and user-edit rows are ordinary function call and output items, so they
  extend the cached prefix like any tool exchange.
- The snapshot changes the instructions only at a rewind, merge or major reorder, which
  rewrite the start of the request anyway.

### Bridge harnesses (Claude, OpenCode, Muse)

The server sends bridge calls the same assembled request as a chat-completions body; the
paired client (`ui/shared/models/harness-bridge.py`) turns it into prompts for its own
harness session, which it resumes while the history only grows.

- The snapshot travels in the instructions; on a resumed session a changed instruction text
  is sent to the harness as a text diff.
- The per-step guidance message is stripped from the recorded history and re-sent only when
  there is a new user message, its text differs from the last copy, or the session is fresh;
  each copy stays in the harness transcript. Because the step line has no revision and
  changes only when a step starts or finishes, copies appear only at those points; the
  unfiled reminder carries its call count, so a copy goes with every call while it applies.
- Agent writes and user-edit rows reach the harness as new items rendered as text; the
  harness model sees ids on tool results (`[tool result <id>]`) but not on its own calls, so
  `evidence` should name result ids. Step times record the evidence window without ids.
- The end reminder call reaches the client as an ordinary call whose new item is the
  reminder message, so the harness records it; the next call's history lacks it, the client
  sees a changed prefix and starts a fresh harness session.
- A fold changes the history prefix, so the next call starts a fresh harness session with
  the compacted history (or a fork of a checkpoint session where the harness supports it).
- Every WebAgent tool reaches the harness model as a full native tool definition, so it can
  call any listed tool and load its skill by name. OpenCode runs its tool server with code
  mode off (`"codemode": false` in `opencode_config`); Claude runs with tool search off
  (`ENABLE_TOOL_SEARCH=false`, `CLAUDE_ENV`), which otherwise shows the model only deferred
  tool names. Each call writes the server's tool list to the chat's `tools.json`: a Claude
  call starts a new process whose tool server reads it, and OpenCode restarts its tool
  server when the file's digest changes, so an added or removed tool reaches a resumed
  session at its next call. The tool definitions lead the provider's request, so such a
  change costs that call its cached prefix; the tool list changes only on a direct change
  (see **What the tool definitions depend on**).

## Provider-specific projection

provider-projection emits bounded references into host-supplied items. The shared
contract validates coverage, ordering and identity; the host reconstructs content
and retains endpoint, credential, transport and publication authority.

| Provider profile | Placement and replay |
| --- | --- |
| OpenAI subscription | Responses items, developer instruction placement, subscription session cache key |
| DeepSeek | Chat-completions system placement, implicit prefix caching, original DeepSeek reasoning replay |
| Other installed compatible chat endpoints | Their existing chat-completions prefix profile, without foreign reasoning fields |
| Typesafe | Decision profile; ordinary chat projection is rejected |

These are installed protocol profiles in crates/provider-contracts, not
model-selectable formatting or permission overrides. No new endpoint is enabled
by a component result.

## Drop-in components and configuration

| Component | Shared contract / implementation / service | Operation |
| --- | --- | --- |
| context-assets | assets-contracts / context-assets / assets-service | assets.plan v1 |
| capability-context | capability-contracts / capability-context / capability-service | capability.prepare v1 |
| provider-projection | provider-contracts / provider-projection / provider-service | provider.project v1 |

Crate package names have the webagent- prefix. Each descriptor under
plugins/engine declares the corresponding service, pure implementation, contract,
engine-service transport, Cargo.toml and Cargo.lock as build inputs. Each has no
runtime component dependency. Limits are 16 MiB request and response, 30 seconds,
and one concurrent request per service. The private-pipe limit includes framing.
The provider contract currently allows a payload up to the same 16 MiB value,
so envelope overhead creates a small linked/live discrepancy near that ceiling.
The review found this boundary mismatch; exact maximum-size equivalence is not
claimed and needs a separate functional correction. A service generation is reused; each
ability does not require a new host connection.

Default linked-context builds call implementation libraries directly. Live builds
disable defaults and enable live-context, resolving independently sealed service
artifacts. Compatible implementation replacements use the engine publication
lane. Contract/registration changes require coordinated consumer validation.
Implementation versions are 0.1.1; the context-assembly interface is version 2.

Runtime data-root config/engine-components.json owns revisioned availability;
source config/engine-components.json initializes only an absent file. The
authenticated engine-component controls fence runtime and catalog revisions.
Deliberate withdrawal pauses accepted work at the relevant component boundary,
without a linked fallback. Restoration rechecks authorization and resumes only
eligible host-marked component pauses. Manual pause, cancellation and uncertainty
remain stopped.

## Verified release evidence and limits

Main release 8a7c2d23-c936-4471-bbeb-d4aa8624aa83 is committed as
4a8a1c558927a457b3ee3cad9548dda3db7e3719. Schema correction
ef29ca27-0a2e-43c9-a74d-a453ff53d1cd is committed as
4b02cf8cf99a7566d04250629550a8d51e57117f. Independent replacement
94bbf01f-d3b1-443e-82b0-d4477b6c57aa is committed as
014c9481787a7455c6a641471bd54456b1226afb.

Final release be5e2765-66c3-4c12-b88c-6108b6475d3d, commit
69389a4942ee9ed40ac84b508d02bf4c9df0c09c, passed 941 tests with five ignored,
both early feature profiles and optimized linked/live proofs. All six service
artifacts were reused. API/loop/gate health, exact source promotion, live prompt
refresh, conversation retention, compaction, unload, per-component pause/resume,
and manual/cancel negative controls passed. Separate descriptor withdrawal and
restoration passed for each new component. No production or v2 change is claimed.

Exact receipts and API proofs are under
build-jobs/coordination/checks/0aff5b55dbbd9e373305015ef269e15defce4a2cdeed1d9997d330640c824c1e/context-lifecycle/.
The full separately published code_verification skill was hash-verified unchanged.
An earlier transient session-creation HTTP 500 was reconciled through its
persisted request; its cause remains unconfirmed. Provider-wide live billing,
cache-hit and performance measurements are not claimed.

Documentation and index maintenance use a separate user-authorized activity
bound to immutable release/source identities. A current-source review does not
retroactively certify a different historical snapshot. Keep implementation tasks
closed and use the supported maintenance activity API for release indexing and
review claims. See engine-modularization.md for the extraction ledger.

## Exact duplicate tool results

Every call executes and saves its complete original output. Suppression changes
only model-facing context. Match the same tool, canonical JSON inputs and
host-resolved session/agent/requester/workspace scope, then compare complete
original result text exactly. Object keys are canonicalized for input identity;
result timestamps, array ordering, domain IDs and embedded result metadata are
not normalized. Interaction-row metadata and transport correlation are separate.
Hashes aid lookup; exact equality and host validation remain mandatory.

Unrelated calls never evict earlier results. Changed results and errors reset
only their matching sequence; errors stay full. The first full result remains
the primary until compaction. Later successful exact repeats become references
only when their serialized replacement is smaller. Recorded call order controls
selection even in parallel batches. Assignments persist across turns, restart and
provider changes. Toggle/allowlist/template edits affect future observations;
previously emitted placeholders stay stable until compaction.

Successful compaction promotes the latest surviving occurrence in each unchanged
sequence to full. Earlier survivors may reference it forward, but every reference
points directly to a full result in that assembled context. Folded changes/errors
remain reset barriers. The host reconstructs and verifies authorized originals,
expands summary evidence, and pauses if provenance cannot be verified. Recall
returns stored original output; it never moves primaries or rewrites placeholders.
Ordinary preview/envelope limits remain separate; no provider cache-hit, billed
cost or latency improvement is claimed by these correctness tests.

### Controls and required live prompt

Identity's **Suppress identical tool results** defaults enabled. Each tool's
Abilities panel independently includes/excludes it through **Suppress identical
results**. Default inclusion is file_read, file_list and file_search; all other
tools default excluded. Inclusion grants no
permission and never skips execution. Lifetime/budget eviction remains separately
preserved as **Under development · Experimental — disabled**; effective forces
that policy off while retaining saved settings.

Identity uses /api/v3/models/agents/{id}/identity with revision. Tool inclusion
uses /api/v3/agents/{id}/abilities/{ability}/tools/{tool}/result-deduplication with
expected_revision and included. Manager authority, tenant scope, catalog identity
and stale-write rejection remain host checks. Agent tool_result_deduplication
policy and tool_result_deduplication_tools overrides have separate persisted
namespaces. The ability API projects the tool map as tool_result_deduplication.

Config: config/configuration/prompts/tool_result_deduplication/placeholder.md
ships the required value. dedup_template reads the live file at each
model-call boundary; it does not consult
embedded defaults or alternate text. Supported variables are {current_call_id},
{reference_call_id} and {tool}. Empty text, malformed braces, unsupported variables
and templates over 8192 bytes are rejected. Prompt files are bounded to 64 KiB each, edited
in place and need no Rust compilation. Enabled suppression with missing/invalid configuration
pauses before model dispatch. New placeholders capture current validated text;
compaction can use captured assignment configuration when policy is later off.
That captured configuration is not a hardcoded/alternate-template fallback.

### Planner and host connections

| Functions / source | Purpose and boundaries |
| --- | --- |
| context-contracts/tool_results::canonical, key, fingerprint | Canonical input/scope identity and exact-text fingerprints; never normalize result content. |
| validate_template, replacement | Validate required syntax, substitute variables once and compare serialized sizes. |
| context-assembly/tool_results::project; contracts::validate_plan | Plan and independently validate sequence resets, stable prefixes, bounded state and direct full-result references. |
| tool_output::dedup_policy, dedup_included, dedup_template | Resolve fresh host-owned policy, per-tool inclusion and mandatory live text without granting tool authority. |
| dedup_scope, record_dedup_origin | Capture existing call/run identity, scope and original hash before pending state is cleared; native names come from recorded calls. |
| dedup_prepare | Recover authorized journal originals, reject ambiguous/mismatched provenance, order observations, invoke linked/live planner, validate and persist through run fences. |
| dedup_summary_source, dedup_config_fingerprint | Expand original summary evidence and fence changed policy/template/inclusion at model dispatch. |
| pause_dedup | Preserve input; use a restorable component marker only for unavailable context-assembly, otherwise report projection/configuration error. |
| storage/activity::tool_result_origin, tool_result, tool_call_record | Resolve session-scoped immutable output and native call identity; reject ambiguity. |
| history/session storage and Base recall_adapter | Carry projection state between turns, mark compaction/history rebase and recall original output without reference mutation. |

The planner uses tool_results contract version 1, the webagent-context-assembly
library and existing webagent-context-service prepare envelope. Stable builds link
the library; live builds reuse the confined component generation. It adds no
per-ability connection. context-assembly remains context.prepare interface 2,
implementation 0.2.1. Its catalog declares service/implementation/contracts, Cargo
inputs and context-compaction config as build dependencies, no runtime component
dependencies, one concurrent request, 16 MiB framed request/response and 30 seconds.
Configuration, credentials, permissions, scope, encrypted persistence, original
reconstruction, final validation and run authority remain in the host.

## RPC execution metadata

Worker protocol 2 returns an outer version/id/result envelope. The worker checks
version before execution; the host checks version/correlation before accepting
result. Payload domain IDs, nested metadata and timestamps stay in returned
content. The protected build host keeps its existing flat protocol: host_reply
removes only its known outer routing id after correlation validation, never nested
job/release/domain IDs or arbitrary fields named id.

Existing model tool-call IDs still bind placeholders, results and recall. Existing
interaction sequences still identify durable private activity (interaction:{seq}).
RPC IDs correlate wire exchanges only; no new logical execution ID is introduced.
Version-1 execution Metadata stores rpc_calls for fixed peers tool-worker/build-host,
with at most 256 entries and nonempty control-free IDs up to 128 bytes.

execution::worker_reply/host_reply decode their explicit contracts. Host
outcome::capture creates task-local evidence for each independently polled
dispatch; rpc records verified exchanges and stage binds them to existing pending
call_id outside run.input. Outcome Deref/Display expose only content. Foreground,
ordered batch and durable background paths retain content/evidence separately.
Store::attach_execution_metadata validates and encrypts nullable
interactions.execution_metadata in the original result transaction, scoped to
session/run/step and fenced against immutable conflicts. Staging clears only
after commit. Recall reads the original interaction item. Authenticated private
activity exposes metadata beside payload.output; background metadata remains
session-fenced and withholds arguments/results. Public chat is not hydrated from
it, and old history is not rewritten/backfilled.

## Duplicate-result acceptance and historical pause

Initial feature publication b550e68d-325f-484f-b9d2-5f9e867fc943, native provenance
correction a27811a9-a4da-478b-97f4-477717f9cfa8 and settings correction
983a9d77-6eff-497a-a105-eb74c4fa5be5 / f2103ed36905882b6f2b1bd1fb5317b4a17ca686
have separate historical receipts. Do not rewrite their then-pending acceptance.

RPC/acceptance release 17ee81ba-d997-4a1c-9c10-69a7fc235470, commit
55a36e852ce19c72f5d9e9c477a8654ad513cc62, passed 987 workspace tests, five
existing ignored tests, both early profiles and optimized linked/live checks.
Under build-jobs/checks/tool-result-rpc-successor-20261019/:

- live-acceptance.json proves distinct routing evidence with identical stored
  content, duplicate references, fresh compaction promotion and original recall.
- live-lifecycle-acceptance.json proves three retained inputs, one eligible
  restoration, preserved manual pause/cancel and independent process replacement
  with unchanged core containers.
- release-commit.json and closure.json bind promoted bytes, commit, health,
  completed implementation and zero claims.
- post-compaction-reconciliation-20261019.json records the historical distinction.

Old run 0f607451-2374-5e8c-dcff-0eaea9295ccc in isolated session
bb6572e7-7a12-48fd-a51a-6143fa495434 paused with component_unavailable after one
compaction while component metadata reported available. Its cause remains
unconfirmed. It did not recur in fresh acceptance; it was not reproduced and
proven fixed, and there is no evidence assigning its disappearance to RPC ID
separation. The old fixture/history remain manually paused, with no component
restoration marker. Acceptance is complete; the unexplained historical anomaly
is not a currently reproduced defect. Do not resume it or reopen implementation
ownership for maintenance. [Session creation recovery](session-creation.md) is a
separately verified host fix and does not establish this pause's cause.
