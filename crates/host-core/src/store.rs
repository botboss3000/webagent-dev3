//! Resources of one storage handle and the primitives that touch only them.
//!
//! The root `Store` owns opening, migrations and every record API that crosses
//! modules; it wraps this type, so these fields stay reachable as `store.pool` etc.
use anyhow::{anyhow, Result};
use serde_json::Value;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;
use webagent_storage::cache::{Cache, ContextCache};
use webagent_storage::codec::{AgentCipher, AgentKeys, Codec};
use webagent_storage::pool::UserPlane;
use webagent_storage::routing::Planes;

/// One storage view: the three authority pools bound to a user and an agent, the
/// codec and agent keys, and the shared disposable caches. Cloning shares everything.
///
/// Fields are public because the root `Store` reads them directly; module host
/// crates should prefer the methods.
#[derive(Clone)]
pub struct StoreCore {
    pub app_pool: SqlitePool,
    pub pool: SqlitePool,
    pub agent_pool: SqlitePool,
    pub root: PathBuf,
    pub user_id: String,
    pub agent_id: String,
    pub user_states: Arc<RwLock<HashMap<String, UserPlane>>>,
    pub agent_states: Arc<RwLock<HashMap<String, SqlitePool>>>,
    pub codec: Codec,
    // Store views share immutable expanded keys instead of copying large key schedules into futures.
    pub agent_cipher: Option<AgentCipher>,
    pub agent_keys: AgentKeys,
    pub cache: Arc<Mutex<Cache>>,
    pub context_cache: Arc<Mutex<ContextCache>>,
    pub context_gate: Arc<RwLock<()>>,
}

// Small accessors are `#[inline]` so release builds keep inlining them across
// the crate boundary without LTO, as they did inside the root crate.
impl StoreCore {
    /// Expose the agent identity already bound to this storage handle.
    #[inline]
    pub fn current_agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Expose the canonical account identity already bound to this storage handle.
    #[inline]
    pub fn current_user_id(&self) -> &str {
        &self.user_id
    }

    // Only user-plane writers may call this. seal() remains encrypted for
    // application/agent secrets and authenticated search continuation cursors.
    #[inline]
    pub fn encode_user_record(&self, value: &Value) -> Result<Vec<u8>> {
        self.codec
            .encode_user_record(&self.root, &self.user_id, value)
    }

    /// Apply optional administrator plaintext policy only to user-plane records; other authorities always seal.
    pub fn encode_for_pool(&self, pool: &SqlitePool, value: &Value) -> Result<Vec<u8>> {
        if std::ptr::eq(pool, &self.agent_pool) {
            self.seal_agent(value)
        } else if std::ptr::eq(pool, &self.pool) {
            self.encode_user_record(value)
        } else {
            self.seal(value)
        }
    }

    /// Encode a JSON value with a fresh AES-GCM nonce and authenticated ciphertext.
    #[inline]
    pub fn seal(&self, value: &Value) -> Result<Vec<u8>> {
        self.codec.seal(value)
    }

    /// Authenticate and decode agent, encrypted or explicitly versioned administrator plaintext records.
    pub fn unseal(&self, data: &[u8]) -> Result<Value> {
        if webagent_storage::codec::is_agent_envelope(data) {
            return self.unseal_agent(data);
        }
        self.codec.unseal(data)
    }

    /// Reset the shared disposable caches, applying limits to existing and future
    /// user handles alike. Database pools and active request copies are not covered.
    pub fn configure_cache(&self, documents: usize, contexts: usize) {
        // Both caches are replaced wholesale, so a poisoned lock is simply rebuilt.
        *self.cache.lock().unwrap_or_else(|error| error.into_inner()) = Cache::new(documents);
        self.cache.clear_poison();
        *self
            .context_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = ContextCache::new(contexts);
        self.context_cache.clear_poison();
    }

    /// Qualify disposable context-cache entries by account so identical session IDs cannot collide.
    #[inline]
    pub fn context_cache_key(&self, sid: &str) -> String {
        format!("{}:{sid}", self.user_id)
    }

    /// Reject document access naming an agent other than this handle owns.
    #[inline]
    pub fn validate_agent_doc_key(&self, key: &str) -> Result<()> {
        webagent_storage::routing::validate_agent_doc_key(&self.agent_id, key)
    }

    /// Borrow this view's three authorities for Storage routing.
    #[inline]
    pub fn planes(&self) -> Planes<'_> {
        Planes {
            application: &self.app_pool,
            user: &self.pool,
            agent: &self.agent_pool,
        }
    }

    /// Route each known namespace to its user, agent or application authority.
    #[inline]
    pub fn pool_for(&self, key: &str) -> &SqlitePool {
        self.planes().for_key(key)
    }

    // Each agent has a random data key; only the trusted storage boundary unwraps it.
    // Storage owns the envelope format and key conversion; this view selects the key.

    /// Seal with the bound agent's key.
    pub fn seal_agent(&self, value: &Value) -> Result<Vec<u8>> {
        self.seal_agent_for(&self.agent_id, value)
    }

    /// Select the bound view's key first, then the shared registry.
    fn agent_key(&self, agent: &str) -> Result<AgentCipher> {
        let keys = self
            .agent_keys
            .read()
            .map_err(|_| anyhow!("Agent key registry unavailable"))?;
        if agent == self.agent_id {
            self.agent_cipher.as_ref().or_else(|| keys.get(agent))
        } else {
            keys.get(agent)
        }
        .cloned()
        .ok_or_else(|| anyhow!("Agent key unavailable"))
    }

    /// Seal with a named agent's key.
    pub fn seal_agent_for(&self, agent: &str, value: &Value) -> Result<Vec<u8>> {
        self.agent_key(agent)?.seal(agent, value)
    }

    /// Open a record sealed with the bound agent's key.
    pub fn unseal_agent(&self, data: &[u8]) -> Result<Value> {
        self.unseal_agent_for(&self.agent_id, data)
    }

    /// Open a record sealed with a named agent's key.
    pub fn unseal_agent_for(&self, agent: &str, data: &[u8]) -> Result<Value> {
        if data.len() < 36 || !webagent_storage::codec::is_agent_envelope(data) {
            return Err(anyhow!("Invalid agent record envelope"));
        }
        self.agent_key(agent)?.unseal(agent, data)
    }

    /// Bind this view's agent key, creating and registering it on first use.
    pub async fn initialize_agent_encryption(&mut self) -> Result<()> {
        let registered = self
            .agent_keys
            .read()
            .map_err(|_| anyhow!("Agent key registry unavailable"))?
            .get(&self.agent_id)
            .cloned();
        if let Some(cipher) = registered {
            self.agent_cipher = Some(cipher);
            return Ok(());
        }
        let cipher = webagent_storage::codec::initialize_agent_key(
            &self.agent_pool,
            &self.codec,
            &self.agent_id,
        )
        .await?;
        self.agent_cipher = Some(cipher.clone());
        self.agent_keys
            .write()
            .map_err(|_| anyhow!("Agent key registry unavailable"))?
            .insert(self.agent_id.clone(), cipher);
        Ok(())
    }
}
