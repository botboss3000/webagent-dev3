# v3_source_list

List immediate files and directories in an open isolated task's V3 source snapshot. Omit `path` to list the V3 root, then use a returned directory path such as `ui/main-panel` to drill down. Read a returned file path with `v3_source_read`. Use `offset` and `limit` while `has_more` is true; this tool never changes source.

Broker task source ≠ session workspace — see the V3 Build skill. A missing directory means check the parent listing. Do not infer that the entire source snapshot is absent from one missing path.

Paths are relative to the V3 source root. The listing omits symlinks and runtime data.

For administrator troubleshooting tasks, the exact directories `data/user_data/admin` and `data/agent_data/shared_default` and their descendants are also listable. Parent data directories and other users/agents remain denied. Database reads use the paginated SQLite text export described in v3_source_read.
