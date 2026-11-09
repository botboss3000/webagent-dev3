---
id: self_review
title: Self-review
when_to_use: Use at the end of a task that had several failed calls, repeated calls or work-arounds, or when a run-health note suggests it.
default_mode: discoverable
---

# Self-review

A short look back at this task's own wasted steps, ending in at most three proposals.

1. Gather facts. Use the run-health note (failed calls by kind, repeated calls) and what you see in this conversation; count tool calls and model steps roughly.
2. Name each waste in one sentence: what happened, what it cost (calls, time), and the likely cause: unclear tool error, missing guidance, own mistake, or tool limitation.
3. Propose at most 3 fixes. Each names where it belongs: skill text, tool description, or tool mechanics.
4. Show them to the user in a few lines. Only if the user agrees, record them as a Main Notes entry with `main_notes_write` (it follows its own approval setting).

Proposals only: never edit your own skills, prompts, policies or tool files. Keep the review brief; skip it when nothing was wasted.
