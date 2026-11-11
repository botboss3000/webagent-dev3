//! Abilities storage on the shared storage handle: Core Vault credential
//! submission and Core Webhooks ingestion. Root calls these methods on its `Store`
//! through `Deref` with [`AbilitiesStore`] imported.
use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use webagent_host_core::StoreCore;

mod saved_scripts;
mod vault;
mod webhooks;

pub use vault::valid_credential_name;

/// Abilities record writes for one storage handle. Futures are `Send` so routes
/// and tool handlers can hold them across awaits.
pub trait AbilitiesStore {
    /// Saves a credential submitted through the conversation's pending Core Vault
    /// form. The request id, name, actor and revision must match that form; a
    /// replayed request returns its first result. The value is never returned.
    fn core_save_credential<'a>(
        &'a self,
        sid: &'a str,
        name: &'a str,
        request: &'a str,
        expected: u64,
        value: &'a str,
    ) -> impl Future<Output = Result<Value>> + Send + 'a;
    /// Retains one external event for a live Core Webhooks receiver and stages a
    /// metadata notification. A repeated event id with the same payload is a
    /// duplicate; with a different payload it is rejected.
    fn core_webhook_ingest<'a>(
        &'a self,
        id: &'a str,
        revision: u64,
        token_digest: &'a str,
        event_id: &'a str,
        payload: &'a Value,
    ) -> impl Future<Output = Result<Value>> + Send + 'a;
    /// Changes this user's saved scripts for `agent` under the writer lock. A missing
    /// document reaches `change` as null. With `expected`, a different document revision
    /// fails without changing anything. The document is written even when `change` only
    /// normalizes it; an error from `change` writes nothing.
    fn saved_scripts_change<'a>(
        &'a self,
        agent: &'a str,
        expected: Option<u64>,
        change: &'a mut (dyn FnMut(&mut Value) -> Result<Value> + Send),
    ) -> impl Future<Output = Result<Value>> + Send + 'a;
}

impl AbilitiesStore for StoreCore {
    fn core_save_credential<'a>(
        &'a self,
        sid: &'a str,
        name: &'a str,
        request: &'a str,
        expected: u64,
        value: &'a str,
    ) -> impl Future<Output = Result<Value>> + Send + 'a {
        vault::save_credential(self, sid, name, request, expected, value)
    }
    fn core_webhook_ingest<'a>(
        &'a self,
        id: &'a str,
        revision: u64,
        token_digest: &'a str,
        event_id: &'a str,
        payload: &'a Value,
    ) -> impl Future<Output = Result<Value>> + Send + 'a {
        webhooks::ingest(self, id, revision, token_digest, event_id, payload)
    }
    fn saved_scripts_change<'a>(
        &'a self,
        agent: &'a str,
        expected: Option<u64>,
        change: &'a mut (dyn FnMut(&mut Value) -> Result<Value> + Send),
    ) -> impl Future<Output = Result<Value>> + Send + 'a {
        saved_scripts::change(self, agent, expected, change)
    }
}

#[cfg(test)]
mod tests;
