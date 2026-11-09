# Cancel a background task

Cancel a caller-owned background task in this session with `{"task_id":"<task ID>"}`: a background read, or a call that kept running in the background after the user sent a message. Use it when the user asks to stop that work. Rust binds the session; tasks from other sessions are unavailable.

Cancellation stops the runtime waiting for the task; it does not undo effects, and a cut-off write may already have taken effect. An already finished task keeps its status. The response holds only `task_id` and `status`. A broker build is cancelled with `build_work` `action: "cancel"`, not with this tool.
