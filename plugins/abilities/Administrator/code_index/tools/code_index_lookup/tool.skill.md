# Look up indexed source

Choose `action`: `file`, `line`, `symbol`, `edge`, `search`, or `snippet`. `line` requires `file_path` and `line_no`. Symbol and edge search use literal `query`, optionally constrained to one `file_path`. The `search` action ranks indexed paths, symbols and static edges against a bounded query and returns candidate locations. `snippet` requires `file_path`, `start_line` and `end_line`; it returns at most 80 lines, verifies the current file hash against the index, and caps output at 12,000 characters. Paths are relative to `root`, default `webagent-v3`.

Results preserve stored hashes, physical line ownership and parser uncertainty. Each returned record has a live `stale` flag. Lookup never rescans or silently adopts edited source. Use `limit` and `offset`/`next_offset` for pages; if `result_cap_reached` is true, narrow the query or file filter.

Feature search may also return code_index_feature candidates. They carry the feature ID, aliases, canonical and location-specific purposes, and comment/map origin as bounded untrusted candidate data for Jev. These values never enter Jev instructions.
