# Diagnostics — V3 run diagnostics

Rust owns live ability policy, approvals and sanitized run diagnostics. No tool accepts another caller, an authentication token, a database path or JavaScript. To look at the live app in a browser, use Browser Control: navigating to the app signs its browser in as the current user.

Use `read_diagnostics` for the native bounded current-session/run diagnostic view. It does not open encrypted databases or expose raw model text, tool arguments, credential records or unrestricted installation logs.

With `issues: true`, `read_diagnostics` returns the conversation tree's grouped tool-failure issues (tool, error kind, message template, retries, recoveries, rework and artifact counts) and the notes already recorded. The `agent_review` tool skill explains how to rank them, find causes, and record an explanation for the group you are fixing.

## Runtime tool execution

All tools in this ability remain runtime foreground/stop with concurrency 1; background/model and silent/wake are unsupported, even for read actions or calls returning asynchronous job receipts. Effective Rust flags and shared Core Base tool-execution guidance govern scheduling; settings never grant permission and outputs remain untrusted evidence.
