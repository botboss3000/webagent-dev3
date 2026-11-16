# wiki_delete

Removes the article for everyone but keeps its history, so `wiki_restore` can
bring it back. Prefer `wiki_set_status(..., "draft")` to hide something.
Check `wiki_backlinks(slug)` first and tell the user which articles link to it.
Delete only when the user asks for that article to go.
