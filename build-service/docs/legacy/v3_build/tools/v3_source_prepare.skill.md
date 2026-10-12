# v3_source_prepare

Prepare one to eight **new** V3 source files in an open isolated task. Pass each V3-relative `path` and its complete `content`, ending with a newline, along with the current `workspace_inputs_sha256`. The broker generates exact Git hunk counts, checks the diff against the task snapshot, and returns `patch`, `prepared_id`, and `patch_sha256`. It does not edit the source.

Review the returned patch as text. Then call `v3_source_apply_prepared` with `task_id`, `request_id`, `prepared_id`, and the same `expected_inputs_sha256`. A changed task digest makes the prepared patch stale. For existing files, use a reviewed standard Git diff with `v3_source_apply`.
