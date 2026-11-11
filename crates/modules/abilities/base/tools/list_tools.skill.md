# List permitted tools

Use `list_tools` to list permitted discoverable tools needing selection, best matches first. Pass task words as `query` (each word matches on its own), or `group` with a group ID from the tool index; set `include_available=true` to also inspect Visible and selected tools. Returned metadata includes exact tool name, description, owning ability, availability and approval requirement.

Call `load_tool` with an exact returned name to select it for this conversation, agent and authenticated requester. Current schemas and applicable guidance are supplied separately in model context. Listing never selects a tool, grants permission or changes approval requirements. Off, unavailable and denied tools stay excluded under current live authority. Use `context_assets` to inspect or unload existing dynamic selectors.
