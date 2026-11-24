# python

## Usage

- Execute Python in this session workspace; reads any container file, writes only the workspace; source changes use broker task tools or checkout_edit; policy-gated
- The sandbox folder (file tools with `"root": "sandbox"`, `sandbox_file`) is the workspace's `sandbox/` subfolder: write there (`sandbox/notes.txt`) for a file the file tools should read back.

Run approved python commands in the current session workspace. They may read system folders and the checkout's source files (Git history included) and write only this workspace. Application state, other sessions' workspaces, control sockets and the checkout's runtime folders (data/, workspaces/, build-jobs/, target/, runtime/) are unreadable; use the app's APIs for app data. Source writes use broker task file tools, or checkout_edit for a direct live-checkout edit. Control sockets stay excluded. Live runtime policy determines approval and authorization.

For isolated task verification use build_work check with the current task digest. Workspace command results do not verify an isolated broker draft. Keep checks bounded and preserve cancellation/recovery.

A command and every process it starts share limits on memory, running processes and CPU time (the app's command limits). Past one, the command is stopped with exit code 137 (memory), 126 (processes) or 152 (CPU time) and a last line saying which; "Cannot fork" or "Resource temporarily unavailable" also means the process limit. Run fewer processes at once or lighter work rather than repeating it.

The result also gives `cpu_seconds`, the processor time the command and its processes used. A run's commands and an account's commands each share a processor-time budget (per run, and per account per day) and a limit on commands going at once (the app's run limits). A call past one is not run: it returns status `denied` with a reason starting "Command budget used" or "Command limit reached". Do not repeat it; finish with what you have, or wait for the running command to end.
