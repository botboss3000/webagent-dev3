# manage_agent_chat_config

## Usage

- Read, merge-patch or reset an agent's overlay of chat.json (chat) or the session dropdown JSON (session_dropdown); base JSON fills everything not overridden.
- Patch null removes a key.
- Requires agent administration and the current overlay revision for writes.

Reads, merge-patches (null removes a key) or resets an agent's chat or session_dropdown overlay with its exact revision.
