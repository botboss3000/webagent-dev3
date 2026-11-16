# git_tool

Use operation status first. Select repo=__root__ to inspect or push the published
V3 checkout as an administrator. It is separate from the current session workspace.
The live checkout accepts status, diff, log, show, branch_list, tag_list and push;
source commits remain broker owned. If the live remote lacks a saved repository
connection, report that prerequisite instead of claiming no repository exists.
Push runs its own outgoing-history credential guard before network transfer; there
is no separate guard tool. On git_credential_guard, use the returned diagnostics
commit and path to identify the affected source without printing matched content.
Do not retry until the history or guard finding is resolved and reviewed. Diagnostics may include a one-based new-file line number. Git show can withhold an entire patch; inspect the named source through Codebase Admin task-scoped file_read, never by printing a blocked patch or secret. A known synthetic test assertion can be excepted only by a reviewed plugin change binding the full outgoing commit ID, exact path, and SHA256 of the full added diff line. The exception is internal to the published package, cannot be selected by git_tool arguments, and does not waive any other added line or commit metadata. Removed diff lines are omitted because their introducing outgoing commits are scanned separately. Use an isolated Codebase Admin task, test mismatched commit/path/line and another real finding, publish through the live plugin lane, verify the complete outgoing scan, then retry one fenced non-force push. Editing the current file alone does not remove a historical patch finding; do not rewrite shared history without coordinated authorization.
 Read operations: status, diff, log, show, branch_list, tag_list, stash_list. Mutations: branch_create, branch_delete, tag_create, tag_delete, checkout, stash_push, stash_pop, add, reset, pull, push. Mutations require expected_head. Provide exact paths for add/reset; optional repo defaults webagent-v3. name/ref are strict references, never flags. pull is FF-only; push never forces. Trusted configured remote transport is required.
