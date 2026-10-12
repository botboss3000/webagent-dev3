# Optional Outbound Notifications package

This administrator-only JSON-line service owns subscription state and delivery envelopes. The host seals state; package output state includes secrets and must never be returned to browser callers or logged. API result projections never include signing keys. Create and rotate require caller-supplied `signing_key` (64 hexadecimal characters); the browser generates/displays it for receiver setup and never logs it.

The host must invoke `tick` then commit state, invoke `reserve_delivery` then commit its lease, and only then invoke `execute_delivery`. Dispatch context is host-authored, never copied from request JSON. Every step is namespace CAS-fenced and generation-authorized. Tick projects fixed lifecycle metadata only. Pending signed envelopes survive event pruning and key rotation.

Subscriptions are created inactive. Installation generation changes deactivate definitions and withdraw pending envelopes. Reactivation is explicit. Transport pins validated public DNS addresses, disables proxies and redirects, and requires HTTPS port 443. Retries retain delivery IDs and signed bytes; receiving services must deduplicate. No agent execution is retried by this package.

Capacity is 64 subscriptions and 2048 retained receipts. Exhaustion blocks ingestion without advancing the cursor. This initial release requires `prune_receipts` explicit terminal receipt retention management rather than deleting uncertain evidence automatically. `cursor_expired` requires explicit operator reconciliation; the host must not silently advance over a feed gap. An administrator explicitly calls `acknowledge_gap` with the expected cursor and chosen resume cursor. History pages default to 100 records, cap at 200, and use the returned `after` continuation.
