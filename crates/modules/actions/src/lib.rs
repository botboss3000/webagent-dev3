//! Actions: intent identity, duplicate-effect protection, claims, approvals of additional
//! matching intent and uncertain-effect reconciliation. Model certainty never establishes new
//! intent; a matching action needs fresh user intent and specific authorization.
//!
//! State lives in the existing sealed `action:<id>` records, the `action_index`/`action_lock`
//! tables of each user plane and the native agent receipts. Storage owns the schema, pools and
//! codec; Engine owns run orchestration and the `Actions` port. There is no `actions.json`.
//! Root-bound adapters (App/Store glue, the Engine port implementation) live in `host/` and are
//! compiled by the root binary.
pub mod intent;
pub mod outcome;
pub mod receipts;
pub mod repository;

use serde::{Deserialize, Serialize};

pub use intent::{strip, valid_id};
pub use outcome::{response, summary};
pub use webagent_chat::RecordCodec;

/// Whether `name` is a tool whose effects the action ledger protects. Only the
/// owner-credential purchase fixture remains; every other write uses the engine's
/// per-run repeat guard.
#[must_use]
pub fn supported(name: &str) -> bool {
    name == "test_owner_purchase"
}

/// Whether duplicate protection is on for `name` when no setting chooses otherwise.
#[must_use]
pub fn default_on(name: &str) -> bool {
    supported(name)
}

/// The per-tool `duplicate_protection` setting.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Protection {
    /// New intents run unprotected; existing matching intents stay authoritative.
    Off,
    /// Every supported call is prepared, claimed and finished through the ledger.
    #[default]
    On,
}

#[cfg(test)]
mod tests;
