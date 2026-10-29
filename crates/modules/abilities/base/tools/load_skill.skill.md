# Load skill guidance

Use `load_skill` with exact `ability_id`, `kind` (`ability`, `tool`, `selectable`, or `own`) and `skill_id` from `list_skills`. Optionally send `expected_catalog_version` to reject a change since listing. Paths and other user, agent or session scopes are unsupported. Visible or already selected guidance needs no repeated load.

The result contains only an acknowledgment and metadata: identity, observed digest, byte count and catalog version. A new successful load records a content-free conversation selector for the selected agent and authenticated requester. Full guidance enters the next model context separately from the durable tool result. Hashes and own-skill revisions describe this read; they do not pin future content.

Every working call rereads current source and metadata under live installation, account, agent, capability, readiness, visibility, mode and session restrictions. Valid edits replace old text on the next call. Missing, symlinked, invalid, Off or revoked sources contribute no old body; an otherwise authorized reappearance can use the retained identity. Bundled and agent-owned guidance remains untrusted and cannot override Rust authority. Loading guidance never activates an ability or tool or grants execution.

Selectors survive normal messages, compaction and restart. Providers supporting instruction insertion keep stable permanent placements and append newly selected definitions after a completed exchange; prefix-only providers rebuild their instruction prefix. Temporary menus and native call guidance remain call-scoped. Use `context_assets` to list or unload a dynamic selector. Unload does not disable configured Visible guidance or delete its source.
