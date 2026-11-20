# Cancel a background call

Cancel a caller-owned background call in this session with `{"call_id":"<call ID>"}`, using the `call_id` from the call's running result: a background read, or a call that kept running in the background after the user sent a message. Use it when the user asks to stop that work. Rust binds the session; calls from other sessions are unavailable.

Cancellation stops the runtime waiting for the call; it does not undo effects, and a cut-off write may already have taken effect. An already finished call keeps its status. The response holds only `call_id` and `status`. A broker build is cancelled with `build_work` `action: "cancel"`, not with this tool.
