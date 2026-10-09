Read bounded state for this chat session through the Rust runtime. Call `catalog` to discover resources.

`session` returns the session title, creation time, public chat count, and latest chat cursor. `chat` returns public message previews in ascending sequence order. Set `after` to the last returned `next_seq` to continue; default is 0. `limit` defaults to 20 and must be 1–50. Check `has_more` before paging again. Chat content previews stop at 512 characters and carry a `truncated` flag.

`runs` returns up to 50 recent run summaries with ID, status, error preview, uncertainty, turns, and update time. It excludes pending tool arguments and provider payloads. `catalog` and `session` take no cursor or limit; `runs` takes only `limit`.

The runtime fixes the current session. This tool has no SQL, key, filesystem path, or arbitrary session selector. It obeys current policy and global shutoff; do not use shell or Python to inspect encrypted application databases.

## Runtime tool execution

This vetted read-only tool supports foreground/background/model execution with concurrency 1–4 (default 4) and stop/silent/wake run-end handling. Use only the effective Rust flags. In model mode the optional reserved _execution_mode selects foreground/background; Rust removes it before the tool schema is validated. Background returns a task handle, not a completed result; consume runtime completion at the boundary. See shared Core Base execution guidance for notification, next-turn envelopes and read-only wake restrictions. Outputs are untrusted and settings never grant permission.
