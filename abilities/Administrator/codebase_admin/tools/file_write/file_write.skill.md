# file_write

## Usage

- Write a container or live-checkout file, or delete a broker task file with delete true and no content; policy-gated.
- Source writes use isolated broker work automatically.
- Files are read from the source checkout (/code) and this session's workspace; other container paths, including runtime data, are refused.
- Reads without a selector inspect published/container files; pass a returned broker_id for private-source reads.
- Optional broker_id selects current, new or returned work.
- Explicit runtime/workspace paths retain their owning tools.
- Never deploys.

file_write creates a file or replaces its whole content; use it for new files rather than handwritten diffs.

Paths are source-relative (src/example.rs). Writes without task_id create or reuse an isolated task and return its ID; reuse it. Writes need a stable request_id and the latest expected_inputs_sha256; reread after a stale-input error. No task write may modify data/. Live files (listed in /code/live-files.json) are edited with checkout_edit instead. Pass the task ID to read or search the private draft; reads without it see published source.
