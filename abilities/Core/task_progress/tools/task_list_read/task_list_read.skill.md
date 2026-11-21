# Read the session task list

`task_list_read` shows this session's task list. It is instant and changes nothing.

- `{}`: the current revision and one line per section (id, title, who asked for it,
  status, done and open step counts, and the closing summary of finished sections).
- `{"section":"s2"}`: that section in full, with its notes, summary and every step
  (id, label, status, outcome, evidence and when it started and finished).
- `{"revision":5}`: the list as it was at revision 5; add `section` for one section of it.
- `{"compare_to":3}`: what changed from revision 3 to now; add `revision` to compare two
  earlier revisions. Use it to answer "why didn't we do what we planned?".

The list is this session's working record; it ends with the session. Facts, decisions or
open work that another session or a later day needs belong in Main Notes instead.
