# wiki_update

`wiki_get` first and pass its `revision` as `expected_revision`; only the
fields you pass change, so send the whole new `body` built on the current text,
not a fragment. Keep facts the article already has unless the user corrects
them. When new information widens the topic, add matching `keywords` (pass
the full list: existing plus new). A "Wiki article changed" error means someone else edited
it: read it again and reapply your change. Tell the user what you changed.
