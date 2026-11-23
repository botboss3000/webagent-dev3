//! Namespaced documents (`docs` rows) on the storage handle: read, list, delete
//! and write, with the Chat lifecycle checks and run-input recovery split that
//! every write applies. Run documents also update the Engine's run index, which
//! lives in the root crate, so `put` refuses them here and the root `Store::put`
//! writes them.
use crate::{now, StoreCore};
use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use webagent_storage::{docs, recovery, revisions, routing};

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
    fn session_record_in<'a>(
        &'a self,
        connection: &'a mut sqlx::SqliteConnection,
        sid: &'a str,
    ) -> impl std::future::Future<Output = Result<Option<Value>>> + Send + 'a {
        StoreCore::session_record_in(self, connection, sid)
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
    fn session_record_in<'a>(
        &'a self,
        connection: &'a mut sqlx::SqliteConnection,
        sid: &'a str,
    ) -> impl std::future::Future<Output = Result<Option<Value>>> + Send + 'a {
        StoreCore::session_record_in(self, connection, sid)
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
        let prepared;
        let value = if key.starts_with("session:") {
            prepared =
                webagent_chat::session::prepare_session_document(self, tx, key, value, now())
                    .await?;
            &prepared
        } else if key.starts_with("run:")
            && matches!(
                value["status"].as_str(),
                Some("completed" | "cancelled" | "failed")
            )
        {
            // Every writer of a finished run stores it without its input copy.
            prepared = recovery::finished_run(value);
            &prepared
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
    /// recovery input in one snapshot. Only documents with a write counter (session records,
    /// session modes and ability states, run records with their input) are reused, and only
    /// while that counter proves them unchanged (`counted_document`); the loop process shares them.
    pub async fn get(&self, key: &str) -> Result<Option<Value>> {
        self.validate_agent_doc_key(key)?;
        if revisions::counted(key).is_some() {
            return self.counted_document(key).await;
        }
        let mut tx = self.pool_for(key).begin().await?;
        let result = match docs::read(&mut tx, key).await? {
            Some(raw) => Some(self.decode_document(&mut tx, &raw).await?),
            None => None,
        };
        tx.commit().await?;
        Ok(result)
    }

    /// The session record `session:{sid}` (see [`Self::get`]).
    pub async fn session_record(&self, sid: &str) -> Result<Option<Value>> {
        self.counted_document(&format!("session:{sid}")).await
    }

    /// A counted document (`revisions::counted`). One indexed read of its write counter (one
    /// statement, so one committed snapshot) answers from this process's copy when the copy
    /// was read at that counter; otherwise the counter and the document are read in one read
    /// transaction, decoded, and kept under that counter. Every write of the document moves
    /// the counter inside the writing transaction (`webagent_storage::revisions` triggers), so
    /// an equal counter is the same committed document; a missing or unreadable counter reads
    /// the document again and keeps nothing.
    async fn counted_document(&self, key: &str) -> Result<Option<Value>> {
        let pool = self.pool_for(key);
        if let Some(counter) = revisions::document_counter(pool, key).await {
            if let Some(value) = self.kept_document(key, counter) {
                return Ok(Some((*value).clone()));
            }
        }
        let mut tx = pool.begin().await?;
        let counter = revisions::document_counter(&mut *tx, key).await;
        let result = match docs::read(&mut tx, key).await? {
            Some(raw) => Some(self.decode_document(&mut tx, &raw).await?),
            None => None,
        };
        tx.commit().await?;
        if let (Some(counter), Some(value)) = (counter, &result) {
            self.keep_document(key, counter, value.clone());
        }
        Ok(result)
    }

    /// The session record read inside the caller's transaction `tx`, so it answers for that
    /// transaction's snapshot exactly as `SELECT value FROM docs` there would (including the
    /// transaction's own uncommitted writes, which move the counter away from any kept copy).
    /// Uses a kept copy only when the counter read in `tx` equals its tag; never keeps what it
    /// reads, since `tx` may still roll back.
    pub async fn session_record_in(
        &self,
        connection: &mut sqlx::SqliteConnection,
        sid: &str,
    ) -> Result<Option<Value>> {
        let key = format!("session:{sid}");
        if let Some(counter) = revisions::document_counter(&mut *connection, &key).await {
            if let Some(value) = self.kept_document(&key, counter) {
                return Ok(Some((*value).clone()));
            }
        }
        match docs::read(connection, &key).await? {
            Some(raw) => Ok(Some(self.unseal(&raw)?)),
            None => Ok(None),
        }
    }

    fn kept_document(&self, key: &str, counter: i64) -> Option<std::sync::Arc<Value>> {
        self.context_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .counted_records
            .get(&self.context_cache_key(key), counter)
    }

    /// Keep a document read in one committed snapshot together with `counter`.
    fn keep_document(&self, key: &str, counter: i64, value: Value) {
        self.context_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .counted_records
            .insert(
                &self.context_cache_key(key),
                counter,
                std::sync::Arc::new(value),
            );
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

    /// How many documents [`Self::list`] would return for `prefix`, without decoding them.
    pub async fn count(&self, prefix: &str) -> Result<usize> {
        self.validate_agent_doc_key(prefix)?;
        Ok(usize::try_from(
            docs::count(self.pool_for(prefix), prefix).await?,
        )?)
    }

    /// The last `limit` model interactions of session `sid`, oldest first, as
    /// `(seq, run_id, item)`. Callers check session authority first.
    pub async fn session_interactions(
        &self,
        sid: &str,
        limit: i64,
    ) -> Result<Vec<(i64, String, Value)>> {
        let rows: Vec<(i64, String, Vec<u8>)> = sqlx::query_as(
            "SELECT seq,run_id,item FROM interactions WHERE session_id=? ORDER BY seq DESC LIMIT ?",
        )
        .bind(sid)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .rev()
            .map(|(seq, run, item)| Ok((seq, run, self.unseal(&item)?)))
            .collect()
    }

    /// Drop the document's cache entry and, for a session, its assembled context.
    pub fn invalidate_document(&self, key: &str) {
        if let Some(sid) = key.strip_prefix("session:") {
            self.context_cache
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .invalidate(&self.context_cache_key(sid));
        }
    }
}
