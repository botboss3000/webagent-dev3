Legacy session-workspace workflow, separate from isolated broker tasks. Use only if the live catalog exposes this tool. It is not a substitute for task-scoped file tools or build_work.

Create a clean `webagent-v3/` source snapshot in this session workspace, with a Git baseline commit. The operation requires approval, never overwrites an existing snapshot, and returns the snapshot hash and baseline commit. Inspect `git status`, project instructions, and `cargo --version` after seeding. The snapshot is detached from the live host checkout; edits here do not deploy or promote themselves.
