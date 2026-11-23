# shell

## Usage

- Execute a shell command in this session workspace; reads any container file, writes only the workspace; source changes use broker task tools or checkout_edit; policy-gated

Run approved shell commands in the current session workspace. They may read system folders and the checkout's source files (Git history included) and write only this workspace. Application state, other sessions' workspaces, control sockets and the checkout's runtime folders (data/, workspaces/, build-jobs/, target/, runtime/) are unreadable; use the app's APIs for app data. Source writes use broker task file tools, or checkout_edit for a direct live-checkout edit. Control sockets stay excluded. Live runtime policy determines approval and authorization.

For isolated task verification use build_work check with the current task digest. Workspace command results do not verify an isolated broker draft. Keep checks bounded and preserve cancellation/recovery.

The /code folder itself cannot be listed, only its source subfolders: run `rg pattern /code/src /code/crates /code/ui` rather than `ls /code` or `rg pattern /code`. Search source with file_search, or with rg, which skips the ignored build-jobs/, target/ and data/ trees. grep -r from the checkout root walks hundreds of gigabytes of build artifacts and runs out of time. A command stops after timeout_seconds (default 120, at most 600) and returns as a failed result; narrow it instead of repeating it. A command and every process it starts share limits on memory, running processes and CPU time (the app's command limits). Past one, the command is stopped with exit code 137 (memory), 126 (processes) or 152 (CPU time) and a last line saying which; "Cannot fork" or "Resource temporarily unavailable" also means the process limit. Run fewer processes at once or lighter work rather than repeating it. Do not read application databases or other runtime state to answer a source or configuration question.

The result also gives `cpu_seconds`, the processor time the command and its processes used. A run's commands and an account's commands each share a processor-time budget (per run, and per account per day) and a limit on commands going at once (the app's run limits). A call past one is not run: it returns status `denied` with a reason starting "Command budget used" or "Command limit reached". Do not repeat it; finish with what you have, or wait for the running command to end.
