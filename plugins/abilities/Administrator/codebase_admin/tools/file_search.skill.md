# file_search

Queries are literal substrings unless regex is true or the query contains `|`, which always means alternation; do not wrap them in quotes. Combine related terms into one regex call (`regex: true`, `query: "first_seq|latest_segment|run_logs"`) instead of one call per term. path may be a folder or one existing file. A search without a narrow path can stop at its time or size budget; the result then says so, so narrow the path (ui/, src/, one crate) rather than treating it as no match.
