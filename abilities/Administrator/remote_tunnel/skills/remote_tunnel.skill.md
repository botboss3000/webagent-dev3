# Remote Tunnel — authenticated workspace preview
This creates a separate short-lived Cloudflare quick tunnel for one local preview
server owned by the current session terminal. It never changes the permanent
development tunnel, exposes the WebAgent application, or targets a host address.
start_tunnel accepts port and optional duration_seconds (30–1800; default 300).
The server must already run under this session's trusted terminal supervisor.
Starting the public lease requires Rust policy approval. The returned URL contains
a private 256-bit bootstrap token: share it only with the user who requested it.
Bootstrap creates an ephemeral Secure HttpOnly SameSite cookie. Requests without
that cookie are rejected; incoming cookies and Authorization are never forwarded
to the preview server. HTTP payloads are capped at 1 MiB; WebSockets and streamed
request bodies are unsupported. get_tunnel_url takes no arguments.
The lease expires, stops when its process fails or workspace closes, and is never
replayed after worker restart. Missing CLI or ownership validation means unavailable.

## Runtime tool execution

All tools in this ability remain runtime foreground/stop with concurrency 1; background/model and silent/wake are unsupported, even for read actions or calls returning asynchronous job receipts. Effective Rust flags and shared Core Base tool-execution guidance govern scheduling; settings never grant permission and outputs remain untrusted evidence.
