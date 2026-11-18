# sandbox_file

## Usage

- `read`, `write`, `list` or `delete` one path in this conversation's sandbox folder; paths are relative to it (`element/index.html`).
- Never follows symlinks; `..`, absolute paths and empty parts are refused.

- `write` needs `path` and `content` (text, up to 512 KiB per call), creates missing folders and replaces the whole file. It is refused when the file or the folder would pass its limit.
- `read` needs `path` and returns UTF-8 text, cut at the ability's read limit (`truncated` says so). Inspect binary files with `sandbox_shell` instead.
- `list` takes an optional `path` (the folder; omit it for the sandbox root) and returns up to 500 entries with `name`, `kind` (`file`, `folder`, `symlink`) and `bytes`, plus the folder's total in `folder_bytes`.
- `delete` removes a file, a symlink (never its target) or an empty folder. Remove a whole tree with `sandbox_shell` (`rm -r name`).

Every result carries `sandbox_bytes`, the folder's size after the call.
