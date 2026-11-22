# sandbox_python

## Usage

- Run Python code (`python3 -I -c`) in this conversation's sandbox folder, with only the standard library.
- No network and nothing outside the folder; limits on run time, memory, processes, file size and folder size.

Pass `code`, and optionally `timeout_seconds`. Results are as for `sandbox_shell`: `status`, `exit_code`, `output`, `truncated` and `sandbox_bytes`.

- Python runs isolated: no user site packages and no environment variables. `pip install` and any socket fail by design.
- Memory, processes and CPU time count all the command's processes together; past one, the command is stopped with exit code 137 (memory), 126 (processes) or 152 (CPU time) and a last line saying which.
- `MemoryError` means the memory limit; an `OSError` with errno 27 (File too large) means the per-file limit.
- Relative paths are in the sandbox folder. Read and write files there; to save a file for later steps, write it with `sandbox_file` or from the code itself.
