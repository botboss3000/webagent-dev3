# List skill guidance

Use `list_skills` with one exact `ability_id` to list permitted discoverable guidance needing a load. The Rust dispatcher supplies the caller, session, agent and current run. No user, session or filesystem path argument is accepted. For Base, the catalog also includes this agent's saved selectable skills with kind `own`.

Optionally pass a short query with task words to filter skill IDs and summaries. Set `include_available=true` to include Visible and already loaded guidance; these entries need no load. Without that flag, already supplied entries are omitted.

The response includes exact `kind`, `skill_id`, digest and byte count, availability and whether loading is needed, plus a live `catalog_version`. Pass the selector IDs and optionally that version to `load_skill`. A load returns acknowledgment and metadata; full guidance is supplied separately in the next model context.

Denied, suppressed, disabled, uninstalled or changed assets are unavailable. Listing does not activate abilities or grant execution. Choose individual discoverable guidance when needed; ability, tool and skill settings remain independent.
