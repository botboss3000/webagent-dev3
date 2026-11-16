# List skill guidance

Use `list_skills` with one exact `ability_id` to list its permitted selectable skills (ability and selectable guidance and agent-owned skills). Tool skills are not listed: `load_skill` also takes any tool name from your tool list as `tool_name`. Rust supplies the caller, session, agent and run. No user, session or filesystem path argument is accepted. Base also lists this agent's private saved selectable skills with kind `own` when caller authority allows that record scope.

Pass a short task-word `query` such as "ui variants" or "add setting". Results are ranked, `best_match` first, with a `score`; a query always returns rows, includes guidance you already have (marked by availability), and marks poor matches `"match": "weak"`. Without a query, set `include_available=true` to include Visible and already selected guidance; otherwise already supplied entries are omitted.

The response includes exact kind and skill ID, observed digest and byte count, availability, load requirement and current catalog version. Pass the identities and optionally that version to `load_skill`. `load_skill` returns the skill text in its result. Source hashes and own revisions are observation receipts, not persistent pins.

Current valid edits are adopted without another selection. Missing, symlinked, invalid, denied, suppressed, Off or uninstalled sources are unavailable. Listing neither activates abilities nor grants execution. Ability, tool and individual skill settings remain independent.
