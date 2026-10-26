//! Conversation lifecycle, participation, sharing, history, controls, follow-ups, Main Chat and session naming.
//! Configuration stays in existing `config/chat.json` and `config/prompts.json`; no new JSON files or daemons are introduced.
//! Root-bound adapters (Store/App, HTTP routes) live in `host/` and are compiled by the root binary.
pub mod configuration;
pub mod followup;
pub mod history;
pub mod main_chat;
pub mod messaging;
pub mod naming;
pub mod session;
pub mod shared;

use anyhow::Result;
use serde_json::Value;
pub use webagent_configuration::digest;

/// Existing Storage codec contract; Chat never manages encryption keys or connection pools.
/// `encode_user_record` applies the owner's user-plane policy (administrator plaintext option);
/// `seal` always encrypts. Implementations must keep that distinction.
pub trait RecordCodec: Sync {
    fn seal(&self, value: &Value) -> Result<Vec<u8>>;
    fn unseal(&self, value: &[u8]) -> Result<Value>;
    fn encode_user_record(&self, value: &Value) -> Result<Vec<u8>>;
}

#[cfg(test)]
mod tests;
