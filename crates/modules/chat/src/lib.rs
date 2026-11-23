//! Conversation lifecycle, participation, sharing, history, controls, follow-ups, Main Chat and session naming.
//! Configuration stays in existing `config/chat.json` and `config/configuration/prompts`; no new JSON files or daemons are introduced.
//! Root-bound adapters (Store/App, HTTP routes) live in `host/` and are compiled by the root binary.
pub mod agent_config;
pub mod configuration;
pub mod followup;
pub mod history;
pub mod main_chat;
pub mod messaging;
pub mod naming;
pub mod session;
pub mod shared;
pub mod task_list;

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
    /// The session record `session:{sid}` as `connection` sees it (in a transaction: that
    /// transaction's snapshot, its own writes included). A storage handle that keeps decrypted
    /// session records answers from a copy only when the record's write counter, read on the
    /// same connection, equals the copy's (stream B6); this default reads and decrypts.
    fn session_record_in<'a>(
        &'a self,
        connection: &'a mut sqlx::SqliteConnection,
        sid: &'a str,
    ) -> impl std::future::Future<Output = Result<Option<Value>>> + Send + 'a {
        async move {
            let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
                .bind(format!("session:{sid}"))
                .fetch_optional(connection)
                .await?;
            raw.map(|raw| self.unseal(&raw)).transpose()
        }
    }
}

#[cfg(test)]
mod tests;
