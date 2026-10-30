# Wiki Control: the shared server Wiki

The Wiki is one shared Markdown knowledge base for the whole app: features,
processes, policies, contacts and project facts. It is not your private memory
and not the chat transcript. Everyone with Wiki Control reads the same articles.

## Read first

1. `wiki_search(query, also)` whenever a question might be answered by shared
   knowledge, and always before creating an article. Search matches words, not
   meaning, so put synonyms and other phrasings in `also` (for "refund": money
   back, reimbursement, return policy). If nothing relevant comes back, retry
   once with different terms before concluding the Wiki has no answer.
2. `wiki_list(category?)` to browse when you have no precise query.
3. `wiki_get(slug)` for the full article. Use only slugs that a search or list
   returned; a missing slug is an error, not an invitation to guess.
4. Answer from what you read and name the articles you used. If nothing
   relevant exists, say so; never invent an article, slug or fact.

Visitors see published articles only. Staff (the agent's owner and members) also
see drafts, and can read `wiki_history(slug)` and `wiki_get_revision(revision_id)`.

## Writing (app administrators only)

Only an app administrator can change the server Wiki. For anyone else a write
returns `denied`; explain that rather than retrying.

- `wiki_create(title, body, tags?, keywords?, category?, status?)`: the slug comes
  from the title. New articles are `draft` unless you pass `published`. Fill
  `keywords` with synonyms, related terms and the questions the article answers,
  so searches worded differently still find it; refresh them when you update it.
- `wiki_update(slug, expected_revision, ...)`: only the fields you pass change.
  `wiki_get` first, build on the current text and pass its `revision`. A
  "changed" error means someone edited it: read it again before retrying.
- `wiki_set_status(slug, status, expected_revision)`: `published` is visible to
  visitors; never publish confidential content.
- `wiki_restore(slug, revision_id, expected_revision)`: brings back a saved
  version, including a deleted article. Pass `current_revision` from
  `wiki_history` as `expected_revision`.
- `wiki_delete(slug, expected_revision)`: removes the article for everyone but
  keeps its history. Prefer `wiki_set_status(..., "draft")` to hide something,
  and check `wiki_backlinks(slug)` first.

Link related articles with `[[Article Title]]` or `[[slug]]` instead of
repeating content; keep one topic per article.
