# sandbox_shell

## Usage

- Run one `/bin/sh -c` command in this conversation's sandbox folder; it starts there, with the folder as home and temporary folder.
- No network and nothing outside the folder; limits on run time, memory, processes, file size and folder size.

Pass `command`, and optionally `timeout_seconds` (default and maximum: the ability's run-time limit). The result gives `status` (`success`, `failed` or `timeout`), `exit_code`, the merged output (`output`, cut at 1 MiB with `truncated`) and `sandbox_bytes`, the folder's size afterwards.

- Exit code 125 with "passed its size limit" means the folder grew past its limit and the command was stopped. "Sandbox folder is full" means no command runs until you delete files.
- "Cannot fork" or "Resource temporarily unavailable" means the process limit; run fewer processes at once.
- Programs that need the network, `/proc` or files outside the sandbox fail with "Permission denied" or "Operation not permitted". That is the sandbox working, not a fault to work around.

Use `sandbox_file` to write file content rather than shell redirection with long here-documents.
