# Sign in the browser as the current user

Signs the Browser Control browser for this chat session in to the V3 development app as the current caller and opens the optional relative `path` (default `/app`). Rust supplies the session, caller, agent, short-lived cookie and authorization generation through trusted metadata; no credential argument or user selector is accepted. The result reports the verified identity and the route opened. Continue with Browser Control's `browser_read` and `browser_input`; the browser stays on the development origin until it is closed.
