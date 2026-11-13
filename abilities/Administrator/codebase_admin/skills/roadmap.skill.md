---
id: roadmap
title: Deferred work and design directions
when_to_use: Use before designing in tool policy, retry risk, file access, network egress, browser sessions, storage limits or plugin installation, to respect decided future directions.
default_mode: discoverable
examples: what is planned for the egress proxy; is per-user browser limits designed; tool audit plans
keywords: roadmap, future, deferred, plan
minimum_access: registered
---

# Deferred work and design directions

Decided directions the owner has parked for later releases. They are not
authorization to build them during unrelated work; follow them when the user
picks one up.

## Tool retry-risk framework (2026-11-03)

- Repeat boundary: a repeat matches only within one run. After the v2 task
  progress ability is ported, consider grouping by task instead.
- Agent-written approval wording at the approval gate, beside the fixed
  `tool.json` warning.
- Abilities page: show each tool's retry-risk level, agent hint and user warning.
- Tool audit UI: track repeat approvals/declines per tool to review risk levels.
- Policy change requests: a "Request policy change" button; requests appear in
  the tool audit with requester, note and an admin-set status. The change itself
  is an ordinary coding request editing `tool.json`.
- Reset by citing a fix: let an agent clear the repeat guard by naming the fix.
- Intent check: a small model judges whether an unasked write matches what the
  user asked (user messages only count as authority) and returns allow, ask or
  deny; failure or timeout asks; per-tool opt-in. Settle model, latency budget
  and audit counting first.
- Tool audit: demote audited tools, typed rejections naming the cause, per-tool
  match keys and automatic outcome checks.

## Tool reach and file access (2026-11-06)

File tools and command sandboxes reach only the session workspace and checkout
source; commands connect over TCP only to 80/443, their preview ports and the
ephemeral range. Deferred:

- Egress proxy: an in-app HTTP(S) proxy allowing hosts from an installation list,
  the only place commands may connect; also covers UDP and raw sockets. Build it
  with computer access.
- Installation config file: one JSON file of allowed folders and hosts,
  defaulting to app self-editing scope; computer access never widens that.
- Computer access as its own ability: off by default, approvals on, never
  reaches the app's `state/`.
- `/proc` visibility: fix with a `hidepid` mount, not a sandbox block.
- Drop the old `/live-checkout` seed prefix once nothing stored uses it.
- Python sandbox connect rules cost about 75 ms per command; move to the Rust
  runner or batch the rules.

## Browser sessions (2026-11-08)

Browser folders are temporarily capped at 100 (`MAX_PRIVATE_SESSIONS`). Target:
configurable running browsers per app (5) and per user (4) in a described JSON
setting, a queue at either limit, and folders deleted with their chat session.
Measure real memory before production.

## Storage limits (2026-11-09)

Near a storage limit: a review-and-trim process across everything the user owns,
with user-approved deletions. Its own session.

## Installed plugin packages (2026-11-11)

Diagnostics, optimization and MCP packages live under the checkout (Git-ignored,
root-owned binaries). Before production, move installed packages and their data
to an installed-plugins location; until then leave them.
