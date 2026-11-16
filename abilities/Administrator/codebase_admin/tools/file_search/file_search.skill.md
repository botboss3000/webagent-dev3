# file_search

## Usage

- Search container or live-checkout files by literal or regex query, or with find answer a plain-language question with the best-matching named blocks and their line ranges; outline comments in one file.
- Source writes use isolated broker work automatically.
- Files are read from the source checkout (/code) and this session's workspace; other container paths, including runtime data, are refused.
- Reads without a selector inspect published/container files; pass a returned broker_id for private-source reads.
- Optional broker_id selects current, new or returned work.
- Explicit runtime/workspace paths retain their owning tools.
- Never deploys.

To find where something is done when you do not know its identifiers, ask first with `find: true` and a plain-language `query` (for example "move the dropdown menu to the body when it opens"), narrowed with `path` where you can; without one it scans each area of the checkout (each UI folder, crate, ability group and the broker) in parallel and gives every area a share of the candidates. It returns the best-matching named blocks (8 by default) (functions, classes, CSS rules, headings) as `path:start-end  name`, each followed by one line from the comment directly above it (blocks are ranked on that comment too) or its first line. Read the best one or two with file_read offset and limit before searching again: the answer is often already in the list, and another search only repeats it. Use a literal or regex query instead for an exact string such as an error message or class name, which find can miss inside a long block. find searches published source only. Its budgets and ranking weight are live settings in `find-ranking.json` beside this skill, and each lookup is logged as a hit when you then read one of its blocks, or a miss when you search again first, so reading the listed blocks also tells the tuning log which result helped.

Each match line is cut to a window around the match. The first match inside a block names it with its line range, for example `ui/chat/chat.css:169:...  ⟨#chat-pill-stats-strip 165-174⟩`; read that range with file_read offset and limit instead of a wide page. A bounded result does not prove absence.

Paths are source-relative (src/example.rs). Writes without task_id create or reuse an isolated task and return its ID; reuse it. Writes need a stable request_id and the latest expected_inputs_sha256; reread after a stale-input error. No task write may modify data/. Live files (listed in /code/live-files.json) are edited with checkout_edit instead. Pass the task ID to read or search the private draft; reads without it see published source. A draft search takes a literal query and an optional path (a file or folder) to narrow it; regex, file_glob and comments_only work only on published source.
