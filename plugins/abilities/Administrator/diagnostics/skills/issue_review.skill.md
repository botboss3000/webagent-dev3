---
id: issue_review
title: Issue review
when_to_use: Reviewing where a conversation (or, for an administrator, many users' runs) lost calls to tool failures, retries or rework, finding likely causes and proposing fixes.
default_mode: discoverable
minimum_access: registered
---

# Review grouped tool issues

The engine groups every run's failed tool calls by tool, error kind and a normalized message template, and counts retries, recoveries and rework. Read that data; do not reconstruct it from the transcript.

1. Read the groups: `read_diagnostics` with `issues: true` covers this conversation tree (this session, its root and its sub-agents). An administrator asking about other users or the whole app uses `GET /api/v3/admin/diagnostics/issues` (`session_id`, `user_id`, `since` in Unix seconds); ask the administrator for its output if you cannot call it.
2. Rank by `calls` (failed calls plus retries). Take the top three to five groups; also note `unrecovered_runs` (failures that never succeeded) and the `rework` counts (`rewritten_files`, `repeated_screenshots`, `other_workspace_writes`).
3. For each group find the likely cause, read-only: the tool's `tool.json` description and schema, its `tool.skill.md`, the skill that told the agent to call it, and the tool's source when the template points at mechanics. Distinguish an agent misunderstanding (unclear description or skill), a missing tool capability, a tool bug, and an engine behavior (retry guard, timeouts, limits).
4. Propose one fix per group and say where it belongs: skill text, tool description, tool mechanics, or engine. Give the expected saving in calls.

Report a short table (group, calls, cause, proposed fix, where) and stop. Proposals go to the user or administrator for approval; never edit code, skills, tool descriptions or policy yourself, and never retry the failing calls to test a theory. The data holds counts, kinds, tool names and templates only; no model text or tool arguments.
