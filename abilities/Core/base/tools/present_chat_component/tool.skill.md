# Show a panel in the chat

## Text limits

Keep each prompt and each field hint to 200 characters or fewer: one or two short sentences. The hard limit is 400, only as a guard against runaway text; a longer prompt or hint is refused and the panel is not shown. Titles stay under 80 characters, field labels under 120 and option labels under 80, on one line. Put longer context in your message before the panel, not inside it.

## Asking several questions

Use one `form` with `paged: true` and one decision per field. Use `radio` for a single choice and `multi` for several, 2-4 concrete options each, with your recommended option first and marked "(Recommended)". Use the hint to state the trade-off. Each `radio` and `multi` step has a comment box unless the field sets `comment: false`; leave it on when the user may want to qualify the choice. The answer arrives as the user's next message, comments included as "<label> — comment:" lines.
