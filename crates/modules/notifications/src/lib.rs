//! Notifications: metadata-only lifecycle events, distinct from private chat events.
//!
//! Every module stages notifications inside its own write transaction on the user plane
//! ([`outbox::stage`]); one relay copies committed staging rows into the application outbox
//! ([`outbox::relay`]), where retention prunes them ([`outbox::prune`]) and installation
//! administrators read them by cursor ([`outbox::page`]). Readers authenticate with revocable
//! event-reader credentials ([`readers`]) and hold bounded connections ([`connections`]).
//!
//! This crate takes borrowed pools and transactions; the root `Store`, `App` and HTTP
//! handlers are the host adapters under `host/`. Optional outbound delivery is the separate
//! `webagent-outbound-notifications` package.

pub mod connections;
pub mod cursor;
pub mod outbox;
pub mod readers;
pub mod staging;

pub use outbox::{page, prune, relay, schema, stage, stage_run, Plane, Source, RETENTION_SECONDS};

use std::time::{SystemTime, UNIX_EPOCH};

/// Current Unix time in whole seconds.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_secs()
}
