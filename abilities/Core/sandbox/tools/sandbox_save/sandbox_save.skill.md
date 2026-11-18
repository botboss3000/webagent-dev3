# sandbox_save

## Usage

- Save `<folder>/index.html` from this conversation's sandbox as one of this agent's UI elements for this user, kept across all their conversations with the agent.
- Needs approval like other approval-gated tools: in Ask mode the user approves it in the chat, in Auto mode it runs.

Pass `folder` (relative to the sandbox, for example `toggle`), `name` (1–40 lowercase letters, digits, `-`, `_`), `title` (up to 80 characters) and an optional `description` (up to 300); the user sees these on the approval. Saving the same name again replaces that element and raises its revision.

An element is one self-contained `index.html` (size limit in the ability's `elements.json`): inline `<style>` and `<script>` only. It runs in a sandboxed frame with no network, no app access and no other files, so links to other files, images by URL, `fetch` and storage do not work; use inline SVG or `data:` images. See the Sandbox skill for the bridge an element uses to talk to the chat.
