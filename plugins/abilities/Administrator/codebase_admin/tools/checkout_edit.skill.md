# checkout_edit

Write or edit one file directly in the live checkout (/live-checkout), without a broker task. Pass path (source-relative or under /live-checkout) and either content to create/replace the file, or old_text and new_text to replace exactly one matching span (optionally expected_sha256 from an earlier read). Runtime state, workspaces, build output and Git metadata are excluded.

A direct edit changes the shared checkout only: it is not built, tested or deployed, and other writers and the broker see it as checkout drift. Prefer file_edit/file_write broker tasks for changes that should ship; use checkout_edit when the user asks for a direct checkout change. Live policy can deny this tool on its own without affecting broker task edits.
