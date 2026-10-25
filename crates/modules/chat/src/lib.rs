//! Conversation lifecycle, participation, sharing, history, controls, follow-ups, Main Chat and session naming.
//! Configuration stays in existing `config/chat.json` and `config/prompts.json`; no new JSON files or daemons are introduced.
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
use sha2::{Digest, Sha256};

/// Existing Storage codec contract; Chat never manages encryption keys or connection pools.
pub trait RecordCodec: Sync {
    fn seal(&self, value: &Value) -> Result<Vec<u8>>;
    fn unseal(&self, value: &[u8]) -> Result<Value>;
    fn encode_user_record(&self, value: &Value) -> Result<Vec<u8>> {
        self.seal(value)
    }
}

pub fn digest<T: serde::Serialize + ?Sized>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).expect("JSON digest serialization");
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests;
