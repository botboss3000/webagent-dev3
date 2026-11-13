# file_list

file_list pages use offset from 0 and limit at most 100.

With `pattern` (for example `*composer*` or `*.test.mjs`), file_list returns the file
paths under `path` that match, recursively, without reading them: use it to find
files by name. Without `pattern` it pages one directory. Search file contents with
file_search. A pattern applies to live-checkout and container paths, not to a
broker draft (`broker_id`).

Paths are source-relative (src/example.rs). Writes without task_id create or reuse an isolated task and return its ID; reuse it. Writes need a stable request_id and the latest expected_inputs_sha256; reread after a stale-input error. No task write may modify data/. Live files (listed in /code/live-files.json) are edited with checkout_edit instead. Pass the task ID to read or search the private draft; reads without it see published source.
