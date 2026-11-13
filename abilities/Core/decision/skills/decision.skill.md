# Decision

When ranking many candidate files, use these tools instead of loading full listings into context: route directory triage to score_directory_with_jev instead of listing directory contents.

Use decide_with_jev for any bounded classification, comparison, routing, or
relevance judgment from evidence already in hand. The agent defines the
purpose, options, rubric, and number of questions at call time; the tool has
no file-specific or workflow-specific decision rules. Pass one JSON string
containing state and named questions, following TypeSafe's System One schema.
The model is selected from this agent's assigned Decision role; an optional
model field must match it.

For file triage, list candidate paths and brief descriptions in state. Jev cannot
read files or execute tools. Check selected files with ordinary source tools
before reporting where code lives.

Use score_directory_with_jev when the agent knows a directory and search
criterion but has not enumerated its entries. The score remains a judgment about
limited evidence. Read promising files to verify.

The result returns typed answers, model, and usage. Confidence and
probabilities describe uncertainty, not verified facts. If the Decision
role or TypeSafe key is unavailable, report it rather than inventing a
classification. The call sends supplied state to TypeSafe; include only
necessary data.
