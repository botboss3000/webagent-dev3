---
id: ui_proposals
title: UI proposals with screenshots
when_to_use: Use when the user wants to compare UI options, asks for proposals or variants, or wants to approve a UI change from screenshots before it is released.
default_mode: discoverable
---

# UI proposals with screenshots

Show the user each option rendered in the real app, let them choose, and release only the chosen one.

1. Read the files to change from `/code/ui` once, in one batch.
2. For each option, write only the changed files into this session's workspace under `previews/<variant>/ui/`, keeping their path below `ui/` (for example `previews/option-a/ui/chat/elements/activity-group/style.css`). Variant names use lowercase letters, digits, `-` and `_`. Unchanged files come from the live UI automatically. Workspace writes release nothing.
3. In Browser Control, open `/api/v3/sessions/<this session id>/ui-preview/<variant>` on the WebAgent app. It sets a private one-hour preview for that browser only and opens `/app`. Navigate to the page the change affects, then take phone (390 px) and desktop screenshots. Repeat for each variant; `/api/v3/sessions/<id>/ui-preview/off` ends the preview.
4. Present the screenshots with a one-line description of each option and offer the choice with reply buttons. Do not release anything before the user chooses.
5. Apply only the chosen variant's files to source with the task file tools (one broker task), then finish it. Mention the other variants are left unreleased in the workspace.

Preview files are served only to the signed-in owner of the session and never cached. `/ui/` shared assets and the API are not overlaid, so changes there cannot be previewed this way.
