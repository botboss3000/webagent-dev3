# file_list

Omit task_id for explicit live/direct edits and runtime configuration. Supply one stable task_id for isolated source changes intended for a build; missing tasks are created automatically and paths are source-relative. Read the task before editing existing files. Task writes use a stable request_id and the latest expected_inputs_sha256; file_edit matches one exact, possibly multiline old_text block, file_write creates/replaces content. No task write may modify data/. Task edits never deploy; build_work finish is explicit. Follow Codebase Admin guidance.
