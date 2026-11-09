---
id: ui_proposals
title: UI proposals with screenshots
when_to_use: Use when the user wants to compare UI options, asks for proposals or variants, or wants to approve a UI change from screenshots before it is released.
default_mode: visible
---

# UI proposals with screenshots

Show the user each option rendered in the real app, let them choose, and release only the chosen one.

Do not open a broker task or edit source until the user has chosen. Options are front-end only: if an option needs data the backend does not provide yet, derive or mock it in the variant's JavaScript and say so in its description. Screenshots come only from Browser Control; the shell has no usable browser. Write each variant's files with one batch of file_write calls.
The preview link below is ready to use: do not read its implementation. The app's design tokens (`--bg-*`, `--fg-*`, `--font-*`, colours) are defined in `/code/build-service/shared-ui/ui/shared/css/design-system.css`, served at `/ui/shared/css/design-system.css`; `/code/ui` only uses them.

1. Read the files to change from `/code/ui` once, in one batch.
2. For each option, write only the changed files into this session's workspace under `previews/<variant>/ui/`, keeping their path below `ui/` (for example `previews/option-a/ui/chat/elements/activity-group/style.css`). Variant names use lowercase letters, digits, `-` and `_`. Unchanged files come from the live UI automatically. Workspace writes release nothing.
3. Capture each variant with one Browser Control `browser_input` `script` call: navigate to `/api/v3/sessions/<this session id>/ui-preview/<variant>`, navigate to the page to show, read and click what must be opened, `wait_for` it, then a step with `"screenshot": true` and a `label` naming the option and view (for example `Option A · step rail · phone`); add a `viewport` `phone` step, navigate again and screenshot for the phone view. Equivalent detail: in Browser Control, open `/api/v3/sessions/<this session id>/ui-preview/<variant>` on the WebAgent app. It sets a private one-hour preview for that browser only and opens `/app`. Navigate to the page the change affects, then take phone (390 px) and desktop screenshots. Repeat for each variant; `/api/v3/sessions/<id>/ui-preview/off` ends the preview.
4. Present the screenshots with a one-line description of each option and its full preview link (`https://<app origin>/api/v3/sessions/<this session id>/ui-preview/<variant>`, plus the `…/ui-preview/off` link once), so the user can open and try each option in their own signed-in browser and offer the choice with one `present_chat_component` `choice` panel (clickable; a click sends the label as the user's message). Do not release anything before the user chooses.
5. Apply only the chosen variant's files to source with the task file tools (one broker task), then finish it. Mention the other variants are left unreleased in the workspace.

Preview files are served only to the signed-in owner of the session and never cached. `/ui/` shared assets and the API are not overlaid, so changes there cannot be previewed this way.
