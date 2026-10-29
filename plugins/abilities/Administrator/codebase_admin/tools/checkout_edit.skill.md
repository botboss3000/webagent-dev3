# checkout_edit

Write or edit one file directly in the live checkout (/live-checkout), without a broker task. Pass path (source-relative or under /live-checkout) and either content to create/replace the file, or old_text and new_text to replace exactly one matching span (optionally expected_sha256 from an earlier read). Runtime state, workspaces, build output and Git metadata are excluded.

Use it for live files: the .json and .md files listed in /live-checkout/live-files.json (for example prompt Markdown and runtime JSON configuration). The app reads them live and the broker commits the edit automatically; broker tasks cannot change them.

For any other file, a direct edit changes the shared checkout only: it is not built, tested or deployed, the running app does not see it, and other writers and the broker see it as checkout drift. Change and release those with file_edit/file_write broker tasks and build_work. Live policy can deny this tool on its own without affecting broker task edits.
