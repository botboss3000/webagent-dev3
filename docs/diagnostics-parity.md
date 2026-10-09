# V3 diagnostics parity with V2

Scope: fixed account `admin` only. Public-user capture is excluded even as feature
coverage grows. Live context snapshots remain selected, expiring and encrypted.

## Backend foundation

Implemented: provider failures, context assembly decisions and encrypted optional
context ingredients; bounded queues, reload, retention and per-process health.
Committed `save_run` transitions add run/tool/recovery checkpoint evidence after
successful user transactions, correlated to the run revision and lease generation.

Next: admission and lease-acquisition/recovery producers, explicit approval and
cancellation outcomes, tool duration/outcome accounting and background/batch-child
correlation. Distinguish checkpoint facts from inferred timing. API filtering,
category statistics and owner-scoped purge should have authenticated acceptance.

## Operator interface

Port V2 filtered search/statistics into a V3 diagnostics page with process health,
latest failed assembly distinct from last success, private snapshot access and
explicit incomplete/dropped evidence. Check the V3 route and Code Index contracts.
Avoid text-log mirrors unless a measured operational need justifies duplication.
Global server/HTTP records require verified fixed-admin attribution; do not capture
unattributed public traffic just to reproduce V2's wider global recorder.

## Journey Recorder

Port the optional application plugin contract into Rust-owned leases, scopes,
ownership and retention. Reuse the browser observer concepts for DOM/stream/cache,
console/error/network metadata, lag and optional screenshots. Support independent
upload acknowledgements, reload/finalization barriers and explicit capture gaps.
Do not assume the V2 Python lifecycle, auth or database adapters operate in V3.

## Reports

Add independent mechanical checks and bounded evidence neighborhoods. Tool-free
model investigation is a separate opt-in feature; approving and executing a fix
must remain separate revision-bound human actions. Never allow report text to
self-authorize commands or mutate source.

## Acceptance

Each slice needs negative public/other-admin tests, secret sentinels, bounded
queues, write-error isolation, retention/reload tests and authenticated API checks.
Browser capture requires browser journeys for stop/reload/lease expiry and gaps.
All releases use isolated tasks and the protected build broker; health alone does
not establish recorder parity or correctness of generated reports.
