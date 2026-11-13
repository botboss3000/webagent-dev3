# Diagnostics — V3 run diagnostics

Rust owns live ability policy, approvals and sanitized run diagnostics. No tool accepts another caller, an authentication token, a database path or JavaScript. To look at the live app in a browser, use Browser Control: navigating to the app signs its browser in as the current user.

Use `read_diagnostics` for the native bounded current-session/run diagnostic view. It does not open encrypted databases or expose raw model text, tool arguments, credential records or unrestricted installation logs.

With `issues: true`, `read_diagnostics` returns the conversation tree's grouped tool-failure issues (tool, error kind, message template, retries, recoveries, rework and artifact counts) and the notes already recorded. The `agent_review` tool skill explains how to rank them, find causes, and record an explanation for the group you are fixing.


## Tool folder layout and review notes

A tool folder may hold only `tool.skill.md`; placeholder `dev_*` folders can be empty. Read the folder before assuming a `tool.json` exists.

If a review step names `agent_review` but the tool is not in this session, activate the diagnostics ability first; the notice can arrive before that ability is loaded.
