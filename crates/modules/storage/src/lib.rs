//! Storage infrastructure for the three data authorities (application, agent and
//! user): the installation key and record codec, per-agent data keys, scoped
//! SQLite pools and their schema, namespace routing, raw document/event SQL,
//! split recovery-input transactions, the legacy split migration and the
//! disposable caches. Domain decisions stay in their owning repositories; the
//! root `Store` composes these primitives with its lifecycle and domain guards.
use anyhow::Result;
use serde_json::Value;

pub mod cache;
pub mod codec;
pub mod docs;
pub mod migration;
pub mod pool;
pub mod recovery;
pub mod routing;
pub mod settings;
#[cfg(test)]
mod tests;

/// Record codec of one bound storage handle. The handle selects the agent key
/// for agent envelopes and applies the administrator plaintext policy to its
/// user-plane writes; master-key sealing is always encrypted.
pub trait ScopedCodec: Sync {
    fn seal(&self, value: &Value) -> Result<Vec<u8>>;
    fn unseal(&self, data: &[u8]) -> Result<Value>;
    fn encode_user_record(&self, value: &Value) -> Result<Vec<u8>>;
}
