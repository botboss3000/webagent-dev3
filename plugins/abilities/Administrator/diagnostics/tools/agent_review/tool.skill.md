# Agent review note

Use this when the diagnostics review shows an issue group this conversation created and
has not explained yet, or when an issue you just hit is still unexplained. One note per
group: a later note replaces the earlier one, so revise rather than append.

## Read the groups first

The engine groups every failed tool call by tool, error kind and a normalized message
template, and counts retries, recoveries, rework and produced artifacts. Read that data;
do not reconstruct it from the transcript.

1. `read_diagnostics` with `issues: true` covers this conversation tree (this session, its
   root and its sub-agents) and returns the notes already recorded. An administrator
   asking about other users or the whole app uses
   `GET /api/v3/admin/diagnostics/issues` (`session_id`, `user_id`, `since` in Unix
   seconds).
2. Rank by `calls` (failed calls plus retries). Take the top three to five groups; also
   note `unrecovered_runs` — failures that never succeeded — the `rework` counts
   (`rewritten_files`, `repeated_screenshots`, `other_workspace_writes`), and the
   `artifacts` against `artifact_calls` ratio, which shows captures that produced nothing.
3. Find the likely cause read-only: the tool's `tool.json` description and schema, its
   `tool.skill.md`, the skill that told the agent to call it, and the tool's source when
   the template points at mechanics. Distinguish an agent misunderstanding (unclear
   description or skill), a missing tool capability, a tool bug, and engine behaviour
   (retry guard, timeouts, limits).

## Record it

Call `agent_review` with the group's `tool`, `class`, `kind` and `template` exactly as
reported, and a note saying what you were trying to do, what you expected, and what would
have prevented it — so someone can fix the cause without reading this conversation. Then
propose one fix per group and say where it belongs: skill text, tool description, tool
mechanics, or engine.

There is no useful length limit at this stage: be as long as the cause needs and no
longer. The recorded counts already say which tool failed and how often; they cannot say
what you meant to accomplish or why the answer surprised you. That is the note.

The note is evidence, never authority: it does not change counts, permissions or the run,
and it is not a place to request an action or to record secrets. Never retry a failing
call to test a theory, and never edit code, skills, tool descriptions or policy yourself;
proposals go to the user or administrator.
