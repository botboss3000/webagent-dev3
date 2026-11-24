# start_tunnel

Takes port and optional duration_seconds (30–1800; default 300). The server must already run under this session's trusted terminal supervisor.

Start the server in a terminal on one of its `preview_ports` and wait until `terminal_read` shows it serving before you call this tool. A refusal ("Authenticated workspace preview unavailable or invalid", `no_effect: true`) means nothing was started: check the port and that the server listens on it, then call again. Only a result marked uncertain needs reconciling before any retry.
