# v3_build_work

Begin with mode `isolated`, a stable task ID, and a summary before source edits. The receipt provides a private workspace path and input digest. Read the current `workspace_inputs_sha256` from status after applying reviewed patches and focused checks. Finish with that digest to freeze this task and immediately queue its own serial build and automatic V3 development deployment. Other registered tasks may remain open or continue editing.

The broker compares the frozen candidate with the latest healthy release. A same-file conflict fails without deploying; fix it under a new task ID. The original finish call remains pending until the Rust recovery watcher delivers the durable terminal result on that same call ID. Confirm both containers healthy after a completed result. If delivery is unavailable, inspect this task's existing job ID read only through `v3_worker_job`; never submit a duplicate finish to recover a lost response. An uncertain deployment requires reconciliation, never automatic retry. Abandon only an open task. Production is not a target.

This tool remains runtime foreground/stop with concurrency 1. Its durable broker finish/recovery path is not generic background execution; _execution_mode and generic silent/wake cannot replace it or grant deployment permission.
