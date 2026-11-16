# Ask to compact an earlier task

## Usage

- Ask the user whether to summarize an earlier, finished task now that the conversation has moved to a new one.
- Shows a Compact/Keep panel and returns at once; keep working and do not wait for the answer.
- On Compact the earlier task folds into a summary in the background; the new task's messages stay word for word and the originals stay encrypted for recall_compacted. new_task_quote is the opening words of the user message that began the new task, quoted exactly; earlier_task names the finished task in a few words; focus (optional) says what the summary must keep.

`compact_context` never compacts by itself. It shows the user a Compact/Keep panel and
returns at once. Keep working on the new task; never wait for the answer, never mention the
panel again, and never ask the user in a message whether to compact.

## When to call it

All of these must hold:

- The user started a clearly separate new task in a recent message, and the earlier task
  is finished or abandoned. A follow-up, a fix or a new step of the same work is not a new
  task.
- The earlier task took a lot of the conversation (many tool results, long files, long
  discussion) and the new task will not need its details.
- You have not asked about this earlier task before. One ask per earlier task: if the
  user picks Keep, ignores the panel or dismisses it, that is their answer. A later,
  separate change of task may ask once more.

When in doubt, do not call it: automatic compaction already handles long conversations, and
the user can type `/compact` at any time.

## Arguments

- `new_task_quote`: the opening words of the user message that began the new task, copied
  exactly (12–300 characters, one line). It must match exactly one user message; on a
  "matches several" error quote more of it.
- `earlier_task`: a few words naming the finished task, as the user would recognise it
  (3–80 characters).
- `focus` (optional): what the summary must keep from the earlier task, such as decisions,
  file paths or open questions that may still matter.

## Results and errors

Success returns the panel id and a note; carry on with the new task. An error means no
panel was shown. Do not retry with different wording after "already asked", "still open",
"already running" or "too little earlier context": those are final for now. Only a quote
that matched no message or several messages is worth one corrected retry.

After a Compact click the summary runs in the background; the conversation's activity shows
its progress, and the earlier task arrives as a summary block at a later step.
