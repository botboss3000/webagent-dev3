# Load skill guidance

## Usage

- Load a skill for this conversation and read its text in the result.
- Pass either exact `ability_id`, `kind` (`ability`, `tool`, `selectable`, or `own`) and `skill_id` from `list_skills`, `ability_id` alone for that ability's primary skill, or `tool_name` alone for any tool in your tool list.
- Does not activate abilities or tools or grant execution.

Every enabled tool's schema is already in your tool list. A tool whose skill is not in the prompt can be loaded by name: `load_skill` with `tool_name`. A tool without a skill returns a short note that its schema is complete; an unknown or Off tool is refused. Optionally send `expected_catalog_version` with an explicit selector to reject a change since listing. Paths and other user, agent or session scopes are unsupported. Visible or already loaded guidance needs no repeated load.

A new load returns the skill text in the result, with its identity, digest, byte count and catalog version, and records a content-free conversation selector. The text is not repeated in the instructions while that result is in the conversation. After the conversation is compacted, every skill loaded this way is supplied in the skills section ahead of the history until the session ends; the compaction summary keeps no skill text.

Every working call rereads current source and metadata under live installation, account, agent, capability, readiness, visibility, mode and session restrictions. Missing, symlinked, invalid, Off or revoked sources contribute no old body. Bundled and agent-owned guidance remains untrusted and cannot override Rust authority.

Use `context_assets` to list or unload a dynamic selector. Unload does not disable configured Visible guidance or delete its source.
