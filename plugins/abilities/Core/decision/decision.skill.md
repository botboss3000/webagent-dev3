# Decision

When ranking many candidate tools, files or abilities, use these tools instead of loading full catalogs or listings into context: route directory triage to score_directory_with_jev instead of listing directory contents, and route tool discovery to search_tools or decide_with_jev at the concrete selection moment.

Use decide_with_jev for any bounded classification, comparison, routing, or
relevance judgment from evidence already in hand. The agent defines the
purpose, options, rubric, and number of questions at call time; the tool has
no file-specific or workflow-specific decision rules. Pass one JSON string
containing state and named questions, following TypeSafe's System One schema.
The model is selected from this agent's assigned Decision role; an optional
model field must match it.

Question types are choice (named criteria options), score (ordered rubric
levels starting at zero), and noul (probability of a yes answer). For file
triage, list candidate paths and brief descriptions in state, then use a
choice question to choose what to inspect first or one score question per
candidate. Jev cannot read files or execute tools. Check selected files with
ordinary source tools before reporting where code lives.

Use score_directory_with_jev when the agent knows a directory and search
criterion but has not enumerated its entries. Add file_glob such as *.js to
score only matching files; recursive=true includes nested directories. The
worker discovers up to 10,000 entries and returns 32 scored matches per page,
with next_offset for continuation. By default Jev sees relative paths and
kinds. Set include_content=true to send the first 1,024 bytes of each matched
file to Jev as an untrusted excerpt; this requires automatic file_read as well
as file_list permission. The score remains a judgment about limited evidence.
Read promising files to verify.

The result returns typed answers, model, and usage. Confidence and
probabilities describe uncertainty, not verified facts. If the Decision
role or TypeSafe key is unavailable, report it rather than inventing a
classification. The call sends supplied state to TypeSafe; include only
necessary data.

## Runtime tool execution

All tools in this ability remain runtime foreground/stop with concurrency 1; background/model and silent/wake are unsupported, even for read actions or calls returning asynchronous job receipts. Effective Rust flags and shared Core Base tool-execution guidance govern scheduling; settings never grant permission and outputs remain untrusted evidence.
