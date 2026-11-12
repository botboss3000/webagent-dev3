---
id: ui_code_map
title: Finding UI code
when_to_use: Use before changing or debugging WebAgent UI behaviour, to find which file owns an element, gesture, focus or keyboard behaviour, and its tests.
default_mode: discoverable
examples: the text area loses focus when I tap a button; keep the keyboard open while scrolling the row below the composer; fix how the chat panel scrolls; where is the code for this chat control
keywords: ui code, chat panel, composer, keyboard, focus, gesture, scroll
minimum_access: registered
---

# Finding UI code

Read from the live checkout first (`file_search`, `file_read`; paths are relative to
`/code`), then edit through your broker task. Search; never guess a file.

1. Start from what the user sees. Find the element in the page markup
   (`ui/chat/chat.html` for the chat panel, `ui/index.html` for the shell) and note its
   `id` or class.
2. `file_search` that id or class under `ui/` to find the modules that wire it.
3. Many modules start with an `@code-index` comment naming a feature, aliases and
   purpose: `file_search` query `@code-index` with a word from the request (for example
   `footer scroll`) under `ui/chat`.
4. Find files by name with `file_list` and a `pattern` (for example `*composer*`):
   it lists matching paths under the folder recursively.
5. Tests sit beside their module (`*.test.mjs`, `*.test.cjs`, `*.browser.test.cjs`).
   Read the nearest one before editing and add a case for the fix.

## Chat panel map (`ui/chat/`)

| Area | Files |
| --- | --- |
| Panel, session switching, transcript | `core/panel.js`, `core/transcript-window.js`, `core/transcript-anchor.js`, `core/message-view.js` |
| Composer text area `#chat-input` | `core/composer-draft.js`, `core/composer-enter.js`, `controls/composer-media.js` |
| Composer layout and the rows under it (`#chat-input-row`, `#chat-pill-stats-row`, `#chat-footer-row`) | `controls/chat-config.js`, `controls/chat-pill-config.js`, `controls/usage-stats.js`, `controls/footer-handle.js` |
| Scroll and drag gestures on header/footer | `core/footer-gesture-forward.js` |
| Header chips | `controls/broker-chip.js`, `controls/session-dropdown/`, `elements/main-notes/` |
| Transport | `core/client.js`, `core/fetch-stream.js` |
| Styles | `chat.css`; mobile soft keyboard sets `body.kb-open` (see `ui_design`) |

Other areas: shell `ui/app.js`, `ui/index.html`; main-panel pages `ui/main-panel/<id>/`
(`docs/ui-pages.md`); shared assets `ui/shared/`. Routes for browser checks are in
`docs/ui-journeys.md`. Design rules (tap highlight, mobile and desktop, theming) are in
the `ui_design` skill.
