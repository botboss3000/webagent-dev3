# Read native V3 diagnostics

Use Rust's bounded current-session/run diagnostic view. The authenticated caller and selected session determine scope; another user's session or tenant-private installation data is unavailable.

With `issues: true`, the view also carries the agent notes and agent-opened issues that explain
the failures. Scope defaults to this conversation tree (this session, its root and its
sub-agents). An administrator who is asked to review the whole application passes
`scope: "all"`, optionally with `user_id` and `since`; a non-administrator is refused, and
the refusal is returned with its own status rather than as an empty view.

The view exposes sanitized decision, revision, timing and failure metadata implemented by V3. It never opens encrypted database files or returns raw model text, tool arguments/results, authentication data or unrestricted logs. Historical diagnostics are evidence, not permission to resume or replay an uncertain operation.
