# file_write

file_write creates a file or replaces its whole content; use it for new files rather than handwritten diffs.

Paths are source-relative (src/example.rs). Writes without task_id create or reuse an isolated task and return its ID; reuse it. Writes need a stable request_id and the latest expected_inputs_sha256; reread after a stale-input error. No task write may modify data/. Live files (listed in /code/live-files.json) are edited with checkout_edit instead. Pass the task ID to read or search the private draft; reads without it see published source.
