//! Namespaced documents (`docs` rows) on the storage handle: read, list, delete
//! and write, with the Chat lifecycle checks and run-input recovery split that
//! every write applies. Run documents also update the Engine's run index, which
//! lives in the root crate, so `put` refuses them here and the root `Store::put`
//! writes them.
use crate::{now, StoreCore};
use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use webagent_storage::{docs, recovery, routing};

impl webagent_storage::ScopedCodec for StoreCore {
    fn seal(&self, value: &Value) -> Result<Vec<u8>> {
        StoreCore::seal(self, value)
    }
    fn unseal(&self, data: &[u8]) -> Result<Value> {
        StoreCore::unseal(self, data)
    }
    fn encode_user_record(&self, value: &Value) -> Result<Vec<u8>> {
        StoreCore::encode_user_record(self, value)
    }
}

impl webagent_chat::RecordCodec for StoreCore {
    fn seal(&self, value: &Value) -> Result<Vec<u8>> {
        StoreCore::seal(self, value)
    }
    fn unseal(&self, value: &[u8]) -> Result<Value> {
        StoreCore::unseal(self, value)
    }
    fn encode_user_record(&self, value: &Value) -> Result<Vec<u8>> {
        StoreCore::encode_user_record(self, value)
    }
}

// Access borrows the existing cipher, identities and pools; no resource is duplicated.
impl webagent_access::storage::RecordCodec for StoreCore {
    fn seal(&self, value: &Value) -> Result<Vec<u8>> {
        StoreCore::seal(self, value)
    }
    fn unseal(&self, value: &[u8]) -> Result<Value> {
        StoreCore::unseal(self, value)
    }
    fn encode_application(&self, value: &Value) -> Result<Vec<u8>> {
        self.encode_for_pool(&self.app_pool, value)
    }
    fn seal_agent(&self, value: &Value) -> Result<Vec<u8>> {
        StoreCore::seal_agent(self, value)
    }
    fn unseal_agent(&self, value: &[u8]) -> Result<Value> {
        StoreCore::unseal_agent(self, value)
    }
    fn seal_agent_for(&self, agent: &str, value: &Value) -> Result<Vec<u8>> {
        StoreCore::seal_agent_for(self, agent, value)
    }
    fn unseal_agent_for(&self, agent: &str, value: &[u8]) -> Result<Value> {
        StoreCore::unseal_agent_for(self, agent, value)
    }
}

impl StoreCore {
    /// Reconstruct a split recovery record under the reader's transaction.
    pub async fn decode_document(
        &self,
        connection: &mut sqlx::SqliteConnection,
        raw: &[u8],
    ) -> Result<Value> {
        recovery::read_document(connection, self, raw).await
    }

    /// Persist a document and its recovery-input delta in one transaction.
    /// Unchanged items retain their ciphertext; append, replacement, compaction,
    /// and truncation update only affected rows. A metadata reference is published
    /// only alongside its complete item set. No transcript rows are deleted.
    pub async fn write_document(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        key: &str,
        value: &Value,
    ) -> Result<()> {
        if key.starts_with("run:")
            && !matches!(
                value["status"].as_str(),
                Some("completed" | "cancelled" | "failed")
            )
        {
            let sid = value["session_id"]
                .as_str()
                .ok_or_else(|| anyhow!("Run has no session"))?;
            webagent_chat::session::require_active_session(self, tx, sid).await?;
            if !matches!(value["status"].as_str(), Some("stopped" | "recovery")) {
                webagent_chat::shared::require_participation_in(self, tx, value).await?;
            }
        }
        let session_value;
        let value = if key.starts_with("session:") {
            session_value =
                webagent_chat::session::prepare_session_document(self, tx, key, value, now())
                    .await?;
            &session_value
        } else {
            value
        };
        let metadata;
        let stored = if key.starts_with("run:") && value["input"].is_array() {
            metadata = recovery::save_run_input(tx, self, key, value).await?;
            &metadata
        } else {
            value
        };
        let encoded = if routing::always_sealed(key) {
            self.seal(stored)?
        } else {
            self.encode_for_pool(self.pool_for(key), stored)?
        };
        docs::upsert(tx, key, &encoded).await?;
        Ok(())
    }

    /// Read and authenticate a namespace-selected document, reconstructing split
    /// recovery input in one snapshot. Never cached: the loop process shares them.
    pub async fn get(&self, key: &str) -> Result<Option<Value>> {
        self.validate_agent_doc_key(key)?;
        let mut tx = self.pool_for(key).begin().await?;
        let result = match docs::read(&mut tx, key).await? {
            Some(raw) => Some(self.decode_document(&mut tx, &raw).await?),
            None => None,
        };
        tx.commit().await?;
        Ok(result)
    }

    /// Write a non-run document through its lifecycle guards, then invalidate the
    /// affected disposable caches. Run documents go through the root `Store::put`.
    pub async fn put(&self, key: &str, value: &Value) -> Result<()> {
        if key.starts_with("run:") {
            bail!("Run documents are written through the run index");
        }
        self.validate_agent_doc_key(key)?;
        let session_id = key.strip_prefix("session:");
        let _context_guard = match session_id {
            Some(_) => Some(self.context_gate.write().await),
            None => None,
        };
        let mut tx = self.pool_for(key).begin().await?;
        self.write_document(&mut tx, key, value).await?;
        tx.commit().await?;
        self.invalidate_document(key);
        Ok(())
    }

    /// Delete a namespace document and any owned recovery rows together before
    /// invalidating caches.
    pub async fn delete(&self, key: &str) -> Result<()> {
        self.validate_agent_doc_key(key)?;
        let _context_guard = match key.strip_prefix("session:") {
            Some(_) => Some(self.context_gate.write().await),
            None => None,
        };
        let mut tx = self.pool_for(key).begin().await?;
        docs::remove(&mut tx, key).await?;
        tx.commit().await?;
        self.invalidate_document(key);
        Ok(())
    }

    /// Read matching authority documents in one snapshot and reconstruct any split
    /// run inputs.
    pub async fn list(&self, prefix: &str) -> Result<Vec<Value>> {
        self.validate_agent_doc_key(prefix)?;
        let mut tx = self.pool_for(prefix).begin().await?;
        let rows = docs::scan(&mut tx, prefix).await?;
        let mut values = Vec::with_capacity(rows.len());
        for raw in rows {
            values.push(self.decode_document(&mut tx, &raw).await?);
        }
        tx.commit().await?;
        Ok(values)
    }

    /// Drop the document's cache entry and, for a session, its assembled context.
    pub fn invalidate_document(&self, key: &str) {
        self.cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .invalidate(&[key.into()]);
        if let Some(sid) = key.strip_prefix("session:") {
            self.context_cache
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .invalidate(&self.context_cache_key(sid));
        }
    }
}
