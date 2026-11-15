# host_admin

- `{"action":"status"}`: uptime, disk, listed units and containers. Runs at once.
- `{"action":"request","host_action":"file_write","host_args":{"path":"/etc/webagent/x.json","content":"<whole file>"}}`:
  `content` replaces the whole file; read it first with status or ask the user, and
  keep everything you are not changing.
- `host_revert {"request_id"}` undoes one finished file_write or broker_install (once).
- `unit_restart {"unit"}`, `container_restart {"container"}`: only names from the host
  profile; a restart of the app's own containers interrupts this chat briefly.
- `packages_upgrade {}`: apt upgrade; saves the package list first (best-effort undo).
- `broker_install {"task_id","tree_hash"}`: install a broker task's reviewed tree (the
  `tree_hash` its build_work task reports). Do not drain first: once approved, the
  supervisor drains the broker, waits up to an hour for running jobs to end, and resumes
  it when done (also on failure). It saves the running
  release, stops, installs, restarts and health-tests the broker, and restores the saved
  release itself if the test fails. `host_revert {"request_id"}` of a finished install
  restores the release it replaced. Add `"allow_stale_tree": true` only for a deliberate
  rollback the user asked for.
- `supervisor_update {}`: replace the host helper with the installed broker release's copy
  (after a broker_install that changed `host_helper.py`); the old copy comes back if the
  new one does not answer within 30 seconds.
- `{"action":"check","request_id":"..."}` reads one request.

Results: `output.state` is pending, running, done, failed or denied (expired only if the host profile sets a request time limit); `note` says
what to do next. A refusal (path not editable, unit not listed) changed nothing; explain it
to the user rather than trying another path.
