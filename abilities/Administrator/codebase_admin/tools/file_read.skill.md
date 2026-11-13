# file_read

file_read pages use one-based offset and limit at most 200; continue until the affected functions are complete.

For a file you have not seen, read it with `outline: true` first: it returns each function, class, CSS rule or heading with its line range (`40-88 function render`), one line per block. Then read only the range you need with offset and limit. The outline comes from plain scanning, so unusual syntax can be missed; a page read is always the fallback.

Paths are source-relative (src/example.rs). Writes without task_id create or reuse an isolated task and return its ID; reuse it. Writes need a stable request_id and the latest expected_inputs_sha256; reread after a stale-input error. No task write may modify data/. Live files (listed in /code/live-files.json) are edited with checkout_edit instead. Pass the task ID to read or search the private draft; reads without it see published source.
