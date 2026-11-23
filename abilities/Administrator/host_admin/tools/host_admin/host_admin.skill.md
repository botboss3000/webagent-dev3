# host_admin

## Usage

- Request a change to the server through the root host helper, or read host status.
- Every change waits for an administrator's approval on the Host Admin page; a background notice wakes this chat when it is decided and finished.
- `file_write`, `host_revert`, `broker_install`, `supervisor_update` and `packages_upgrade` are root-equivalent: approval needs the host approval passphrase unless an allow-all window covers this chat (the request then runs at once); on hosts set to SSH-only approvals (production) the user approves it with `sudo buildctl host approve ID`. Say so when you file one.

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
- Not offered here: `rotate_browser_login_token` (new Diagnostics browser sign-in key). If the
  user asks for it, tell them to run `buildctl host request rotate_browser_login_token` over
  SSH and approve it; never ask for, read or repeat the key.

Results: `output.state` is pending, running, done, failed or denied (expired only if the host profile sets a request time limit); `note` says
what to do next. A refusal (path not editable, unit not listed) changed nothing; explain it
to the user rather than trying another path.
