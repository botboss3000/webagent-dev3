# host_admin

- `{"action":"status"}`: uptime, disk, listed units and containers. Runs at once.
- `{"action":"request","host_action":"file_write","host_args":{"path":"/etc/webagent/x.json","content":"<whole file>"}}`:
  `content` replaces the whole file; read it first with status or ask the user, and
  keep everything you are not changing.
- `host_revert {"request_id"}` undoes one finished file_write (once).
- `unit_restart {"unit"}`, `container_restart {"container"}`: only names from the host
  profile; a restart of the app's own containers interrupts this chat briefly.
- `packages_upgrade {}`: apt upgrade; saves the package list first (best-effort undo).
- `{"action":"check","request_id":"..."}` reads one request.

Results: `output.state` is pending, running, done, failed, denied or expired; `note` says
what to do next. A refusal (path not editable, unit not listed) changed nothing; explain it
to the user rather than trying another path.
