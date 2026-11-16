# Show a panel in the chat

## Usage

- Show the user a panel in the chat, after your message.
- Keep prompts and hints to 200 characters (refused over 400).
- Text you write in the same response is shown to the user; after it, end the run with one short line instead of restating it.
- choice and form panels need a title naming their topic (for example "For Bob"); keep titles distinct across panels.
- Use one panel per topic: put related questions in one form (paged if long) and add a second panel only for a distinct, separate topic.
- Every choice and form panel has Submit and Dismiss built in.
- choice: one round single-choice list (data {prompt, options:[{label}]}, 1-8 one-line labels); Submit sends "<title>: <label>" as the user's message.
- form: fields the user fills and submits as one message starting with the title (data {prompt, paged, fields:[{name,label,hint,type:text|textarea|email|number|select|radio|multi,required,options,other,comment,attach}]}); radio is a round single choice and multi a checkbox group, each of 1-8 one-line options given as plain strings (options:["08:00 (Recommended)","07:00"]; {label} objects are refused, unlike choice); other:true on radio or multi adds a "Something else…" free-text row; radio and multi show an optional comment box on the same step unless comment:false, and a comment is sent as an extra "<label> — comment: <text>" line; attach:true on text or textarea adds a + that attaches files to the answer; paged:true shows one field per page with a step counter, Back, Skip (optional fields) and Continue.
- One field is sent as "<title>: A, C", several as the title then one "<label>: <value>" line each, skipped ones as "skipped".
- Answered panels stay in the chat, read-only.
- status and todo_list are display only.
- Reuse component_id with the latest revision to update a panel, including after a refused call; dismiss:true closes one you replace.
- Panels never authorize actions

## Text limits

Keep each prompt and each field hint to 200 characters or fewer: one or two short sentences. The hard limit is 400, only as a guard against runaway text; a longer prompt or hint is refused and the panel is not shown. Titles stay under 80 characters, field labels under 120 and option labels under 80, on one line. Put longer context in your message before the panel, not inside it.

## Asking several questions

Use one `form` with `paged: true` and one decision per field. Use `radio` for a single choice and `multi` for several, 2-4 concrete options each as plain strings (`["08:00 (Recommended)", "07:00"]`; unlike `choice`, `{label}` objects are refused), with your recommended option first and marked "(Recommended)". Use the hint to state the trade-off. Each `radio` and `multi` step has a comment box unless the field sets `comment: false`; leave it on when the user may want to qualify the choice. The answer arrives as the user's next message, comments included as "<label> — comment:" lines.

## Answers

The server checks each answer against your panel and builds the message, so it always reads exactly as described above. It ends with `[Answer to chat panel <id>, shown by tool call <call id>]`, naming the panel and the call that drew it. A panel takes one answer; after that it stays read-only. A dismissed panel sends no message: you read `[The user dismissed your panel "<title>" without answering.]` with the user's next message, and should not show the same question again unless they ask.
