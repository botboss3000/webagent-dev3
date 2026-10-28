//! A `StoreCore` over three fresh SQLite files for host crates' storage tests.
use crate::StoreCore;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use webagent_storage::cache::{Cache, ContextCache};
use webagent_storage::codec::{AgentKeys, Codec};

/// Open a handle for `user` and agent `shared_default` under `root`, with a fixed
/// fixture key and an `agents` table so agent keys can be initialized. Root-only
/// schema (notification tables) is not created.
pub async fn store_core(root: &Path, user: &str) -> StoreCore {
    // Event/run tables per plane as root `Store::open` creates them.
    let open = |name: &str, events: bool, runs: bool| {
        let path = root.join(name);
        async move {
            webagent_storage::pool::open_pool(&path, events, runs)
                .await
                .expect("fixture pool")
        }
    };
    let agent_pool = open("agent.db", false, false).await;
    sqlx::raw_sql(
        "CREATE TABLE agents(id TEXT PRIMARY KEY, metadata BLOB, revision INTEGER NOT NULL)",
    )
    .execute(&agent_pool)
    .await
    .expect("fixture agents table");
    StoreCore {
        app_pool: open("app.db", true, false).await,
        pool: open("user.db", true, true).await,
        agent_pool,
        root: root.to_owned(),
        user_id: user.to_owned(),
        agent_id: "shared_default".to_owned(),
        user_states: Arc::default(),
        agent_states: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        codec: Codec::new(&[7; 32]).expect("fixture key"),
        agent_cipher: None,
        agent_keys: AgentKeys::default(),
        cache: Arc::new(Mutex::new(Cache::new(16))),
        context_cache: Arc::new(Mutex::new(ContextCache::new(16))),
        context_gate: Arc::default(),
    }
}
