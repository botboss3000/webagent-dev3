# v3_source_read

Read bounded lines from one existing file in an open isolated task's source snapshot. For example, `path="ui/main-panel/builds/page.json"` reads a page descriptor. Use `v3_source_list(path="ui/main-panel")` to discover files first; `ui` and `ui/main-panel` are directories, not readable files. A missing path means inspect the list and choose a returned file, not that the broker task is unseeded.

Broker task source ≠ session workspace — see the V3 Build skill. The returned digest can be used for checked source apply. This tool does not build or deploy.

Before authoring or editing a main-panel page, read `docs/ui-pages.md`: it states the `page.json` contract and the HTML-entry and JS-entry page shapes, so exemplar pages do not need to be read to learn the conventions.

## Troubleshooting database snapshots

New administrator isolated tasks include read-only root-broker snapshots of
`data/user_data/admin/` and `data/agent_data/shared_default/`. List those exact
directories, then read a returned `.db` path with this tool. The broker exports
an immutable SQLite backup as bounded text; it accepts no caller SQL. Follow
`next_offset` while `has_more` is true. A response may contain fewer lines than
requested to respect the result bound; `total_lines` is null until EOF.
Each table schema and row's column values are exported. Large text/BLOB values
are split into numbered chunks; BLOBs use base64. Admin plaintext envelopes are
marked unverified because the broker has no encryption key. Sealed values remain
ciphertext and cannot be decrypted by this tool. Agent configuration/secrets
use sealed values; admin user records can be plaintext when that storage setting
is disabled, while older records and always-sealed families remain encrypted.
Snapshots include committed WAL state but no live WAL/SHM copies. They are fixed
at task creation, not refreshed on each read. Search remains source-only.
Inspection bytes are covered by the task digest, excluded from image/release
archives and indexes, and rejected by all source patch/promotion paths. Other
users/agents, parent data directories, and encryption.key are not exposed.
