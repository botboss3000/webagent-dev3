# Remote Tunnel — authenticated workspace preview
This creates a separate short-lived Cloudflare quick tunnel for one local preview
server owned by the current session terminal. It never changes the permanent
development tunnel, exposes the WebAgent application, or targets a host address.
Starting the public lease requires Rust policy approval. The returned URL contains
a private 256-bit bootstrap token: share it only with the user who requested it.
Bootstrap creates an ephemeral Secure HttpOnly SameSite cookie. Requests without
that cookie are rejected; incoming cookies and Authorization are never forwarded
to the preview server. HTTP payloads are capped at 1 MiB; WebSockets and streamed
request bodies are unsupported.
The lease expires, stops when its process fails or workspace closes, and is never
replayed after worker restart. Missing CLI or ownership validation means unavailable.
