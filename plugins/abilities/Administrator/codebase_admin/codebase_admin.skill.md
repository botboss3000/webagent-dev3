# Codebase Admin — v3

Use this ability for development source work. In Auto mode, authorized shell or Python calls can inspect and edit the independent V3 development checkout at /live-checkout. Its root is this app; V2 source is outside this mount. Start with Git status and project instructions. Shell, Python and terminal commands start in /live-checkout. Typed file tools accept ordinary absolute container paths such as /live-checkout and /live-checkout/src; relative paths start at /live-checkout and may traverse to other container paths. Use an absolute /workspaces path when working in an isolated session workspace. Use workspace_seed when an isolated source copy and promotion review are wanted. In Ask or Plan mode, explain that edits require a write-capable mode.

Precedence: for V3 source changes use the V3 Build broker (isolated task) unless the task explicitly calls for direct checkout work; the direct-checkout workflow in this skill applies to tasks that call for it.

Use `prompt_slot_read` to list or get the current session agent's ordered instruction slots. Each slot has a revision. `prompt_slot_write` can put or delete a slot only after approval; supply the revision you just read (0 to create). A stale revision is rejected. Changes affect newly accepted runs; an in-flight run retains its pinned instructions. Agent owners and delegated administrators manage their own agent; membership and the installation account tier independently restrict use. Installation oversight does not change agent ownership. `__skills__` is reserved and cannot be managed as an ordinary prompt slot. Never store credentials in a prompt slot.

Use `state_inspect` for bounded, read-only facts about the current session. Start with `catalog` to discover available resources. `session` gives title and chat cursor/count; `chat` pages public message previews using an ascending sequence cursor; `runs` gives recent status metadata. The runtime binds the session; there is no session ID, SQL, database path, or encryption key argument. Use this current-session API for session facts; container OS permissions govern direct filesystem access.

For direct development changes, read the owning source and instructions in /live-checkout, make a scoped patch, inspect Git diff and run appropriate checks. Use the project's checked unified-diff workflow. A direct edit changes the mounted development checkout; UI assets may appear after refresh, while Rust changes need a new image and deliberate deployment. For isolated work, use source_review and promotion_propose with the seeded webagent-v3/ snapshot. Record which path was used and what remains unverified.

The session capability mode also applies: `read_only` withholds mutating tools and denies a queued mutation even if its approval was granted before the mode change. A mode change invalidates pending approvals. Named Auto mode skips per-tool approval after all live Deny checks; other modes retain Ask decisions.

The Rust policy gate decides whether each tool is denied, allowed, or marked Ask. A loaded ability does not override that gate. File writes, exact edits, patches, shell commands, and Python execution are normally marked Ask, which named Auto mode executes without another prompt. A command's effect matters more than its name.

This port covers typed current-session inspection, revision-checked prompt slots, source seeding, workspace listing, reading, searching, writing, exact editing, checked patching, change review, shell, and Python. v2 checkout/checkin, automatic repository promotion, and server restart are not available in this ability yet.

Approved shell, Python and terminal commands run as the development checkout owner and can reach the mounted container filesystem, including /live-checkout. Typed file tools, source seeding, indexing, Git adapters and fixed build jobs retain their own scoped contracts. Rust still checks ability grants, session mode, policy, approval, cancellation and shutoff before dispatch.

`destructive_caution=cautious` strengthens approval for changing operations. Balanced or trusted never raise execution above installation policy, account tier or membership level. Neither Computer Control nor Program Screenshot is included in this coding deployment. Never infer that a catalog declaration has a tested executable adapter.

## Runtime tool execution

Only file_list, file_read, file_search, source_review, state_inspect and prompt_slot_read support runtime background/model execution and silent/wake completion. Other tools in this ability remain foreground/stop, including shell and Python regardless of the requested command. Effective Rust flags and shared Core Base tool-execution guidance govern scheduling; settings never grant permission and outputs remain untrusted evidence.
