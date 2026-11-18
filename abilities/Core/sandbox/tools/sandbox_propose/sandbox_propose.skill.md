# sandbox_propose

## Usage

- Propose `<folder>/index.html` from this conversation's sandbox as a saved UI element of this agent, kept for this user across all their conversations with the agent.
- Nothing is saved until the user approves it on the agent's Artifacts page; tell them it is waiting and give the `review` route from the result.

Pass `folder` (relative to the sandbox, for example `toggle`), `name` (1–40 lowercase letters, digits, `-`, `_`), `title` (up to 80 characters) and an optional `description` (up to 300). A new proposal with the same name replaces your earlier pending one; approving it replaces the saved element of that name and raises its revision.

An element is one self-contained `index.html` (size limit in the ability's `elements.json`): inline `<style>` and `<script>` only. It runs in a sandboxed frame with no network, no app access and no other files, so links to other files, images by URL, `fetch` and storage do not work; use inline SVG or `data:` images. See the Sandbox skill for the bridge an element uses to talk to the chat.
