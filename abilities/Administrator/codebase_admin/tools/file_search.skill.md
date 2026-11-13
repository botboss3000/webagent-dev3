# file_search

Each match line is cut to a window around the match. The first match inside a block names it with its line range, for example `ui/chat/chat.css:169:...  ⟨#chat-pill-stats-strip 165-174⟩`; read that range with file_read offset and limit instead of a wide page. A bounded result does not prove absence.

Paths are source-relative (src/example.rs). Writes without task_id create or reuse an isolated task and return its ID; reuse it. Writes need a stable request_id and the latest expected_inputs_sha256; reread after a stale-input error. No task write may modify data/. Live files (listed in /code/live-files.json) are edited with checkout_edit instead. Pass the task ID to read or search the private draft; reads without it see published source. A draft search takes a literal query and an optional path (a file or folder) to narrow it; regex, file_glob and comments_only work only on published source.
