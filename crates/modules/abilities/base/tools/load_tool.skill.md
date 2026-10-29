# Load one tool

Use `list_tools` to find an exact permitted tool name, then call `load_tool(tool_name="...")`. No Decision model is required. Visible or already selected tools return an acknowledgment without changing selection.

A new successful selection lasts for this conversation, selected agent and authenticated requester. Rust supplies the current authorized schema and applicable current guidance in each working model call. Its identity survives normal messages, compaction and restart until unloaded with `context_assets`. Source edits and schema replacements are resolved live; an unavailable or revoked tool is withdrawn without reviving an old schema.

Selection changes exposure only. It never grants permission, overrides Off or suppression, inherits approvals, or starts execution. Installation, account, agent, capability, session and approval checks remain authoritative on every call. Tool discovery stays foreground, stop-at-run-end and concurrency one; call it alone.
