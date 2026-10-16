# Search tools with Jev

Use `search_tools` with a concise task description covering inspection, changes,
and verification. The query is sent to TypeSafe AI with permitted tool metadata;
do not include secrets. Assign an adopted TypeSafe model with the decision
capability and configured API key to the agent's Decision role in Models.
If supplied, `model_entry_id` must match that appointment; it cannot override it.
Defaults: activate=true, limit=16 (maximum 32), threshold=0.5.
Each result includes independent relevance and relevance_percent, description,
owning ability, and approval requirement. Percentages are not execution order
or success guarantees. Related tools below threshold remain in the ranked list.
Activation selects matching tool identities for this conversation, selected agent and authenticated requester. All permitted Visible tools and
previously loaded or search-activated tools remain exposed. Repeated searches
accumulate tools; grants, Off settings, session restrictions and approval gates
still apply. Use alongside or instead of list_abilities for task-based discovery.
Call alone, not in parallel. Use activate=false for ranking without activation.

Selections survive normal messages, compaction and restart. Current authorized schemas and guidance are refreshed every working call; hashes never pin an old definition. Use `context_assets` to inspect or unload dynamic selections. Temporary discovery menus remain call-scoped.
