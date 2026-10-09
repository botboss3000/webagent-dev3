# Live diagnostics application plugin

Installed package: `/home/ar/webagent-dev3/plugins/diagnostics/`. Contains
`plugin.json`, `webagent-diagnostics` executable and optional Rust source.
The API supervisor scans this administrator-controlled package every two seconds.
Add it to start, remove it to stop, atomically replace the executable to update.
Source alone is not executable. No automatic reinstallation occurs on restart.

The standalone Rust app owns archive writes and retention. The core bridge owns
fixed-admin admission, config gates, bounded queues, encryption and authorized
read projections. No key, cookie or provider credential enters the socket protocol.
The process runs as the trusted application identity; installed executable code
is administrator-trusted, not a sandbox for untrusted third-party packages.

Protocol 1 uses a local UID-checked 0600 socket inside a 0700 diagnostics directory,
length-prefixed bounded JSON and durable-write acknowledgements. Context arrives
as ciphertext with numeric size and identity coordinates, never plaintext.
One frame is processed at a time; timeout or disappearance drops best-effort
queued evidence and increments errors without blocking chat. Retained reads remain
available while the package is absent. Plugin removal preserves the archive.
The supervisor clears inherited environment and restarts crashed children on the
next scan. It rejects symlink packages/entries, incompatible manifests and group-
or world-writable executables. Do not install untrusted binaries.

Canonical broker build inputs live in `src/plugin_packages/diagnostics/` because
the fixed broker allowlist already includes `src/`. The installed package lives
under `plugins/`; its own Cargo manifest builds independently of the main app.
Use the matching lockfile shipped with the source and pinned Rust 1.90 environment.
The initial image includes the executable for explicit first installation; deleting
an installed package does not resurrect it from that image.
