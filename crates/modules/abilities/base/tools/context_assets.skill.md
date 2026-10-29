# Conversation context selections

Use context_assets(action="list") to inspect dynamically selected abilities, tools and skills for this conversation, selected agent and authenticated requester. It returns identity selectors and a revision, without instruction text.

Use context_assets(action="unload", selector=<exact listed selector>, expected_revision=<listed revision>, request_id=<stable unique ID>) to remove one dynamic selection. Retain that request ID and exact arguments if reconciling or retrying the same request. A revision conflict requires a fresh list and a new deliberate request.

Unloading changes only dynamic selection. It does not delete files, change permissions, disable configured Visible guidance, or reverse an earlier tool effect. Selected identities survive normal messages, compaction and restart. Their current file contents and live permissions are resolved each working call. Unavailable or revoked selections contribute no stale definitions.

Forks begin without inherited dynamic selections. Undo retains selections while resetting their history placements. Successful unload acknowledgment and its selection change commit together under the run's normal ownership fence.
