# wiki_create

Only app administrators can write; for anyone else a write returns `denied`, so
explain that instead of retrying.

## Turning what the user tells you into articles

When the user shares information for the Wiki ("add this", "remember for the
team", a pasted document or a long explanation):

1. Split it into topics: one article per thing a reader would look up (a
   feature, a procedure, a policy, a component). Short related facts can share
   an article; a long explanation usually becomes several linked ones.
2. For each topic, `wiki_search` with the likely title and synonyms in `also`.
   If an article already covers it, `wiki_get` it and extend it with
   `wiki_update` instead of creating a duplicate.
3. Write the body from the user's information only. Keep their facts, names,
   numbers and steps; do not add details they did not give. Mark anything they
   were unsure about. Start with a one-paragraph summary, then sections
   (`##`), lists for steps. Link related articles with `[[Article Title]]`.
4. Choose:
   - `title`: the plain name a reader would search for ("Broker release
     cycle", not "Notes from today").
   - `category`: one broad area, reused across articles (check `wiki_list`).
   - `tags`: 2–6 short labels shared with related articles.
   - `keywords`: up to 30 terms someone might search with that are not already
     in the title: synonyms, abbreviations and expansions, old or informal
     names, related component names, and 2–4 questions the article answers
     ("how do I deploy a change"). These are what let differently worded
     searches find it.
5. Leave `status` as `draft` (visible to staff only) unless the user asks to
   publish. Never publish credentials, personal data or internal-only details.
6. Report back: each article's title and slug, created or updated, its status,
   and anything you left out or need the user to confirm.

An error saying an article with this title exists means it is already there:
`wiki_get` it and use `wiki_update` instead.
