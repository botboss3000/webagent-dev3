# Sandbox

This conversation has its own scratch folder, the sandbox. Use it to write and test small
files, pages and scripts.

- `sandbox_file` reads, writes, lists and deletes files in it. Paths are relative to the
  sandbox (`element/index.html`).
- `sandbox_shell` runs a shell command there and `sandbox_python` runs Python there. Both
  start in the sandbox folder, which is also their home and temporary folder.

What the sandbox cannot do, by design:

- No network at all: no downloads, package installs, web requests or local services. Use
  only the tools and Python standard library already installed.
- Nothing outside the folder: no app source, settings, data, other conversations or
  `/proc`. Commands read only the system's program and library folders.
- Limits on every command: run time, memory, processes, file size and the folder's total
  size, which also counts toward the user's storage. The exact limits are in the ability's
  `limits.json`. A command that passes the folder limit is stopped; delete files to free
  space.

The folder belongs to this conversation only; other conversations cannot see it. Nothing in
it leaves the sandbox on its own.

When a command is refused or fails because of a limit, say which limit it was and work
within it; do not try to get around it.
