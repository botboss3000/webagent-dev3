# MCP live package

This optional package runs independently as `webagent-mcp`. Install the reviewed
binary with `plugin.json` and `mcp.json` in the live `plugins/mcp/` directory.
The runtime supervisor discovers the directory live; removing or disabling it
withdraws MCP availability. The core does not recreate a missing package.
A core deployment is needed once for the broker/ability adapter; future reviewed
package binary/config replacements use the live supervisor lifecycle.

The host supplies `V3_PLUGIN_PACKAGE` and `V3_PLUGIN_STATE`; runtime files live
under `$V3_PLUGIN_STATE/mcp/`: UID-authenticated 0600 `plugin.sock`, PID and
two-second readiness heartbeat. There is no credential or connection database
in this package. The host owns encrypted per-agent connections and approvals.

One big-endian u32-length JSON frame per connection, maximum 1 MiB:
`{version:1,operation:"search"|"inspect"|"call",endpoint?,query?,tool?,arguments?,bearer?,expected_schema?}`.
Reply: `{version:1,status:"success"|"error",output:...}`.
Search returns `{servers:[{name,description,endpoint,transport,source}],registry_available}`.
Inspect returns `{tools:[{name,description,inputSchema}],protocol_version}`.
Call requires the exact inspected tool in `expected_schema`, refreshes tools/list,
rejects drift before dispatch and returns the standard MCP tools/call result.
An MCP result with `isError:true` may follow partial external effects and is held as uncertain.
An error with `output.uncertain:true` means dispatch happened and its outcome
must be reconciled before repeating a potentially external write.

Initial transport is public HTTPS Streamable HTTP only, protocol 2025-06-18.
Initialization, session headers, initialized notifications, JSON/SSE replies,
bounded tool pagination and best-effort session DELETE are supported.
No legacy SSE transport, subprocess packages, resources, prompts, OAuth,
sampling, elicitation, roots, server-initiated requests or automatic retry.
Bearer credentials are supplied by the host per operation, never logged or
returned; exact credential reflections in output are redacted. Tools/results
are untrusted external data. Server annotations do not grant authorization.

Endpoints reject URL credentials, queries and fragments. Every DNS answer must
be public; connections pin the checked DNS results, disable proxy environment
and redirects, and reject private, loopback, link-local, metadata, CGNAT and
special-use IPv4/IPv6. Local/internal servers are unsupported initially.
Requests run within 30 seconds with 1 MiB output and four local-client slots.
The host must never automatically retry an uncertain external tool call.

Search calls the official registry read API at
https://registry.modelcontextprotocol.io/v0.1/servers with search, limit=50 and
version=latest, and merges optional curated entries from mcp.json:
`{"version":1,"servers":[{"name":"example","description":"...","remotes":[{"type":"streamable-http","url":"https://example.com/mcp"}]}]}`.
Only remote metadata is returned; search never downloads or executes packages.
Registry lookup has an eight-second budget inside the thirty-second IPC deadline.
Registry failure or timeout sets registry_available=false; curated results still work when
registry HTTP retrieval fails. Credential-free endpoint metadata is untrusted
and is checked again with DNS pinning before any connection.

Protocol reference:
https://modelcontextprotocol.io/specification/2025-06-18/basic/transports
https://modelcontextprotocol.io/specification/2025-06-18/basic/lifecycle
Registry API:
https://github.com/modelcontextprotocol/registry/blob/main/docs/reference/api/openapi.yaml

## Standalone reviewed replacements

The package has its own minimal pinned manifest and Cargo.lock; building it does
not require compiling the WebAgent application. On the development server in an
isolated reviewed source task and the pinned Rust 1.99 environment:

```sh
cargo build --locked --release --manifest-path src/plugin_packages/mcp/Cargo.toml
cargo test --locked --manifest-path src/plugin_packages/mcp/Cargo.toml
```

Coordinate Cargo/cache/target use and attest unchanged package inputs before and
after verification. Release replacements consist of the verified
`src/plugin_packages/mcp/target/release/webagent-mcp`, `plugin.json`, and `mcp.json` installed into the
existing live `plugins/mcp/` directory with administrator-owned secure
permissions. Replace the reviewed binary atomically; the live supervisor notices
replacement, stops the prior process, clears stale readiness and starts the new
package. Preserve rollback binaries/config and reconcile interrupted external
writes before retrying. Source and this README are review/build assets rather
than runtime dependencies. Initial ability/IPC adapter deployment is separate.
