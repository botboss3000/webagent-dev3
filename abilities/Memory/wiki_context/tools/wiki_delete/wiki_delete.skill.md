# wiki_delete

## Usage

- App administrators only.
- Remove an article for everyone; its history is kept and wiki_restore can bring it back.
- Prefer wiki_set_status draft to hide an article.
- Check wiki_backlinks first.

Removes the article for everyone but keeps its history, so `wiki_restore` can
bring it back. Prefer `wiki_set_status(..., "draft")` to hide something.
Check `wiki_backlinks(slug)` first and tell the user which articles link to it.
Delete only when the user asks for that article to go.
