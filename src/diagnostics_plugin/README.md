# Fixed-admin v3 diagnostics

The live standalone Rust package is installed under `plugins/diagnostics/`.
Adding/removing that folder starts/stops the supervised recorder without rebuilding
the main app. The trusted core bridge remains under `src/diagnostics_plugin/`:
it owns scope, bounded observations, encryption and authorized archive reads.
Package build inputs and installation details are in
`src/plugin_packages/diagnostics/README.md`. Moving the package preserves evidence; deleting it deletes its archive.

## Configuration

`config/diagnostics.json` is the checked-in template. Install the package with
`plugins/diagnostics/diagnostics.json`; the API reads `/live-checkout/plugins/diagnostics/diagnostics.json`.
The loop reads these settings through the UID-checked plugin socket because it has no checkout mount. Use atomic replacement to edit it. Reload occurs within five seconds; missing or
invalid settings disable new capture without stopping chat. Configuration
`version` is 1 and `capture_scope` must be `fixed_admin`. The fixed account ID
`admin` is the only capture subject and reader, regardless of other accounts' roles.
`V3_DIAGNOSTICS_ENABLED=false` remains an emergency disable override.

Defaults: capture off; provider and context-assembly categories selected; successful
assembly metadata off; seven-day / 200,000-row metadata retention. Full context is
off and requires `enabled=true`, the context-assembly category, and an entry in
`full_context.sessions`: `{"session_id":"<UUID>","expires_at":"2026-10-10T00:00:00Z"}`.
Expiry must be RFC3339 UTC at whole-second precision. All assembly attempts in that
window are captured, including successful requests and failures. Limits default
to 24-hour retention, 16 MiB per snapshot and 256 MiB retained serialized payload.
Configuration enforces positive limits, at most 128 unique sessions, 64 MiB maximum
snapshot size, 2,000,000 maximum metadata rows and 10-year maximum retention.

A full snapshot contains the exact provider-neutral assembly ingredients,
projected history, contributions, resolved policy, and measured outcome; it is
not the provider HTTP wire body. It may contain sensitive conversation/tool text.
Snapshots always use explicit authenticated encryption, even when admin record
storage is configured as plaintext. Credentials and transport headers are never
part of this capture. Ordinary chat, recovery and interaction persistence are
unaffected by the diagnostic switch. Global HTTP/server events are not captured.

## Storage and overhead

Ownership and configuration gates precede diagnostic construction and private
copies. Metadata uses a nonblocking 4,096-event queue; full context uses eight
entries and a 64 MiB estimated serialized-byte budget. Those limits do not bound
all allocator overhead. Serialization/encryption/writes occur in the background,
except bounded input measurement and making the owned selected-session copy.
Oversized snapshots are skipped whole. Capture errors never pause chat.

The archive is `plugins/diagnostics/logs.db`; this is operational evidence, not a
session database. Five-second reload/status heartbeats and 120-second bounded
retention pruning continue while capture is disabled. Disabling discards queued
unwritten evidence; it does not delete retained evidence. Retention byte limits
measure payload, not physical SQLite/WAL size. Abrupt termination can lose queued
records; snapshots are best-effort evidence rather than recovery authority.

## Read APIs

Committed run transitions, approval checkpoints, tool-result checkpoints and recovery
overrides now use the same fixed-admin queue. Events are emitted only after the
user transaction commits, and correlate by session/run/revision, generation and
lease epoch. They contain fixed states and counters, never raw errors, model text,
tool arguments or results. Category switches `run`, `tool` and `recovery` default
to true when omitted from existing configurations. Capture still requires the
master switch. This is checkpoint evidence, not tool latency metrics or a complete
recovery timeline; lifecycle paths outside the observed save wrappers need separate producers.

* `GET /api/v3/admin/diagnostics`: existing session/run/pagination filters plus
  `category=provider|context_assembly|run|tool|recovery` and `level=info|warning|error`.
* `GET /api/v3/admin/diagnostics/status`: per-process configuration digest,
  switches, queues, drops, write errors and capture skips; heartbeat older than
  15 seconds is marked stale. Disabled capture still permits retained reads while the package is present.
* `GET /api/v3/sessions/{sid}/runs/{rid}/diagnostics`: content-free latest
  assembly attempt, distinct from numeric last-successful context accounting.
* `GET /api/v3/sessions/{sid}/runs/{rid}/diagnostics/context/{attempt_id}`:
  exact encrypted snapshot projection, fixed admin only, current session/agent
  access required, `Cache-Control: no-store`; missing/expired captures return 404.

Old archive records without verified fixed-admin ownership are not exposed by
new listings. No settings UI, text-log mirror, public-user tracing or live feed
is added. Earlier attempts cannot be reconstructed exactly from later settings.

## Verification

Focused Rust tests: `cargo test --locked diagnostics -- --test-threads=1`.
Run each process-global acceptance test separately:
`cargo test --locked recorder_end_to_end_live_reload_and_private_capture -- --ignored --nocapture`.
Measurement on temporary state:
`cargo test --locked capture_accounting_measurement -- --ignored --nocapture`.
Measurements cover fixture capture cost and retained bytes, not complete service
latency or allocator counts. Use the pinned remote Rust environment and capture
source inputs before and after each run.

The installed package owns diagnostics.json and logs.db (including SQLite WAL sidecars). Removing the folder removes archive access and capture; moving it preserves its settings and evidence. The host does not recreate it. Ephemeral socket/PID/ready files remain under /state/diagnostics. Snapshot ciphertext still requires the originating installation key to decrypt; a package alone does not carry that key.
