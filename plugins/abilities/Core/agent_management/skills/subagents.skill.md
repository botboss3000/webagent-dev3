---
id: subagents
title: Work faster with sub agents
when_to_use: Use whenever a task splits into independent parts (variants, separate files or pages, separate questions to research), or when a long piece of work would crowd this conversation.
default_mode: discoverable
---

# Work faster with sub agents

Split independent work across sub agents instead of doing it in series. Each sub agent is a child session with its own workspace and browser, so parts run at the same time and their details stay out of this conversation.

1. Decide the parts first. Parts are independent when none needs another's result and they write different files.
2. Start every part in one turn: one `start_spawn` per part, all in the same step. Give each a self-contained task: the goal, the exact files or paths, what to produce, and how to report back (a short summary plus any links or labelled screenshots). The sub agent sees only that text, not this conversation.
3. End your turn after starting them. Each one's final reply arrives as a notice; do not poll. Use `read_session` only if one runs unusually long.
4. When all have reported, combine their results into one answer for the user. Present, do not redo: link to their outputs instead of repeating their work.
5. Keep shared or sequential work here: a step that needs another part's result, or a single small edit, is faster done directly.
