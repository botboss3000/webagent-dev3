# Sandbox

This conversation has its own scratch folder, the sandbox. Use it to write and test small
files, pages and scripts.

- `sandbox_file` reads, writes, lists and deletes files in it. Paths are relative to the
  sandbox (`element/index.html`).
- `sandbox_shell` runs a shell command there and `sandbox_python` runs Python there. Both
  start in the sandbox folder, which is also their home and temporary folder.

What the sandbox cannot do, by design:

- No network at all: no downloads, package installs, web requests or local services. Use
  only the tools and Python standard library already installed.
- Nothing outside the folder: no app source, settings, data, other conversations or
  `/proc`. Commands read only the system's program and library folders.
- Limits on every command: run time, memory, processes, file size and the folder's total
  size, which also counts toward the user's storage. The exact limits are in the ability's
  `limits.json`. A command that passes the folder limit is stopped; delete files to free
  space.

The folder belongs to this conversation only; other conversations cannot see it. Nothing in
it leaves the sandbox on its own.

When a command is refused or fails because of a limit, say which limit it was and work
within it; do not try to get around it.

## Saved UI elements

An element is a small self-contained page (`index.html` with inline style and script) that
this agent keeps for this user across all their conversations, and shows in a chat as a card
in the Artifacts chip.

1. Build it in a sandbox folder, for example `toggle/index.html`, and test its logic there.
2. `sandbox_save` it. Like other approval-gated tools it follows the session's execution
   mode: in Ask the user approves it in the chat, in Auto it is saved at once.
3. Once saved, `sandbox_present` places a card for it in any of the user's chats with this
   agent, with your title, a short description and optional `params`.

Saved elements are read-only in the sandbox at `saved/mine/<name>/` and `saved/shared/<name>/`.
To change one, copy it into your folder, edit and save it again. `sandbox_elements` lists
them.

An element runs in a sandboxed frame with no network and no app access. It talks to the chat
only through `window.webagent`, which the app adds:

- `webagent.params`: the card's `params` object; `webagent.card`: its title and description.
- `webagent.preview.status()`: a promise of `{variant}` for the card's preview session in
  this browser (`variant` is null when off). `webagent.preview.on(variant)` and
  `webagent.preview.off()` turn a variant of this chat's UI previews (see the UI proposals
  skill) on or off; the app then reloads the page in the same tab. A card whose `params`
  name `preview_session` (one of this chat's sub-agent sessions) previews that session's
  variants instead.
- `webagent.prompt(text)`: sends `text` as the user's next message after a short countdown
  the user can cancel. Call it only from a click in the element.
- `webagent.open(route)`: opens an allowed app page (listed in the ability's
  `elements.json`) in the same tab, after a click.
- `webagent.resize(height)`: asks for a card height in pixels (bounded by the app).

The app checks every request and ignores anything else. Keep elements small and readable on
a phone (360 px wide) and on desktop.
