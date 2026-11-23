# sandbox_python

## Usage

- Run Python code (`python3 -I -c`) in this conversation's sandbox folder, with only the standard library.
- No network and nothing outside the folder; limits on run time, memory, processes, file size and folder size.

Pass `code`, and optionally `timeout_seconds`. Results are as for `sandbox_shell`: `status`, `exit_code`, `output`, `truncated` and `sandbox_bytes`.

- Python runs isolated: no user site packages and no environment variables. `pip install` and any socket fail by design.
- Memory, processes and CPU time count all the command's processes together; past one, the command is stopped with exit code 137 (memory), 126 (processes) or 152 (CPU time) and a last line saying which.
- `MemoryError` means the memory limit; an `OSError` with errno 27 (File too large) means the per-file limit.
- Relative paths are in the sandbox folder. Read and write files there; to save a file for later steps, write it with `sandbox_file` or from the code itself.

The result also gives `cpu_seconds`, the processor time the command and its processes used. A run's commands and an account's commands each share a processor-time budget (per run, and per account per day) and a limit on commands going at once (the app's run limits). A call past one is not run: it returns status `denied` with a reason starting "Command budget used" or "Command limit reached". Do not repeat it; finish with what you have, or wait for the running command to end.
