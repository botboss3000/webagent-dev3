//! Durable storage for the three owners: application, agent, and user.
//! A run revision and a time-limited lease stop stale processes from replacing
//! newer progress. See docs/code-tour.md before following the SQL below.
pub(crate) mod session;
pub(crate) mod conversation_history;
mod agent_pages;
mod app_pages;
pub(crate) mod search;
pub(crate) mod own_skills;
pub(crate) mod base_task;
mod activity;
mod execution_metadata;
mod naming;
mod background_tasks;
mod background_followup;
mod followup_queue;
mod session_messaging;
mod reasoning_cleanup;
pub(crate) mod notifications;
pub(crate) mod shared_chat;
pub(crate) mod participation;
mod shared_read;
mod requester_quota;
mod actions;
mod module_records;
mod main_notes;
pub(crate) mod context_assets;
mod engine;

use crate::access;
use crate::cache::{Cache, ContextCache};
use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, bail, Context, Result};
use rand::RngCore;
use sha2::{Digest, Sha256};
use serde_json::{json, Value};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteRow, SqliteSynchronous},
    Row, SqlitePool,
};
use std::{collections::HashMap, sync::{Arc, Mutex}};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    str::FromStr,
};
use tokio::sync::RwLock;

// RECOVERY-MANIFEST: hashes are committed inside AEAD-sealed metadata, never
// trusted from a caller or an unencrypted side table. Pair content and ciphertext
// digests to compare current input without decrypting unchanged retained rows.
fn recovery_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

// RECOVERY-HASH-STREAM: serde writes canonical compact JSON directly into the
// digest. No history-sized temporary JSON buffer is allocated for comparison.
// The digest must match recovery_digest(serde_json::to_vec(item)) exactly.
fn recovery_item_digest(item: &Value) -> Result<String> {
    struct HashWriter(Sha256);
    impl std::io::Write for HashWriter {
        /// Stream canonical JSON bytes into the recovery digest without retaining a history-sized buffer.
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        /// Finish the Write contract; this digest writer has no buffered bytes to flush.
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, item)?;
    Ok(format!("{:x}", writer.0.finalize()))
}

/// Optional extension of the v1 split format. Old records validate by decryption;
/// malformed manifests fail closed instead of silently falling back to that path.
fn recovery_manifest(reference: &Value, count: usize) -> Result<Option<Vec<(String, String)>>> {
    match reference.get("manifest") {
        None => Ok(None),
        Some(value) => {
            let pairs: Vec<(String, String)> = serde_json::from_value(value.clone())
                .map_err(|_| anyhow!("Invalid recovery input manifest"))?;
            if pairs.len() != count || pairs.iter().any(|(plain, sealed)|
                [plain, sealed].iter().any(|hash| hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()))) {
                return Err(anyhow!("Invalid recovery input manifest"));
            }
            Ok(Some(pairs))
        }
    }
}

/// Project retrieval diagnostics onto fixed content-free status and stage enums.

const PROMPT_SLOT_NAME_MAX: usize = 128;
const PROMPT_SLOT_CONTENT_MAX: usize = 16 * 1024;
const PROMPT_SLOT_COUNT_MAX: usize = 32;
const PROMPT_SLOT_TOTAL_MAX: usize = 64 * 1024;

#[derive(Clone)]
pub struct Store {
    pub app_pool: SqlitePool,
    pub pool: SqlitePool,
    pub agent_pool: SqlitePool,
    pub(crate) root: PathBuf,
    user_id: String,
    agent_id: String,
    user_states: Arc<RwLock<HashMap<String, UserState>>>,
    agent_states: Arc<RwLock<HashMap<String, SqlitePool>>>,
    cipher: Aes256Gcm,
    cache: Arc<Mutex<Cache>>,
    context_cache: Arc<Mutex<ContextCache>>,
    context_gate: Arc<RwLock<()>>,
}

#[derive(Clone)]
struct UserState {
    pool: SqlitePool,
    cache: Arc<Mutex<Cache>>,
    context_cache: Arc<Mutex<ContextCache>>,
    context_gate: Arc<RwLock<()>>,
}
impl UserState {
    /// Keep one user pool and its cache invalidation gate shared across Store clones.
    fn new(pool: SqlitePool) -> Self {
        Self {pool, cache:Arc::new(Mutex::new(Cache::new(64*1024*1024))),
            context_cache:Arc::new(Mutex::new(ContextCache::new(16*1024*1024))),
            context_gate:Arc::new(RwLock::new(()))}
    }
}

/// Publish one complete private encryption key atomically; never replace a missing key for existing databases.
fn load_or_create_key(root: &Path) -> Result<Vec<u8>> {
    let keypath = root.join("encryption.key");
    let key = match std::fs::read(&keypath) {
        Ok(key) => key,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if [
                "runtime.sqlite",
                "runtime.sqlite-wal",
                "runtime.sqlite-shm",
                "db/app.db",
                "user_data/admin/admin.db",
                "agent_data/shared_default/shared_default.db",
            ]
            .iter()
            .any(|name| root.join(name).exists())
            {
                return Err(anyhow!(
                    "Storage encryption key is missing for existing database"
                ));
            }
            let mut key = vec![0; 32];
            rand::thread_rng().fill_bytes(&mut key);
            let temp_path = root.join(format!(".encryption.key.{}.tmp", uuid::Uuid::new_v4()));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp_path)?;
            let published = (|| -> Result<bool> {
                file.write_all(&key)?;
                file.sync_all()?;
                match std::fs::hard_link(&temp_path, &keypath) {
                    Ok(()) => {
                        File::open(root)?.sync_all()?;
                        Ok(true)
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
                    Err(e) => Err(e.into()),
                }
            })();
            drop(file);
            let _ = std::fs::remove_file(&temp_path);
            if published? {
                key
            } else {
                std::fs::read(&keypath)?
            }
        }
        Err(e) => return Err(e.into()),
    };
    if key.len() != 32 {
        return Err(anyhow!("Invalid storage encryption key"));
    }
    Ok(key)
}

/// Open a durable WAL authority with bounded connections, profile permissions and the tables its owner needs.
pub(crate) async fn open_pool(path: &Path, events: bool, runs: bool) -> Result<SqlitePool> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("Database path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    // Checkout data remains editable by the administrator worker. Only other
    // user/agent profiles keep private modes; fixed admin profiles are exceptions.
    let family = parent.parent().and_then(Path::file_name);
    let profile = parent.file_name();
    let private_profile = (family == Some(std::ffi::OsStr::new("user_data"))
        && profile != Some(std::ffi::OsStr::new("admin")))
        || (family == Some(std::ffi::OsStr::new("agent_data"))
            && profile != Some(std::ffi::OsStr::new("shared_default")));
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(if private_profile { 0o700 } else { 0o770 }))?;
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Full)
        .busy_timeout(std::time::Duration::from_secs(5));
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        // IDLE-POOL-BOUND: release idle connections without closing pools that
        // active Store clones still use. Reconnection remains transparent.
        .min_connections(0)
        .idle_timeout(std::time::Duration::from_secs(60))
        .max_connections(4)
        .connect_with(options)
        .await?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(if private_profile { 0o600 } else { 0o660 }))?;
    sqlx::query("CREATE TABLE IF NOT EXISTS docs (key TEXT PRIMARY KEY, value BLOB NOT NULL)")
        .execute(&pool)
        .await?;
    if events {
        notifications::schema(&pool, runs).await?;
        sqlx::raw_sql("CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, kind TEXT NOT NULL, payload BLOB NOT NULL);
            CREATE INDEX IF NOT EXISTS events_session ON events(session_id,seq);
            CREATE TABLE IF NOT EXISTS legacy_event_map(legacy_seq INTEGER PRIMARY KEY,new_seq INTEGER NOT NULL UNIQUE);")
            .execute(&pool).await?;
    }
    if events && !runs {
        sqlx::raw_sql("CREATE TABLE IF NOT EXISTS loop_heartbeat(
            singleton INTEGER PRIMARY KEY CHECK(singleton=1),owner TEXT NOT NULL,
            protocol_version INTEGER NOT NULL CHECK(protocol_version=1),updated_at INTEGER NOT NULL)")
            .execute(&pool).await?;
    }
    if runs {
        sqlx::raw_sql("CREATE TABLE IF NOT EXISTS action_lock(singleton INTEGER PRIMARY KEY CHECK(singleton=1),revision INTEGER NOT NULL); INSERT OR IGNORE INTO action_lock VALUES(1,0); CREATE TABLE IF NOT EXISTS action_index(id TEXT PRIMARY KEY,resource TEXT NOT NULL,intent_key TEXT NOT NULL,state TEXT NOT NULL); CREATE INDEX IF NOT EXISTS action_intent ON action_index(resource,intent_key);").execute(&pool).await?;
        sqlx::raw_sql("CREATE TABLE IF NOT EXISTS run_index(id TEXT PRIMARY KEY,session_id TEXT NOT NULL,status TEXT NOT NULL,updated_at INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS runs_status ON run_index(status,updated_at);
            CREATE INDEX IF NOT EXISTS runs_session ON run_index(session_id,updated_at);
            CREATE TABLE IF NOT EXISTS interactions(seq INTEGER PRIMARY KEY AUTOINCREMENT,session_id TEXT NOT NULL,run_id TEXT NOT NULL,step TEXT NOT NULL,item BLOB NOT NULL,created_at INTEGER NOT NULL,UNIQUE(run_id,step));
            CREATE INDEX IF NOT EXISTS interactions_session ON interactions(session_id,seq);
            CREATE TABLE IF NOT EXISTS activity_boundaries(interaction_seq INTEGER PRIMARY KEY,boundary_item_id TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS chat_context_boundaries(message_id TEXT PRIMARY KEY,through_seq INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS activity_boundary_item ON activity_boundaries(boundary_item_id);
            CREATE TABLE IF NOT EXISTS chat(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,session_id TEXT NOT NULL,run_id TEXT NOT NULL,role TEXT NOT NULL CHECK(role IN (\'user\',\'assistant\')),content BLOB NOT NULL,created_at INTEGER NOT NULL,terminal_run_id TEXT UNIQUE);
            CREATE INDEX IF NOT EXISTS chat_session ON chat(session_id,seq);
            CREATE TABLE IF NOT EXISTS loop_commands(seq INTEGER PRIMARY KEY AUTOINCREMENT,command_id TEXT NOT NULL UNIQUE,run_id TEXT NOT NULL,schema_version INTEGER NOT NULL CHECK(schema_version=1),kind TEXT NOT NULL,payload BLOB NOT NULL,created_at INTEGER NOT NULL,acknowledged_at INTEGER);
            CREATE INDEX IF NOT EXISTS loop_commands_run ON loop_commands(run_id,seq);
            CREATE TABLE IF NOT EXISTS loop_events(seq INTEGER PRIMARY KEY AUTOINCREMENT,run_id TEXT NOT NULL,schema_version INTEGER NOT NULL CHECK(schema_version=1),kind TEXT NOT NULL,payload BLOB NOT NULL,created_at INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS loop_events_run ON loop_events(run_id,seq);
            CREATE TABLE IF NOT EXISTS publication_outbox(run_id TEXT PRIMARY KEY,schema_version INTEGER NOT NULL CHECK(schema_version=1),payload BLOB NOT NULL,created_at INTEGER NOT NULL);")
            .execute(&pool).await?;
        // Recovery input is encrypted item-by-item, separate from small run
        // metadata. Both are committed atomically; interactions remain untouched.
        sqlx::query("CREATE TABLE IF NOT EXISTS recovery_input(run_id TEXT NOT NULL,ordinal INTEGER NOT NULL,item BLOB NOT NULL,PRIMARY KEY(run_id,ordinal))")
            .execute(&pool).await?;
        sqlx::query("CREATE TABLE IF NOT EXISTS reasoning_cleanup(segment_id TEXT PRIMARY KEY,session_id TEXT NOT NULL,payload BLOB NOT NULL)")
            .execute(&pool).await?;
        let interaction_columns=sqlx::query("PRAGMA table_info(interactions)").fetch_all(&pool).await?;
        if !interaction_columns.iter().any(|row|row.get::<String,_>("name")=="execution_metadata") {
            sqlx::query("ALTER TABLE interactions ADD COLUMN execution_metadata BLOB").execute(&pool).await?;
        }
        let columns = sqlx::query("PRAGMA table_info(run_index)").fetch_all(&pool).await?;
        if !columns.iter().any(|r| r.get::<String,_>("name") == "enqueue_seq") {
            sqlx::query("ALTER TABLE run_index ADD COLUMN enqueue_seq INTEGER").execute(&pool).await?;
        }
        sqlx::query("UPDATE run_index SET enqueue_seq=rowid WHERE enqueue_seq IS NULL").execute(&pool).await?;
        for (name, declaration) in [
            ("lease_epoch", "INTEGER NOT NULL DEFAULT 0"),
            ("lease_owner", "TEXT"),
            ("lease_expires_at", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            if !columns.iter().any(|r| r.get::<String,_>("name") == name) {
                sqlx::query(&format!("ALTER TABLE run_index ADD COLUMN {name} {declaration}")).execute(&pool).await?;
            }
        }
    }
    Ok(pool)
}

// Both API and loop processes read the same data-root policy. A missing file
// keeps encryption on; a malformed policy stops writes instead of exposing data.
pub(crate) fn read_app_settings(root: &Path) -> Result<Value> {
    match std::fs::read(root.join("config/app-settings.json")) {
        Ok(bytes) => {
            let value: Value = serde_json::from_slice(&bytes)?;
            if !value.is_object() { return Err(anyhow!("app-settings.json must be an object")); }
            Ok(value)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e.into()),
    }
}

/// Validate the optional administrator encryption switch; absence keeps encryption enabled.
pub(crate) fn admin_encryption_enabled(settings: &Value) -> Result<bool> {
    match settings.get("admin_database_encryption") {
        None => Ok(true),
        Some(value) => value["enabled"].as_bool()
            .ok_or_else(|| anyhow!("app-settings.json requires admin_database_encryption.enabled boolean")),
    }
}

impl Store {
    /// Initialize the three storage authorities, migrations and shared per-owner handles under one encryption key.
    pub async fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        admin_encryption_enabled(&read_app_settings(root)?)?;
        let key = load_or_create_key(root)?;
        // Keep the administrator's original data path during migration.
        let admin_state = UserState::new(open_pool(&root.join("user_data/admin/admin.db"),true,true).await?);
        let mut user_states = HashMap::new();
        user_states.insert("admin".to_owned(),admin_state.clone());
        let shared_agent_pool = open_pool(&root.join("agent_data/shared_default/shared_default.db"),false,false).await?;
        let mut agent_states = HashMap::new();
        agent_states.insert("shared_default".to_owned(),shared_agent_pool.clone());
        let store = Self {
            app_pool: open_pool(&root.join("db/app.db"), true, false).await?,
            pool: admin_state.pool.clone(),
            agent_pool: shared_agent_pool,
            root: root.to_owned(),
            user_id: "admin".to_owned(),
            agent_id: "shared_default".to_owned(),
            user_states: Arc::new(RwLock::new(user_states)),
            agent_states: Arc::new(RwLock::new(agent_states)),
            cipher: Aes256Gcm::new_from_slice(&key)
                .map_err(|_| anyhow!("Invalid encryption key"))?,
            cache: admin_state.cache,
            context_cache: admin_state.context_cache,
            context_gate: admin_state.context_gate,
        };
        sqlx::raw_sql(
            "CREATE TABLE IF NOT EXISTS prompt_slots (
                slot_name TEXT PRIMARY KEY, order_index INTEGER NOT NULL,
                locked INTEGER NOT NULL CHECK(locked IN (0,1)),
                merge_mode TEXT NOT NULL CHECK(merge_mode IN ('replace','append')),
                content BLOB NOT NULL, revision INTEGER NOT NULL CHECK(revision > 0),
                updated_at INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS prompt_slots_order ON prompt_slots(order_index,slot_name);",
        )
        .execute(&store.agent_pool)
        .await?;
        store.migrate_legacy(root).await?;
        store.migrate_session_metadata().await?;
        store.init_accounts().await?;
        store.init_access_policy().await?;
        store.init_policy_tiers().await?;
        store.init_agent_tables().await?;
        store.ensure_default_agent_directory().await?;
        Ok(store)
    }
    /// Select an existing account database while sharing cache budgets and retaining active invalidation gates.
    pub async fn for_user(&self, user_id:&str) -> Result<Self> {
        if !access::valid_id(user_id) {return Err(anyhow!("Invalid account identity"));}
        if self.account_by_id(user_id).await?.is_none() {return Err(anyhow!("Account not found"));}
        let existing = self.user_states.read().await.get(user_id).cloned();
        let state = match existing {Some(state)=>state,None=>{
            let mut states=self.user_states.write().await;
            if let Some(state)=states.get(user_id) {state.clone()} else {
                // INACTIVE-USER-HANDLES: keep at most 64 idle registry entries.
                // The gate's sole owner is the registry only when no Store clone
                // can still use this user. Never evict active gates or close their
                // pools; that would split context invalidation synchronization.
                if states.len() >= 64 {
                    states.retain(|id,state| id == "admin"
                        || Arc::strong_count(&state.context_gate) > 1);
                }
                let path=self.root.join("user_data").join(user_id).join(format!("{user_id}.db"));
                let mut state=UserState::new(open_pool(&path,true,true).await?);
                // Every user shares one retained-cache budget per Store family.
                // User-qualified context keys prevent cross-account collisions.
                state.cache=self.cache.clone();
                state.context_cache=self.context_cache.clone();
                states.insert(user_id.to_owned(),state.clone()); state
            }
        }};
        let handle = Self {app_pool:self.app_pool.clone(),pool:state.pool,
            agent_pool:self.agent_pool.clone(),root:self.root.clone(),
            user_id:user_id.to_owned(),agent_id:self.agent_id.clone(),
            user_states:self.user_states.clone(),agent_states:self.agent_states.clone(),
            cipher:self.cipher.clone(),cache:state.cache,
            context_cache:state.context_cache,context_gate:state.context_gate};
        handle.migrate_session_metadata().await?;
        Ok(handle)
    }
    /// Select a registered agent database without changing the canonical conversation user authority.
    pub async fn for_agent(&self, agent_id:&str) -> Result<Self> {
        if !access::valid_agent_id(agent_id) {
            return Err(anyhow!("Invalid agent identity"));
        }
        if agent_id!="shared_default" {
            let found:Option<String>=sqlx::query_scalar(
                "SELECT id FROM agent_directory WHERE id=?")
                .bind(agent_id).fetch_optional(&self.app_pool).await?;
            if found.is_none() {return Err(anyhow!("Agent not found"));}
        }
        let existing=self.agent_states.read().await.get(agent_id).cloned();
        let pool=match existing {Some(pool)=>pool,None=>{
            let mut states=self.agent_states.write().await;
            if let Some(pool)=states.get(agent_id) {pool.clone()} else {
                let pool=crate::agent_storage::open_agent_pool(&self.root,agent_id).await?;
                states.insert(agent_id.to_owned(),pool.clone()); pool
            }
        }};
        Ok(Self {app_pool:self.app_pool.clone(),pool:self.pool.clone(),
            agent_pool:pool,root:self.root.clone(),
            user_id:self.user_id.clone(),agent_id:agent_id.to_owned(),
            user_states:self.user_states.clone(),agent_states:self.agent_states.clone(),
            cipher:self.cipher.clone(),
            cache:self.cache.clone(),context_cache:self.context_cache.clone(),
            context_gate:self.context_gate.clone()})
    }
    /// Expose the agent identity already bound to this storage handle.
    pub fn current_agent_id(&self)->&str {&self.agent_id}
    /// Expose the canonical account identity already bound to this storage handle.
    pub fn current_user_id(&self)->&str {&self.user_id}
    /// Create agent-owned tables and migrate the legacy default agent and prompt slots once.
    async fn init_agent_tables(&self) -> Result<()> {
        sqlx::raw_sql("CREATE TABLE IF NOT EXISTS agents(
            id TEXT PRIMARY KEY, name TEXT NOT NULL, description TEXT NOT NULL,
            provider TEXT NOT NULL, model TEXT NOT NULL, status TEXT NOT NULL,
            metadata BLOB NOT NULL, revision INTEGER NOT NULL,
            created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS agent_connections(
            id TEXT PRIMARY KEY, agent_id TEXT NOT NULL REFERENCES agents(id),
            connection_type TEXT NOT NULL, section TEXT NOT NULL,
            enabled INTEGER NOT NULL CHECK(enabled IN (0,1)), config BLOB NOT NULL,
            revision INTEGER NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS agent_connections_agent ON agent_connections(agent_id,section);
            CREATE TABLE IF NOT EXISTS agent_prompts(
            id TEXT PRIMARY KEY, agent_id TEXT NOT NULL REFERENCES agents(id),
            slot_name TEXT NOT NULL, order_index INTEGER NOT NULL,
            lock INTEGER NOT NULL CHECK(lock IN (0,1)),
            merge_mode TEXT NOT NULL CHECK(merge_mode IN ('replace','append')),
            content BLOB NOT NULL, revision INTEGER NOT NULL, updated_at INTEGER NOT NULL,
            UNIQUE(agent_id,slot_name));
            CREATE INDEX IF NOT EXISTS agent_prompts_order ON agent_prompts(agent_id,order_index,slot_name);")
            .execute(&self.agent_pool).await?;
        let mut tx = self.agent_pool.begin().await?;
        let exists: Option<String> = sqlx::query_scalar("SELECT id FROM agents WHERE id='shared_default'")
            .fetch_optional(&mut *tx).await?;
        if exists.is_none() {
            let model_blob: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key='agent:shared_default:model'")
                .fetch_optional(&mut *tx).await?;
            let old_model = model_blob.map(|blob| self.unseal(&blob)).transpose()?
                .unwrap_or(json!({"model":"gpt-6-luna"}));
            let ability_blob: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key='agent_abilities:shared_default'")
                .fetch_optional(&mut *tx).await?;
            let old_abilities = ability_blob.map(|blob| self.unseal(&blob)).transpose()?
                .unwrap_or(json!({"revision":0,"visibility":{},"skill_visibility":{}}));
            let mut modes = serde_json::Map::new();
            if let Some(old) = old_abilities["visibility"].as_object() {
                for (id, mode) in old { if mode != "off" { modes.insert(id.clone(),mode.clone()); } }
            }
            let metadata = json!({"discovery_default":"discoverable","ability_modes":modes,
                "skill_modes":old_abilities["skill_visibility"],"tool_modes":{},"ability_access":{},
                "ability_revision":old_abilities["revision"].as_i64().unwrap_or(0).max(0)});
            let now: i64 = sqlx::query_scalar("SELECT CAST(strftime('%s','now') AS INTEGER)")
                .fetch_one(&mut *tx).await?;
            sqlx::query("INSERT INTO agents(id,name,description,provider,model,status,metadata,revision,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?)")
                .bind("shared_default").bind("WebAgent").bind("")
                .bind(old_model["provider"].as_str().unwrap_or("openai_subscription"))
                .bind(old_model["model"].as_str().unwrap_or("gpt-6-luna"))
                .bind("active").bind(self.seal(&metadata)?)
                .bind(old_abilities["revision"].as_i64().unwrap_or(0).max(0))
                .bind(now).bind(now).execute(&mut *tx).await?;
            if let Some(old) = old_abilities["visibility"].as_object() {
                for (id, mode) in old {
                    sqlx::query("INSERT INTO agent_connections(id,agent_id,connection_type,section,enabled,config,revision,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?)")
                        .bind(format!("ability:{id}")).bind("shared_default").bind(id).bind("ability")
                        .bind((mode != "off") as i64).bind(self.seal(&json!({}))?)
                        .bind(1_i64).bind(now).bind(now).execute(&mut *tx).await?;
                }
            }
            sqlx::query("INSERT INTO agent_prompts(id,agent_id,slot_name,order_index,lock,merge_mode,content,revision,updated_at)
                SELECT 'shared_default:'||slot_name,'shared_default',slot_name,order_index,locked,merge_mode,content,revision,updated_at FROM prompt_slots")
                .execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }
    /// Seed absent ability connections without overriding saved enablement choices.
    pub async fn ensure_agent_connections(&self, ability_ids: &[String]) -> Result<()> {
        let mut tx = self.agent_pool.begin().await?;
        for id in ability_ids {
            sqlx::query("INSERT OR IGNORE INTO agent_connections(
                id,agent_id,connection_type,section,enabled,config,revision,created_at,updated_at)
                VALUES(?,?,?,?,1,?,1,strftime('%s','now'),strftime('%s','now'))")
                .bind(format!("ability:{id}")).bind(&self.agent_id).bind(id).bind("ability")
                .bind(self.seal(&json!({}))?).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }
    /// Read the bound agent model assignment from its canonical agent table.
    pub async fn agent_model(&self) -> Result<Value> {
        let row = sqlx::query("SELECT provider,model FROM agents WHERE id=?")
            .bind(&self.agent_id).fetch_one(&self.agent_pool).await?;
        Ok(json!({"provider":row.get::<String,_>("provider"),"model":row.get::<String,_>("model")}))
    }
    /// Replace the bound agent provider/model assignment and advance its table revision.
    pub async fn save_agent_model(&self, value: &Value) -> Result<()> {
        sqlx::query("UPDATE agents SET provider=?,model=?,revision=revision+1,updated_at=strftime('%s','now') WHERE id=?")
            .bind(value["provider"].as_str().ok_or_else(||anyhow!("Missing provider"))?)
            .bind(value["model"].as_str().ok_or_else(||anyhow!("Missing model"))?)
            .bind(&self.agent_id)
            .execute(&self.agent_pool).await?;
        Ok(())
    }
    /// Read ability controls for the agent already selected by this handle.
    pub async fn agent_abilities(&self) -> Result<Value> {
        self.agent_abilities_for(&self.agent_id).await
    }
    /// Separate tool overrides from the agent policy; project legacy flat values without writing on reads.
    pub(crate) fn tool_result_overrides(metadata:&Value)->Result<Value> {
        let tools=if let Some(value)=metadata.get("tool_result_deduplication_tools") {
            value.as_object().context("Invalid duplicate-result tool overrides")?.clone()
        } else {
            let mut legacy=metadata["tool_result_deduplication"].as_object().cloned().unwrap_or_default();
            legacy.remove("enabled");legacy
        };
        if tools.len()>1024 || tools.values().any(|value|!value.is_boolean()) {
            bail!("Invalid duplicate-result tool overrides");
        }
        Ok(Value::Object(tools))
    }
    /// Project current encrypted agent metadata and disabled connections into effective ability controls.
    pub async fn agent_abilities_for(&self, agent_id: &str) -> Result<Value> {
        if agent_id != self.agent_id {return Err(anyhow!("Agent view mismatch"));}
        let row = sqlx::query("SELECT metadata,revision FROM agents WHERE id=?")
            .bind(agent_id).fetch_one(&self.agent_pool).await?;
        let metadata = self.unseal(&row.get::<Vec<u8>,_>("metadata"))?;
        let mut visibility = metadata["ability_modes"].as_object().cloned().unwrap_or_default();
        let rows = sqlx::query("SELECT connection_type,enabled FROM agent_connections WHERE agent_id=? AND section='ability'")
            .bind(agent_id).fetch_all(&self.agent_pool).await?;
        for conn in rows {
            if conn.get::<i64,_>("enabled")==0 {
                visibility.insert(conn.get::<String,_>("connection_type"),json!("off"));
            }
        }
        Ok(json!({"revision":metadata["ability_revision"].as_i64().unwrap_or(0),"visibility":visibility,
            "skill_visibility":metadata["skill_modes"],"tool_modes":metadata["tool_modes"],"settings":metadata["ability_settings"],
            "tool_execution":metadata["tool_execution"].as_object().cloned().unwrap_or_default(),
            "tool_result_deduplication":Self::tool_result_overrides(&metadata)?}))
    }
    /// CAS encrypted visibility settings, connection state and their audit receipt in one agent transaction.
    pub async fn save_agent_abilities(&self, value: &Value, expected_revision: u64) -> Result<()> {
        if expected_revision==u64::MAX {return Err(anyhow!("Agent ability revision overflow"));}
        let mut tx = self.agent_pool.begin().await?;
        sqlx::query("UPDATE agents SET revision=revision WHERE id=?").bind(&self.agent_id).execute(&mut *tx).await?;
        let row = sqlx::query("SELECT metadata,revision FROM agents WHERE id=?")
            .bind(&self.agent_id).fetch_one(&mut *tx).await?;
        let mut metadata = self.unseal(&row.get::<Vec<u8>,_>("metadata"))?;
        if metadata["ability_revision"].as_u64().unwrap_or(0) != expected_revision {
            return Err(anyhow!("Agent ability revision changed"));
        }
        let modes = value["visibility"].as_object().ok_or_else(||anyhow!("Invalid ability modes"))?;
        let mut active = serde_json::Map::new();
        for (id,mode) in modes {
            if mode != "off" { active.insert(id.clone(),mode.clone()); }
            sqlx::query("INSERT INTO agent_connections(id,agent_id,connection_type,section,enabled,config,revision,created_at,updated_at)
                VALUES(?,?,?,?,?,?,1,strftime('%s','now'),strftime('%s','now'))
                ON CONFLICT(id) DO UPDATE SET enabled=excluded.enabled,revision=agent_connections.revision+1,updated_at=excluded.updated_at")
                .bind(format!("ability:{id}")).bind(&self.agent_id).bind(id).bind("ability")
                .bind((mode != "off") as i64).bind(self.seal(&json!({}))?)
                .execute(&mut *tx).await?;
        }
        metadata["ability_modes"]=Value::Object(active);
        metadata["skill_modes"]=value["skill_visibility"].clone();
        metadata["tool_modes"]=value["tool_modes"].clone();
        metadata["ability_settings"]=value["settings"].clone();
        if let Some(execution)=value.get("tool_execution") {metadata["tool_execution"]=execution.clone();}
        if let Some(settings)=value.get("tool_result_deduplication") {
            let fields=settings.as_object().context("Invalid duplicate-result tool settings")?;
            if fields.len()>1024 || fields.values().any(|value|!value.is_boolean()){bail!("Invalid duplicate-result tool settings");}
            metadata["tool_result_deduplication_tools"]=settings.clone();
        }
        metadata["ability_revision"]=json!(expected_revision + 1);
        // Visibility and its encrypted audit receipt have one agent-owned commit.
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(format!("agent:{}:ability_visibility_audit:{}",self.agent_id,expected_revision+1))
            .bind(self.seal(&json!({"agent_id":self.agent_id,"actor_id":self.current_user_id(),"revision":expected_revision+1}))?)
            .execute(&mut *tx).await?;
        sqlx::query("UPDATE agents SET metadata=?,revision=revision+1,updated_at=strftime('%s','now') WHERE id=? AND revision=?")
            .bind(self.seal(&metadata)?).bind(&self.agent_id)
            .bind(row.get::<i64,_>("revision"))
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    /// Reject document access naming an agent other than this handle owns.
    fn validate_agent_doc_key(&self,key:&str) -> Result<()> {
        if let Some(rest)=key.strip_prefix("agent:") {
            if rest!=self.agent_id && !rest.starts_with(&format!("{}:",self.agent_id)) {
                return Err(anyhow!("Agent view mismatch"));
            }
        }
        if let Some(rest)=key.strip_prefix("agent_abilities:") {
            if rest!=self.agent_id {
                return Err(anyhow!("Agent view mismatch"));
            }
        }
        Ok(())
    }
    /// Route each known namespace to its user, agent or application authority; conversation assets stay user-owned.
    fn pool_for(&self, key: &str) -> &SqlitePool {
        if key.starts_with("agent:") || key.starts_with("agent_abilities:") {
            &self.agent_pool
        } else if key.starts_with("module_state:")
            || key.starts_with("receipt:module:")
            || key.starts_with("session:")
            || key.starts_with("manager_review:")
            || key.starts_with("manager_pending:")
            || key.starts_with("followup_queue:")
            || key.starts_with("messaging_route:")
            || key.starts_with("receipt:agent_message:")
            || key.starts_with("receipt:messaging_route:")
            || key.starts_with("receipt:queue:")
            || key.starts_with("user_agent_page_preferences:")
            || key.starts_with("user_app_page_preferences:")
            || key.starts_with("message_meta:")
            || key.starts_with("shared_help:")
            || key.starts_with("receipt:shared_help:")
            || key.starts_with("receipt:participation:")
            || key.starts_with("session_mode:")
            || key.starts_with("session_chat_controls:")
            || key.starts_with("session_naming:")
            || key.starts_with("receipt:chat_controls:")
            || key.starts_with("compaction_segment:")
            || key.starts_with("session_ability_state:")
            || key.starts_with("session_context_assets:")
            || key.starts_with("action:")
            || key.starts_with("background_task:")
            || key.starts_with("run:")
            || key.starts_with("run_view:")
            || key.starts_with("administrator_receipt:")
            || key.starts_with("administrator_browser:")
            || key.starts_with("administrator_host:")
            || key.starts_with("administrator_notice:")
            || key.starts_with("attachment:")
            || key.starts_with("session_usage:")
            || key=="attachment_quota"
            || key.starts_with("message_attachments:")
            || key.starts_with("memory:")
            || key.starts_with("promotion:")
            || key.starts_with("receipt:session:")
            || key.starts_with("receipt:direct_message:")
            || key.starts_with("receipt:message:")
            || key.starts_with("receipt:run:")
            || key.starts_with("receipt:session_mode:")
            || key.starts_with("receipt:approval:")
        {
            &self.pool
        } else {
            &self.app_pool
        }
    }
    /// Reject ambiguous, reserved or oversized prompt-slot identities before agent-table access.
    fn validate_prompt_slot_name(name: &str) -> Result<()> {
        if name.is_empty()
            || name.len() > PROMPT_SLOT_NAME_MAX
            || name.trim() != name
            || name.chars().any(char::is_control)
            || name == "__skills__"
        {
            return Err(anyhow!("Invalid prompt slot name"));
        }
        Ok(())
    }
    /// Decrypt one agent prompt row into the public slot shape, rejecting nontext content.
    fn prompt_slot_from_row(&self, row: &SqliteRow) -> Result<Value> {
        let content = self.unseal(&row.get::<Vec<u8>, _>("content"))?;
        let content = content
            .as_str()
            .ok_or_else(|| anyhow!("Invalid encrypted prompt slot content"))?;
        Ok(json!({
            "slot_name": row.get::<String, _>("slot_name"),
            "order_index": row.get::<i64, _>("order_index"),
            "lock": row.get::<i64, _>("locked") != 0,
            "merge_mode": row.get::<String, _>("merge_mode"),
            "content": content,
            "revision": row.get::<i64, _>("revision"),
            "updated_at": row.get::<i64, _>("updated_at")
        }))
    }
    /// Read ordered prompt slots for the bound agent using the canonical agent-table path.
    pub async fn list_prompt_slots(&self) -> Result<Vec<Value>> {
        self.list_agent_prompt_slots(&self.agent_id).await
    }
    /// Read one bound agent ordered prompt snapshot and enforce count and total-content limits.
    pub async fn list_agent_prompt_slots(&self, agent_id: &str) -> Result<Vec<Value>> {
        if agent_id != self.agent_id {return Err(anyhow!("Agent view mismatch"));}
        let rows = sqlx::query(
            "SELECT slot_name,order_index,lock AS locked,merge_mode,content,revision,updated_at
             FROM agent_prompts WHERE agent_id=? ORDER BY order_index,slot_name",
        )
        .bind(agent_id)
        .fetch_all(&self.agent_pool)
        .await?;
        if rows.len() > PROMPT_SLOT_COUNT_MAX {
            return Err(anyhow!("Prompt slot count exceeds limit"));
        }
        let mut total = 0usize;
        let mut slots = Vec::with_capacity(rows.len());
        for row in &rows {
            let slot = self.prompt_slot_from_row(row)?;
            let length = slot["content"].as_str().unwrap().len();
            if length > PROMPT_SLOT_CONTENT_MAX {
                return Err(anyhow!("Prompt slot content exceeds limit"));
            }
            total = total
                .checked_add(length)
                .ok_or_else(|| anyhow!("Prompt slot content total exceeds limit"))?;
            if total > PROMPT_SLOT_TOTAL_MAX {
                return Err(anyhow!("Prompt slot content total exceeds limit"));
            }
            slots.push(slot);
        }
        Ok(slots)
    }
    /// Read an optional named prompt slot only inside the bound agent authority.
    pub async fn get_prompt_slot(&self, name: &str) -> Result<Option<Value>> {
        Self::validate_prompt_slot_name(name)?;
        let row = sqlx::query(
            "SELECT slot_name,order_index,lock AS locked,merge_mode,content,revision,updated_at
             FROM agent_prompts WHERE agent_id=? AND slot_name=?",
        )
        .bind(&self.agent_id).bind(name)
        .fetch_optional(&self.agent_pool)
        .await?;
        row.as_ref()
            .map(|row| self.prompt_slot_from_row(row))
            .transpose()
    }
    /// Create or revise a prompt slot through the shared validated transaction without an action receipt.
    pub async fn upsert_prompt_slot(
        &self,
        name: &str,
        content: &str,
        order_index: Option<i64>,
        lock: Option<bool>,
        merge_mode: Option<&str>,
        expected_revision: i64,
    ) -> Result<Value> {
        self.upsert_prompt_slot_receipted(name,content,order_index,lock,merge_mode,expected_revision,None).await
    }
    /// Reserve the agent writer, enforce revision/capacity bounds and commit slot plus optional retry receipt atomically.
    pub(crate) async fn upsert_prompt_slot_receipted(
        &self,name:&str,content:&str,order_index:Option<i64>,lock:Option<bool>,merge_mode:Option<&str>,
        expected_revision:i64,receipt:Option<(&str,&str)>,
    )->Result<Value> {
        Self::validate_prompt_slot_name(name)?;
        if content.len() > PROMPT_SLOT_CONTENT_MAX {
            return Err(anyhow!("Prompt slot content exceeds limit"));
        }
        if expected_revision < 0 {
            return Err(anyhow!("Invalid prompt slot revision"));
        }
        if order_index.is_some_and(|value| !(0..=1_000_000).contains(&value)) {
            return Err(anyhow!("Invalid prompt slot order"));
        }
        if merge_mode.is_some_and(|value| value != "replace" && value != "append") {
            return Err(anyhow!("Invalid prompt slot merge mode"));
        }
        let mut tx = self.agent_pool.begin().await?;
        // Reserve the writer before capacity reads; concurrent creates cannot
        // both pass the count and assembled-size checks.
        sqlx::query("UPDATE agent_prompts SET revision=revision WHERE 0")
            .execute(&mut *tx)
            .await?;
        if let Some((key,hash))=receipt {
            if let Some(record)=self.action_agent_receipt_tx(&mut tx,key).await? {
                if record["arguments_digest"]!=hash {return Err(anyhow!("Action receipt input conflict"));}
                return Ok(record["result"]["output"]["slot"].clone());
            }
        }
        let rows = sqlx::query(
            "SELECT slot_name,order_index,lock AS locked,merge_mode,content,revision,updated_at
             FROM agent_prompts WHERE agent_id=?",
        )
        .bind(&self.agent_id).fetch_all(&mut *tx)
        .await?;
        let existing = rows
            .iter()
            .find(|row| row.get::<String, _>("slot_name") == name);
        match (existing, expected_revision) {
            (None, 0) => {}
            (None, _) => return Err(anyhow!("Prompt slot not found")),
            (Some(row), revision) if revision > 0 && row.get::<i64, _>("revision") == revision => {}
            _ => return Err(anyhow!("Prompt slot revision mismatch")),
        }
        if existing.is_none() && rows.len() >= PROMPT_SLOT_COUNT_MAX {
            return Err(anyhow!("Prompt slot count exceeds limit"));
        }
        let mut total = content.len();
        for row in &rows {
            if row.get::<String, _>("slot_name") == name {
                continue;
            }
            let old = self.prompt_slot_from_row(row)?;
            let length = old["content"].as_str().unwrap().len();
            if length > PROMPT_SLOT_CONTENT_MAX {
                return Err(anyhow!("Prompt slot content exceeds limit"));
            }
            total = total
                .checked_add(length)
                .ok_or_else(|| anyhow!("Prompt slot content total exceeds limit"))?;
        }
        if total > PROMPT_SLOT_TOTAL_MAX {
            return Err(anyhow!("Prompt slot content total exceeds limit"));
        }
        let order = match order_index {
            Some(order) => order,
            None => match existing {
                Some(row) => row.get::<i64, _>("order_index"),
                None => rows
                    .iter()
                    .map(|row| row.get::<i64, _>("order_index"))
                    .max()
                    .unwrap_or(0)
                    .checked_add(10)
                    .ok_or_else(|| anyhow!("Prompt slot order exceeds limit"))?,
            },
        };
        if !(0..=1_000_000).contains(&order) {
            return Err(anyhow!("Invalid prompt slot order"));
        }
        let locked = lock
            .or_else(|| existing.map(|row| row.get::<i64, _>("locked") != 0))
            .unwrap_or(false);
        let mode = merge_mode
            .map(str::to_owned)
            .or_else(|| existing.map(|row| row.get::<String, _>("merge_mode")))
            .unwrap_or_else(|| "replace".to_owned());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as i64;
        let sealed = self.seal(&json!(content))?;
        let revision = expected_revision
            .checked_add(1)
            .ok_or_else(|| anyhow!("Invalid prompt slot revision"))?;
        if existing.is_some() {
            let changed = sqlx::query(
                "UPDATE agent_prompts SET order_index=?,lock=?,merge_mode=?,content=?,revision=?,updated_at=?
                 WHERE agent_id=? AND slot_name=? AND revision=?",
            )
            .bind(order).bind(locked as i64).bind(&mode).bind(sealed)
            .bind(revision).bind(now).bind(&self.agent_id).bind(name).bind(expected_revision)
            .execute(&mut *tx).await?.rows_affected();
            if changed != 1 {
                return Err(anyhow!("Prompt slot revision mismatch"));
            }
        } else {
            sqlx::query(
                "INSERT INTO agent_prompts(id,agent_id,slot_name,order_index,lock,merge_mode,content,revision,updated_at)
                 VALUES(?,?,?,?,?,?,?,?,?)",
            )
            .bind(format!("{}:{name}",self.agent_id)).bind(&self.agent_id).bind(name).bind(order).bind(locked as i64).bind(&mode).bind(sealed)
            .bind(revision).bind(now).execute(&mut *tx).await?;
        }
        let slot=json!({"slot_name":name,"order_index":order,"lock":locked,
            "merge_mode":mode,"content":content,"revision":revision,"updated_at":now});
        if let Some((key,hash))=receipt {
            self.action_agent_receipt_write(&mut tx,key,hash,&json!({"status":"success","output":{"slot":slot}})).await?;
        }
        tx.commit().await?;
        Ok(slot)
    }
    /// Delete one prompt slot through the same revision-fenced path used by native actions.
    pub async fn delete_prompt_slot(&self, name: &str, expected_revision: i64) -> Result<bool> {
        self.delete_prompt_slot_receipted(name,expected_revision,None).await
    }
    /// Delete only the expected slot revision and bind an optional action receipt to the same commit.
    pub(crate) async fn delete_prompt_slot_receipted(&self,name:&str,expected_revision:i64,receipt:Option<(&str,&str)>)->Result<bool> {
        Self::validate_prompt_slot_name(name)?;
        if expected_revision <= 0 {
            return Err(anyhow!("Invalid prompt slot revision"));
        }
        let mut tx = self.agent_pool.begin().await?;
        sqlx::query("UPDATE agent_prompts SET revision=revision WHERE 0").execute(&mut *tx).await?;
        if let Some((key,hash))=receipt {
            if let Some(record)=self.action_agent_receipt_tx(&mut tx,key).await? {
                if record["arguments_digest"]!=hash {return Err(anyhow!("Action receipt input conflict"));}
                return Ok(record["result"]["output"]["deleted"]==true);
            }
        }
        let actual: Option<i64> =
            sqlx::query_scalar("SELECT revision FROM agent_prompts WHERE agent_id=? AND slot_name=?")
                .bind(&self.agent_id).bind(name)
                .fetch_optional(&mut *tx)
                .await?;
        match actual {
            None => return Err(anyhow!("Prompt slot not found")),
            Some(revision) if revision != expected_revision => {
                return Err(anyhow!("Prompt slot revision mismatch"));
            }
            Some(_) => {}
        }
        let changed = sqlx::query("DELETE FROM agent_prompts WHERE agent_id=? AND slot_name=? AND revision=?")
            .bind(&self.agent_id).bind(name)
            .bind(expected_revision)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if changed != 1 {
            return Err(anyhow!("Prompt slot revision mismatch"));
        }
        if let Some((key,hash))=receipt {
            self.action_agent_receipt_write(&mut tx,key,hash,&json!({"status":"success","output":{"deleted":true,"slot_name":name}})).await?;
        }
        tx.commit().await?;
        Ok(true)
    }
    /// Copy legacy public messages idempotently and reject conflicting destination identities or contents.
    async fn migrate_legacy_chat(&self, sid: &str, session: &Value) -> Result<()> {
        let Some(messages) = session["messages"].as_array() else {
            return Ok(());
        };
        for (ordinal, message) in messages.iter().enumerate() {
            let role = message["role"]
                .as_str()
                .ok_or_else(|| anyhow!("Legacy chat role is missing"))?;
            if role != "user" && role != "assistant" {
                return Err(anyhow!("Unsupported legacy chat role"));
            }
            let content = message
                .get("content")
                .ok_or_else(|| anyhow!("Legacy chat content is missing"))?;
            let id = format!("legacy:{sid}:{ordinal}");
            let run_id = message["run_id"].as_str().unwrap_or("");
            let created_at = message["created_at"]
                .as_i64()
                .or_else(|| session["created_at"].as_i64())
                .unwrap_or(0);
            sqlx::query("INSERT OR IGNORE INTO chat(id,session_id,run_id,role,content,created_at,terminal_run_id) VALUES(?,?,?,?,?,?,NULL)")
                .bind(&id).bind(sid).bind(run_id).bind(role).bind(self.encode_user_record(content)?)
                .bind(created_at).execute(&self.pool).await?;
            let row = sqlx::query(
                "SELECT session_id,run_id,role,content,created_at FROM chat WHERE id=?",
            )
            .bind(&id)
            .fetch_one(&self.pool)
            .await?;
            if row.get::<String, _>("session_id") != sid
                || row.get::<String, _>("run_id") != run_id
                || row.get::<String, _>("role") != role
                || row.get::<i64, _>("created_at") != created_at
                || self.unseal(&row.get::<Vec<u8>, _>("content"))? != *content
            {
                return Err(anyhow!("Conflicting migrated chat record"));
            }
        }
        Ok(())
    }
    /// Preserve legacy event identity across split-storage retries without reusing another mapped sequence.
    async fn migrate_legacy_event(
        &self,
        destination: &SqlitePool,
        legacy_seq: i64,
        sid: &str,
        kind: &str,
        raw: &[u8],
        upgrading_v1: bool,
    ) -> Result<()> {
        let value = self.unseal(raw)?;
        if !upgrading_v1 {
            sqlx::query(
                "INSERT OR IGNORE INTO events(seq,session_id,kind,payload) VALUES(?,?,?,?)",
            )
            .bind(legacy_seq)
            .bind(sid)
            .bind(kind)
            .bind(raw)
            .execute(destination)
            .await?;
            let existing = sqlx::query("SELECT session_id,kind,payload FROM events WHERE seq=?")
                .bind(legacy_seq)
                .fetch_one(destination)
                .await?;
            if existing.get::<String, _>("session_id") != sid
                || existing.get::<String, _>("kind") != kind
                || self.unseal(&existing.get::<Vec<u8>, _>("payload"))? != value
            {
                return Err(anyhow!("Conflicting migrated event: {legacy_seq}"));
            }
            return Ok(());
        }
        let mut tx = destination.begin().await?;
        let mapped =
            sqlx::query_scalar::<_, i64>("SELECT new_seq FROM legacy_event_map WHERE legacy_seq=?")
                .bind(legacy_seq)
                .fetch_optional(&mut *tx)
                .await?;
        let new_seq = if let Some(seq) = mapped {
            seq
        } else {
            let sequence_claimed = sqlx::query_scalar::<_, i64>(
                "SELECT legacy_seq FROM legacy_event_map WHERE new_seq=?",
            )
            .bind(legacy_seq)
            .fetch_optional(&mut *tx)
            .await?
            .is_some();
            let same_seq = sqlx::query("SELECT session_id,kind,payload FROM events WHERE seq=?")
                .bind(legacy_seq)
                .fetch_optional(&mut *tx)
                .await?;
            let already_copied = if let Some(row) = same_seq {
                !sequence_claimed
                    && row.get::<String, _>("session_id") == sid
                    && row.get::<String, _>("kind") == kind
                    && self.unseal(&row.get::<Vec<u8>, _>("payload"))? == value
            } else {
                false
            };
            let seq = if already_copied {
                legacy_seq
            } else {
                sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,?,?)")
                    .bind(sid)
                    .bind(kind)
                    .bind(raw)
                    .execute(&mut *tx)
                    .await?
                    .last_insert_rowid()
            };
            sqlx::query("INSERT INTO legacy_event_map(legacy_seq,new_seq) VALUES(?,?)")
                .bind(legacy_seq)
                .bind(seq)
                .execute(&mut *tx)
                .await?;
            seq
        };
        let existing = sqlx::query("SELECT session_id,kind,payload FROM events WHERE seq=?")
            .bind(new_seq)
            .fetch_one(&mut *tx)
            .await?;
        if existing.get::<String, _>("session_id") != sid
            || existing.get::<String, _>("kind") != kind
            || self.unseal(&existing.get::<Vec<u8>, _>("payload"))? != value
        {
            return Err(anyhow!("Conflicting migrated event mapping: {legacy_seq}"));
        }
        tx.commit().await?;
        Ok(())
    }
    /// Copy and verify legacy documents, events and run indexes before publishing the completed split marker.
    async fn migrate_legacy(&self, root: &Path) -> Result<()> {
        let legacy_path: PathBuf = root.join("runtime.sqlite");
        let marker_version = self
            .get("storage:split_migrated")
            .await?
            .and_then(|marker| marker["version"].as_u64())
            .unwrap_or(0);
        if !legacy_path.exists() || marker_version >= 2 {
            return Ok(());
        }
        let upgrading_v1 = marker_version == 1;
        let legacy = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::from_str(&format!("sqlite://{}", legacy_path.display()))?
                    .read_only(true),
            )
            .await?;
        let result = async {
            // Each row is idempotent. The marker is written only after all three
            // authorities have received and verified their legacy records.
            let mut last_key: Option<String> = None;
            loop {
                let rows = sqlx::query("SELECT key,value FROM docs WHERE (? IS NULL OR key>?) ORDER BY key LIMIT 128")
                    .bind(&last_key).bind(&last_key).fetch_all(&legacy).await?;
                if rows.is_empty() { break; }
                for row in rows {
                    let key: String = row.get("key");
                    last_key = Some(key.clone());
                    if key == "storage:split_migrated" { continue; }
                    let raw: Vec<u8> = row.get("value");
                    let mut value = self.unseal(&raw)?;
                    if key.starts_with("session:") && value["checkpoint_input"].is_null() {
                        if let Some(context) = value["context"].as_array() {
                            value["checkpoint_input"] = json!(context);
                            value["checkpoint_through"] = json!(0);
                        }
                    }
                    let destination = self.pool_for(&key);
                    sqlx::query("INSERT OR IGNORE INTO docs(key,value) VALUES(?,?)")
                        .bind(&key).bind(self.seal(&value)?).execute(destination).await?;
                    let existing: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
                        .bind(&key).fetch_one(destination).await?;
                    if self.unseal(&existing)? != value {
                        if upgrading_v1 && (std::ptr::eq(destination, &self.app_pool)
                            || std::ptr::eq(destination, &self.agent_pool)) {
                            continue;
                        }
                        return Err(anyhow!("Conflicting migrated document: {key}"));
                    }
                    if key.starts_with("session:") {
                        let sid = key.strip_prefix("session:").unwrap();
                        self.migrate_legacy_chat(sid, &value).await?;
                    }
                }
            }
            let mut last_seq = 0i64;
            loop {
                let rows = sqlx::query("SELECT seq,session_id,kind,payload FROM events WHERE seq>? ORDER BY seq LIMIT 128")
                    .bind(last_seq).fetch_all(&legacy).await?;
                if rows.is_empty() { break; }
                for row in rows {
                    let seq: i64 = row.get("seq");
                    let sid: String = row.get("session_id");
                    let kind: String = row.get("kind");
                    let raw: Vec<u8> = row.get("payload");
                    self.unseal(&raw)?;
                    last_seq = seq;
                    let destination = if sid.is_empty() { &self.app_pool } else { &self.pool };
                    self.migrate_legacy_event(destination, seq, &sid, &kind, &raw, upgrading_v1).await?;
                }
            }
            let mut last_id: Option<String> = None;
            loop {
                let rows = sqlx::query("SELECT id,session_id,status,updated_at FROM run_index WHERE (? IS NULL OR id>?) ORDER BY id LIMIT 128")
                    .bind(&last_id).bind(&last_id).fetch_all(&legacy).await?;
                if rows.is_empty() { break; }
                for row in rows {
                    let id: String = row.get("id");
                    let sid: String = row.get("session_id");
                    let status: String = row.get("status");
                    let updated_at: i64 = row.get("updated_at");
                    last_id = Some(id.clone());
                    sqlx::query("INSERT OR IGNORE INTO run_index(id,session_id,status,updated_at) VALUES(?,?,?,?)")
                        .bind(&id).bind(&sid).bind(&status).bind(updated_at).execute(&self.pool).await?;
                    let existing = sqlx::query("SELECT session_id,status,updated_at FROM run_index WHERE id=?")
                        .bind(&id).fetch_one(&self.pool).await?;
                    if existing.get::<String,_>("session_id") != sid
                        || existing.get::<String,_>("status") != status
                        || existing.get::<i64,_>("updated_at") != updated_at
                    {
                        return Err(anyhow!("Conflicting migrated run index: {id}"));
                    }
                }
            }
            self.put("storage:split_migrated", &json!({"version":2})).await
        }.await;
        legacy.close().await;
        result
    }
    // Only user-plane writers may call this. seal() remains encrypted for
    // application/agent secrets and authenticated search continuation cursors.
    fn encode_user_record(&self, value: &Value) -> Result<Vec<u8>> {
        if self.user_id != "admin" { return self.seal(value); }
        if admin_encryption_enabled(&read_app_settings(&self.root)?)? { return self.seal(value); }
        let mut nonce = [0; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let aad = serde_json::to_vec(&json!(["v3-admin-plaintext-v1", value]))?;
        let tag = self.cipher.encrypt(Nonce::from_slice(&nonce), Payload { msg: &[], aad: &aad })
            .map_err(|_| anyhow!("Storage authentication failed"))?;
        Ok(serde_json::to_vec(&json!({"format":"v3-admin-plaintext-v1","value":value,"nonce":nonce,"tag":tag}))?)
    }
    /// Apply optional administrator plaintext policy only to user-plane records; other authorities always seal.
    fn encode_for_pool(&self, pool: &SqlitePool, value: &Value) -> Result<Vec<u8>> {
        if std::ptr::eq(pool, &self.pool) { self.encode_user_record(value) } else { self.seal(value) }
    }
    /// Encode a JSON value with a fresh AES-GCM nonce and authenticated ciphertext.
    pub fn seal(&self, value: &Value) -> Result<Vec<u8>> {
        let mut nonce = [0; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let mut data = nonce.to_vec();
        data.extend(
            self.cipher
                .encrypt(
                    Nonce::from_slice(&nonce),
                    serde_json::to_vec(value)?.as_ref(),
                )
                .map_err(|_| anyhow!("Encryption failed"))?,
        );
        Ok(data)
    }
    /// Authenticate and decode either encrypted records or the explicitly versioned administrator plaintext envelope.
    pub fn unseal(&self, data: &[u8]) -> Result<Value> {
        // A nonce in the legacy binary format can begin with '{'; only a
        // parsed, explicitly versioned envelope selects the plaintext reader.
        if let Ok(record) = serde_json::from_slice::<Value>(data) {
            if record["format"] == "v3-admin-plaintext-v1" {
                let nonce: [u8; 12] = serde_json::from_value(record["nonce"].clone())?;
                let tag: Vec<u8> = serde_json::from_value(record["tag"].clone())?;
                let value = record.get("value").ok_or_else(|| anyhow!("Invalid plaintext record"))?;
                let aad = serde_json::to_vec(&json!(["v3-admin-plaintext-v1", value]))?;
                self.cipher.decrypt(Nonce::from_slice(&nonce), Payload { msg: &tag, aad: &aad })
                    .map_err(|_| anyhow!("Storage authentication failed"))?;
                return Ok(value.clone());
            }
        }
        if data.len() < 28 {
            return Err(anyhow!("Invalid encrypted record"));
        }
        let plain = self
            .cipher
            .decrypt(Nonce::from_slice(&data[..12]), &data[12..])
            .map_err(|_| anyhow!("Storage authentication failed"))?;
        Ok(serde_json::from_slice(&plain)?)
    }
    /// Reset the shared disposable caches, applying limits to existing and future
    /// user handles alike. Database pools and active request copies are not covered.
    pub fn configure_cache(&self, documents: usize, contexts: usize) {
        *self.cache.lock().unwrap() = Cache::new(documents);
        *self.context_cache.lock().unwrap() = ContextCache::new(contexts);
    }
    /// Qualify disposable context-cache entries by account so identical session IDs cannot collide.
    fn context_cache_key(&self, sid: &str) -> String {
        format!("{}:{sid}", self.user_id)
    }
    /// Reconstruct canonical checkpoint plus eligible interactions under the context invalidation gate.
    pub async fn context(&self, sid: &str) -> Result<Vec<Value>> {
        // Checkpoint writers hold the exclusive side of this gate through commit
        // and cache invalidation, so a reader never combines two checkpoints.
        let _guard = self.context_gate.read().await;
        let session = self
            .get(&format!("session:{sid}"))
            .await?
            .ok_or_else(|| anyhow!("Session not found"))?;
        let through = session["checkpoint_through"].as_i64().unwrap_or(0);
        // A rewind may retain the same compaction cursor. Each process must reject its previous history cache.
        let history_revision = session["history_revision"].as_u64().unwrap_or(0);
        let cached = self.context_cache.lock().unwrap().get(&self.context_cache_key(sid), through, history_revision);
        let (mut input, mut cursor) = cached.unwrap_or_else(|| {
            (
                session["checkpoint_input"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
                through,
            )
        });
        let rows = sqlx::query(
            "SELECT seq,run_id,step,item FROM interactions WHERE session_id=? AND seq>? ORDER BY seq",
        )
        .bind(sid)
        .bind(cursor)
        .fetch_all(&self.pool)
        .await?;
        for row in rows {
            cursor = row.get::<i64, _>("seq");
            if row.get::<String,_>("step")=="user" {
                let rid:String=row.get("run_id");
                if self.get(&format!("message_meta:{rid}")).await?.is_some_and(|meta|meta["context_eligible"]==false || meta["queued"]==true) {continue;}
            }
            input.push(self.unseal(&row.get::<Vec<u8>, _>("item"))?);
        }
        self.context_cache
            .lock()
            .unwrap()
            .insert(self.context_cache_key(sid), through, history_revision, cursor, input.clone());
        Ok(input)
    }
    /// Read public messages and their maximum sequence from one consistent user transaction.
    pub async fn chat_snapshot(&self, sid: &str) -> Result<(Vec<Value>, i64)> {
        let mut tx = self.pool.begin().await?;
        let cursor = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(seq),0) FROM chat WHERE session_id=?",
        )
        .bind(sid)
        .fetch_one(&mut *tx)
        .await?;
        let rows = sqlx::query("SELECT seq,id,run_id,role,content,created_at FROM chat WHERE session_id=? AND seq<=? ORDER BY seq")
            .bind(sid).bind(cursor).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        let messages = rows
            .iter()
            .map(|row| {
                Ok(json!({
                    "seq":row.get::<i64,_>("seq"),"id":row.get::<String,_>("id"),
                    "run_id":row.get::<String,_>("run_id"),"role":row.get::<String,_>("role"),
                    "content":self.unseal(&row.get::<Vec<u8>,_>("content"))?,
                    "created_at":row.get::<i64,_>("created_at")
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok((messages, cursor))
    }
    /// Read bounded identity and public-chat counts without exposing private recovery input.
    pub async fn session_summary(&self, sid: &str) -> Result<Value> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query("SELECT value FROM docs WHERE key=?")
            .bind(format!("session:{sid}"))
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| anyhow!("Session not found"))?;
        let counts = sqlx::query(
            "SELECT COUNT(*) AS total, COALESCE(MAX(seq),0) AS cursor FROM chat WHERE session_id=?",
        )
        .bind(sid)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        let session = self.unseal(&row.get::<Vec<u8>, _>("value"))?;
        Ok(json!({
            "id": sid,
            "title": session["title"],
            "created_at": session["created_at"],
            "chat_count": counts.get::<i64, _>("total"),
            "chat_cursor": counts.get::<i64, _>("cursor"),
        }))
    }
    /// Page a known session public messages with validated bounds and a stable sequence cursor.
    pub async fn chat_page(&self, sid: &str, after_seq: i64, limit: i64) -> Result<Value> {
        if after_seq < 0 || !(1..=50).contains(&limit) {
            return Err(anyhow!("Invalid chat page bounds"));
        }
        if self.get(&format!("session:{sid}")).await?.is_none() {
            return Err(anyhow!("Session not found"));
        }
        let rows = sqlx::query(
            "SELECT seq,id,run_id,role,content,created_at FROM chat WHERE session_id=? AND seq>? ORDER BY seq LIMIT ?",
        )
        .bind(sid).bind(after_seq).bind(limit + 1)
        .fetch_all(&self.pool).await?;
        let has_more = rows.len() > limit as usize;
        let mut messages = Vec::with_capacity(rows.len().min(limit as usize));
        let mut next_seq = after_seq;
        for row in rows.into_iter().take(limit as usize) {
            next_seq = row.get::<i64, _>("seq");
            messages.push(json!({
                "seq": next_seq,
                "id": row.get::<String, _>("id"),
                "run_id": row.get::<String, _>("run_id"),
                "role": row.get::<String, _>("role"),
                "content": self.unseal(&row.get::<Vec<u8>, _>("content"))?,
                "created_at": row.get::<i64, _>("created_at"),
            }));
        }
        Ok(json!({"messages": messages, "next_seq": next_seq, "has_more": has_more}))
    }
    /// Project bounded recent run status for one session without pending arguments or provider state.
    pub async fn run_summaries(&self, sid: &str, limit: i64) -> Result<Vec<Value>> {
        if !(1..=50).contains(&limit) {
            return Err(anyhow!("Invalid run summary limit"));
        }
        if self.get(&format!("session:{sid}")).await?.is_none() {
            return Err(anyhow!("Session not found"));
        }
        let rows = sqlx::query(
            "SELECT r.id,r.status,r.updated_at,d.value FROM run_index r JOIN docs d ON d.key='run_view:'||r.id WHERE r.session_id=? ORDER BY r.updated_at DESC,r.id DESC LIMIT ?",
        )
        .bind(sid).bind(limit).fetch_all(&self.pool).await?;
        rows.iter()
            .map(|row| {
                let view = self.unseal(&row.get::<Vec<u8>, _>("value"))?;
                Ok(json!({
                    "id": row.get::<String, _>("id"),
                    "status": row.get::<String, _>("status"),
                    "error": view["error"],
                    "uncertain": view["uncertain"],
                    "turns": view["turns"],
                    "updated_at": row.get::<i64, _>("updated_at"),
                }))
            })
            .collect()
    }
    /// Read the next bounded public-chat delta after a caller sequence cursor.
    pub async fn chat_events(&self, sid: &str, after: i64) -> Result<Vec<Value>> {
        let rows = sqlx::query("SELECT seq,id,run_id,role,content,created_at FROM chat WHERE session_id=? AND seq>? ORDER BY seq LIMIT 200")
            .bind(sid).bind(after).fetch_all(&self.pool).await?;
        rows.iter()
            .map(|row| {
                Ok(json!({
                    "seq":row.get::<i64,_>("seq"),"id":row.get::<String,_>("id"),
                    "run_id":row.get::<String,_>("run_id"),"role":row.get::<String,_>("role"),
                    "content":self.unseal(&row.get::<Vec<u8>,_>("content"))?,
                    "created_at":row.get::<i64,_>("created_at")
                }))
            })
            .collect()
    }
    /// Insert a private context item once and bind its public activity anchor in the caller transaction.
    pub(crate) async fn insert_interaction(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        sid: &str,
        run_id: &str,
        step: &str,
        item: &Value,
    ) -> Result<()> {
        let inserted = sqlx::query("INSERT OR IGNORE INTO interactions(session_id,run_id,step,item,created_at) VALUES(?,?,?,?,strftime('%s','now'))")
            .bind(sid).bind(run_id).bind(step).bind(self.encode_user_record(item)?)
            .execute(&mut **tx).await?.rows_affected();
        if inserted == 0 { return Ok(()); }
        // ACTIVITY-BOUNDARY: immutable public anchor, atomic with the private item.
        // Never infer same-second promotion ordering from timestamps.
        sqlx::query("INSERT OR IGNORE INTO activity_boundaries(interaction_seq,boundary_item_id) SELECT i.seq,c.id FROM interactions i JOIN chat c ON c.seq=(SELECT MAX(seq) FROM chat WHERE session_id=?) WHERE i.run_id=? AND i.step=?")
            .bind(sid).bind(run_id).bind(step).execute(&mut **tx).await?;
        Ok(())
    }
    /// Publish one user/final assistant bubble and its context boundary, activity and notification atomically.
    async fn insert_chat(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        sid: &str,
        run_id: &str,
        role: &str,
        content: &Value,
    ) -> Result<()> {
        let id = format!("{role}:{run_id}");
        let terminal = if role == "assistant" {
            Some(run_id)
        } else {
            None
        };
        let inserted = sqlx::query("INSERT OR IGNORE INTO chat(id,session_id,run_id,role,content,created_at,terminal_run_id) VALUES(?,?,?,?,?,strftime('%s','now'),?)")
            .bind(&id).bind(sid).bind(run_id).bind(role)
            .bind(self.encode_user_record(content)?).bind(terminal).execute(&mut **tx).await?;
        if inserted.rows_affected() > 0 {
            sqlx::query("INSERT INTO chat_context_boundaries(message_id,through_seq) SELECT ?,COALESCE(MAX(seq),0) FROM interactions WHERE session_id=?")
                .bind(&id).bind(sid).execute(&mut **tx).await?;
            self.touch_session_activity(tx, sid).await?;
            if role == "user" {
                self.stage_notification(tx,&format!("message:{run_id}"),"message.received",sid,Some(run_id),None).await?;
            }
        }
        Ok(())
    }
    /// Publish a bounded nonterminal bubble once; reject reuse of its stable ID with different text.
    async fn insert_working_message(
        &self, tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, sid: &str,
        rid: &str, message_id: &str, text: &str,
    ) -> Result<()> {
        // Working rows have no terminal_run_id. Their stable IDs make a
        // recovered model response or private control safe to replay.
        if !message_id.starts_with(&format!("working:{rid}:"))
            || message_id.len() > 256 || text.trim().is_empty() || text.len() > 16384 {
            return Err(anyhow!("Invalid working message"));
        }
        let existing = sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT content FROM chat WHERE id=?")
            .bind(message_id).fetch_optional(&mut **tx).await?;
        if let Some(raw) = existing {
            if self.unseal(&raw)? != json!(text) {
                return Err(anyhow!("Working message ID conflicts with existing text"));
            }
        } else {
            sqlx::query("INSERT INTO chat(id,session_id,run_id,role,content,created_at,terminal_run_id) VALUES(?,?,?,?,?,strftime('%s','now'),NULL)")
                .bind(message_id).bind(sid).bind(rid).bind("assistant")
                .bind(self.encode_user_record(&json!(text))?).execute(&mut **tx).await?;
            sqlx::query("INSERT INTO chat_context_boundaries(message_id,through_seq) SELECT ?,COALESCE(MAX(seq),0) FROM interactions WHERE session_id=?")
                .bind(message_id).bind(sid).execute(&mut **tx).await?;
            self.touch_session_activity(tx, sid).await?;
        }
        Ok(())
    }
    /// Reconstruct a split recovery record under the reader's transaction.
    /// Legacy records contain input inline and need no migration on read. Missing
    /// rows or gaps are corruption, never an excuse to resume with partial input.
    async fn decode_document(&self, connection: &mut sqlx::SqliteConnection, raw: &[u8]) -> Result<Value> {
        let mut value = self.unseal(raw)?;
        if let Some(reference) = value.get("_recovery_input").cloned() {
            if reference["version"] != 1 { return Err(anyhow!("Unsupported recovery input version")); }
            let rid = value["id"].as_str().ok_or_else(||anyhow!("Recovery record has no run id"))?;
            let count = reference["count"].as_u64().ok_or_else(||anyhow!("Invalid recovery input count"))?;
            let rows = sqlx::query("SELECT ordinal,item FROM recovery_input WHERE run_id=? ORDER BY ordinal")
                .bind(rid).fetch_all(&mut *connection).await?;
            if rows.len() as u64 != count { return Err(anyhow!("Incomplete recovery input")); }
            let manifest = recovery_manifest(&reference, rows.len())?;
            let mut input = Vec::with_capacity(rows.len());
            for (ordinal,row) in rows.iter().enumerate() {
                if row.get::<i64,_>("ordinal") != ordinal as i64 { return Err(anyhow!("Recovery input ordinal gap")); }
                let sealed = row.get::<Vec<u8>,_>("item");
                let item = self.unseal(&sealed)?;
                // RECOVERY-MANIFEST-READ: reconstruction verifies both commitments;
                // save optimization never weakens the authoritative read path.
                if let Some(pairs) = &manifest {
                    if pairs[ordinal].1 != recovery_digest(&sealed) ||
                        pairs[ordinal].0 != recovery_item_digest(&item)? {
                        return Err(anyhow!("Recovery input manifest mismatch"));
                    }
                }
                input.push(item);
            }
            value.as_object_mut().unwrap().remove("_recovery_input");
            value["input"] = Value::Array(input);
        }
        Ok(value)
    }

    /// Persist a document and its recovery-input delta in one transaction.
    /// Unchanged items retain their ciphertext; append, replacement, compaction,
    /// and truncation update only affected rows. A metadata reference is published
    /// only alongside its complete item set. No transcript rows are deleted.
    async fn write_document(&self, tx: &mut sqlx::Transaction<'_,sqlx::Sqlite>, key: &str, value: &Value) -> Result<()> {
        if key.starts_with("run:") && !matches!(value["status"].as_str(), Some("completed" | "cancelled" | "failed")) {
            self.require_active_session(tx, value["session_id"].as_str().ok_or_else(||anyhow!("Run has no session"))?).await?;
            if !matches!(value["status"].as_str(),Some("stopped"|"recovery")) {self.require_participation(tx,value).await?;}
        }
        let session_value;
        let value = if key.starts_with("session:") {
            session_value = self.prepare_session_document(tx, key, value).await?;
            &session_value
        } else { value };
        let mut metadata;
        let stored = if key.starts_with("run:") && value["input"].is_array() {
            let rid = value["id"].as_str().ok_or_else(||anyhow!("Run has no id"))?;
            if key != format!("run:{rid}") { return Err(anyhow!("Recovery run identity mismatch")); }
            let input = value["input"].as_array().unwrap();
            let rows = sqlx::query("SELECT ordinal,item FROM recovery_input WHERE run_id=? ORDER BY ordinal")
                .bind(rid).fetch_all(&mut **tx).await?;
            // RECOVERY-MANIFEST-SAVE: verify every previous ciphertext against
            // sealed metadata before any mutation. Legacy records decrypt once to
            // establish content hashes; manifest-backed saves skip that decryption.
            let mut prior_manifest = None;
            if let Some(raw) = sqlx::query_scalar::<_,Vec<u8>>("SELECT value FROM docs WHERE key=?")
                .bind(key).fetch_optional(&mut **tx).await? {
                let previous = self.unseal(&raw)?;
                if let Some(reference) = previous.get("_recovery_input") {
                    if reference["version"] != 1 || reference["count"].as_u64() != Some(rows.len() as u64) {
                        return Err(anyhow!("Incomplete or unsupported recovery input"));
                    }
                    prior_manifest = recovery_manifest(reference, rows.len())?;
                }
            }
            let mut previous_hashes = Vec::with_capacity(rows.len());
            for (ordinal,row) in rows.iter().enumerate() {
                if row.get::<i64,_>("ordinal") != ordinal as i64 {
                    return Err(anyhow!("Recovery input ordinal gap"));
                }
                let sealed = row.get::<Vec<u8>,_>("item");
                let sealed_hash = recovery_digest(&sealed);
                let plain_hash = if let Some(pairs) = &prior_manifest {
                    if pairs[ordinal].1 != sealed_hash { return Err(anyhow!("Recovery input manifest mismatch")); }
                    pairs[ordinal].0.clone()
                } else {
                    recovery_item_digest(&self.unseal(&sealed)?)?
                };
                previous_hashes.push((plain_hash, sealed_hash));
            }
            let mut manifest = Vec::with_capacity(input.len());
            for (ordinal,item) in input.iter().enumerate() {
                let plain_hash = recovery_item_digest(item)?;
                if let Some(pair) = previous_hashes.get(ordinal).filter(|pair| pair.0 == plain_hash) {
                    manifest.push(pair.clone());
                } else {
                    let sealed = self.encode_user_record(item)?;
                    manifest.push((plain_hash, recovery_digest(&sealed)));
                    sqlx::query("INSERT INTO recovery_input(run_id,ordinal,item) VALUES(?,?,?) ON CONFLICT(run_id,ordinal) DO UPDATE SET item=excluded.item")
                        .bind(rid).bind(ordinal as i64).bind(sealed).execute(&mut **tx).await?;
                }
            }
            sqlx::query("DELETE FROM recovery_input WHERE run_id=? AND ordinal>=?")
                .bind(rid).bind(input.len() as i64).execute(&mut **tx).await?;
            metadata = Value::Object(value.as_object().ok_or_else(||anyhow!("Invalid run document"))?
                .iter().filter(|(key,_)|key.as_str() != "input").map(|(key,value)|(key.clone(),value.clone())).collect());
            metadata["_recovery_input"] = json!({"version":1,"count":input.len(),"manifest":manifest});
            &metadata
        } else { value };
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(key).bind(if key.starts_with("module_state:")||key.starts_with("receipt:module:")||key.starts_with("session_context_assets:"){self.seal(stored)?}else{self.encode_for_pool(self.pool_for(key), stored)?}).execute(&mut **tx).await?;
        Ok(())
    }

    /// Read and authenticate a namespace-selected document, reconstructing split recovery input in one snapshot.
    pub async fn get(&self, key: &str) -> Result<Option<Value>> {
        self.validate_agent_doc_key(key)?;
        // These documents are shared with the independent loop process.
        let cacheable = false;
        let epoch = {
            let mut cache = self.cache.lock().unwrap();
            if cacheable {
                if let Some(value) = cache.get(key) {
                    return Ok(Some(value));
                }
            }
            cache.epoch
        };
        let mut tx = self.pool_for(key).begin().await?;
        let row = sqlx::query("SELECT value FROM docs WHERE key=?")
            .bind(key).fetch_optional(&mut *tx).await?;
        let result = match row {
            Some(row) => Some(self.decode_document(&mut tx, &row.get::<Vec<u8>,_>("value")).await?),
            None => None,
        };
        tx.commit().await?;
        if cacheable {
            if let Some(value) = &result {
                self.cache
                    .lock()
                    .unwrap()
                    .insert(key.into(), value.clone(), epoch);
            }
        }
        Ok(result)
    }
    /// Write a document through its authority and lifecycle guards, then invalidate affected disposable caches.
    pub async fn put(&self, key: &str, value: &Value) -> Result<()> {
        self.validate_agent_doc_key(key)?;
        let session_id = key.strip_prefix("session:");
        let _context_guard = if session_id.is_some() {
            Some(self.context_gate.write().await)
        } else {
            None
        };
        let mut tx = self.pool_for(key).begin().await?;
        if key.starts_with("run:") {
            self.require_active_session(&mut tx,value["session_id"].as_str().ok_or_else(||anyhow!("Run has no session"))?).await?;
        }
        self.write_document(&mut tx,key,value).await?;
        if key.starts_with("run:") {
            self.index_run(&mut tx, value).await?;
        }
        tx.commit().await?;
        self.cache.lock().unwrap().invalidate(&[key.into()]);
        if let Some(sid) = session_id {
            self.context_cache.lock().unwrap().invalidate(&self.context_cache_key(sid));
        }
        Ok(())
    }
    /// Update run lookup, redacted view and usage deltas in the same checkpoint transaction.
    async fn index_run(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        run: &Value,
    ) -> Result<()> {
        self.engine_repository().index_run(tx,run).await
    }
    /// List bounded nonterminal run views in admission order for scheduler recovery.
    pub async fn active_runs(&self) -> Result<Vec<Value>> {
        self.engine_repository().active_runs().await
    }
    /// List bounded recent session runs or application-wide failures using redacted stored views.
    pub async fn recent_runs(&self, sid: Option<&str>) -> Result<Vec<Value>> {
        self.engine_repository().recent_runs(sid).await
    }
    /// Remove only an unclaimed upload owned by this session and decrement its quota atomically.
    pub(crate) async fn delete_pending_upload(&self,sid:&str,aid:&str)->Result<()>{
        let mut tx=self.pool.begin().await?;
        sqlx::query("UPDATE docs SET value=value WHERE key='attachment_quota'").execute(&mut *tx).await?;
        let raw:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(format!("attachment:{aid}")).fetch_optional(&mut *tx).await?;
        let Some(raw)=raw else{return Ok(())};
        let record=self.unseal(&raw)?;
        if record["session_id"]!=sid||!record["run_id"].is_null(){return Err(anyhow!("Attachment removal unavailable"));}
        let quota:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key='attachment_quota'").fetch_optional(&mut *tx).await?;
        let used=quota.map(|raw|self.unseal(&raw)).transpose()?.and_then(|v|v["bytes"].as_u64()).unwrap_or(0);
        sqlx::query("UPDATE docs SET value=? WHERE key='attachment_quota'").bind(self.encode_user_record(&json!({"bytes":used.saturating_sub(record["bytes"].as_u64().unwrap_or(0))}))?).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM docs WHERE key=?").bind(format!("attachment:{aid}")).execute(&mut *tx).await?;
        tx.commit().await?;
        self.cache.lock().unwrap().invalidate(&[format!("attachment:{aid}"),"attachment_quota".into()]);
        Ok(())
    }
    /// Delete a namespace document and any owned recovery rows together before invalidating caches.
    pub async fn delete(&self, key: &str) -> Result<()> {
        self.validate_agent_doc_key(key)?;
        let session_id = key.strip_prefix("session:");
        let _context_guard = if session_id.is_some() {
            Some(self.context_gate.write().await)
        } else {
            None
        };
        // The split input is owned by this recovery record, not the transcript.
        // Delete both together so recreating a run cannot retain orphaned items.
        let mut tx = self.pool_for(key).begin().await?;
        sqlx::query("DELETE FROM docs WHERE key=?").bind(key).execute(&mut *tx).await?;
        if let Some(rid) = key.strip_prefix("run:") {
            sqlx::query("DELETE FROM recovery_input WHERE run_id=?")
                .bind(rid).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        self.cache.lock().unwrap().invalidate(&[key.into()]);
        if let Some(sid) = session_id {
            self.context_cache.lock().unwrap().invalidate(&self.context_cache_key(sid));
        }
        Ok(())
    }
    /// Read matching authority documents in one snapshot and reconstruct any split run inputs.
    pub async fn list(&self, prefix: &str) -> Result<Vec<Value>> {
        self.validate_agent_doc_key(prefix)?;
        let mut tx = self.pool_for(prefix).begin().await?;
        let rows = sqlx::query("SELECT value FROM docs WHERE key LIKE ? ORDER BY key")
            .bind(format!("{prefix}%")).fetch_all(&mut *tx).await?;
        let mut values = Vec::with_capacity(rows.len());
        for row in rows { values.push(self.decode_document(&mut tx,&row.get::<Vec<u8>,_>("value")).await?); }
        tx.commit().await?;
        Ok(values)
    }
    /// Append an encoded event to the application or user plane selected by session identity.
    pub async fn event(&self, sid: &str, kind: &str, payload: &Value) -> Result<()> {
        let destination = if sid.is_empty() {
            &self.app_pool
        } else {
            &self.pool
        };
        sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,?,?)")
            .bind(sid)
            .bind(kind)
            .bind(self.encode_for_pool(destination, payload)?)
            .execute(destination)
            .await?;
        Ok(())
    }
    // Save one checkpoint only if the caller still has the revision and, for
    // loop execution, the live lease it read. The override below is reserved
    // for deliberate recovery or access shutdown and still checks revision.
    pub async fn save_run(&self, run: &mut Value, kind: &str, payload: &Value) -> Result<()> {
        self.engine_repository().save_run(run,kind,payload).await
    }
    /// Allow deliberate terminal shutdown without planner availability while preserving revision fences and failed staging.
    pub async fn save_run_override(&self, run:&mut Value, kind:&str, payload:&Value) -> Result<()> {
        self.engine_repository().save_run_override(run,kind,payload).await
    }
    /// Commit run-owned effects and publication only after revision, generation, lease and participation fences pass.

    // Take temporary ownership of queued work in one transaction. A new
    // generation, lease epoch, and revision fence out a previous loop worker.
    pub async fn claim_run(&self, run: &mut Value, owner: &str, generation: &str) -> Result<bool> {
        self.engine_repository().claim_run(run,owner,generation).await
    }
    /// Reserve the SQLite writer before checking ownership so queue commits cannot stale the renewal snapshot.
    pub async fn renew_run_lease(&self, rid:&str, generation:&str, owner:&str, epoch:i64) -> Result<bool> {
        self.engine_repository().renew_run_lease(rid,generation,owner,epoch).await
    }
    /// Publish the current loop owner liveness using the fixed heartbeat protocol.
    pub async fn write_loop_heartbeat(&self, owner:&str) -> Result<()> {
        self.engine_repository().write_loop_heartbeat(owner).await
    }
    /// Check whether a compatible loop heartbeat is within the caller allowed age.
    pub async fn loop_fresh(&self, seconds:i64) -> Result<bool> {
        self.engine_repository().loop_fresh(seconds).await
    }
    /// Invalidate recorded loop liveness without deleting its protocol or owner identity.
    pub async fn clear_loop_heartbeat(&self) -> Result<()> {
        self.engine_repository().clear_loop_heartbeat().await
    }
    // Validate and mark accepted commands as seen. The queued run remains the
    // authority for execution, so acknowledging a command does not erase work.
    pub async fn acknowledge_loop_commands(&self) -> Result<usize> {
        self.engine_repository().acknowledge_loop_commands().await
    }
    /// Read a bounded authenticated session-event delta after a sequence cursor.
    pub async fn events(&self, sid: &str, after: i64) -> Result<Vec<Value>> {
        let rows=sqlx::query("SELECT seq,kind,payload FROM events WHERE session_id=? AND seq>? ORDER BY seq LIMIT 200")
            .bind(sid).bind(after).fetch_all(&self.pool).await?;
        rows.iter().map(|r|Ok(json!({"seq":r.get::<i64,_>("seq"),"kind":r.get::<String,_>("kind"),"payload":self.unseal(&r.get::<Vec<u8>,_>("payload"))?}))).collect()
    }
    // Save related documents and their event together. For message admission,
    // this includes the queued run and retry receipt: either all exist or none do.
    pub async fn atomic_docs_event(
        &self,
        docs: Vec<(String, Value)>,
        sid: &str,
        kind: &str,
        payload: &Value,
    ) -> Result<()> {
        self.atomic_admission(docs,sid,kind,payload,None).await
    }
    /// Admit only supported first-message events with newly prepared session records in the same transaction.
    pub async fn atomic_first_message(
        &self, docs:Vec<(String,Value)>,sid:&str,kind:&str,payload:&Value,created:&Value
    )->Result<()> {
        if !matches!(kind,"message"|"manager_submission"|"human_message") || sid.is_empty() {
            return Err(anyhow!("Invalid first-message admission"));
        }
        self.atomic_admission(docs,sid,kind,payload,Some(created)).await
    }
    /// Commit related admission documents, public/private messages and loop commands without crossing authority planes.
    async fn atomic_admission(
        &self,mut docs:Vec<(String,Value)>,sid:&str,kind:&str,payload:&Value,created:Option<&Value>
    )->Result<()> {
        if matches!(kind,"message"|"manager_submission") && payload["role"]=="user" {
        if let Some((_,run))=docs.iter().find(|(key,_)|key.starts_with("run:")) {self.reserve_requester_run(run).await?;}
        else if let Some((_,review))=docs.iter().find(|(key,_)|key.starts_with("manager_review:")) {self.reserve_requester_run(&review["run"]).await?;}
        }
        let destination = if sid.is_empty() {
            &self.app_pool
        } else {
            &self.pool
        };
        if docs
            .iter()
            .any(|(key, _)| !std::ptr::eq(self.pool_for(key), destination))
        {
            return Err(anyhow!("Atomic documents cross storage authorities"));
        }
        let mut touched = docs.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>();
        if kind == "message" && payload["role"] == "user" {
            touched.push(format!("session:{sid}"));
        }
        let touches_session = touched.iter().any(|key| key.starts_with("session:"));
        let _context_guard = if touches_session {
            Some(self.context_gate.write().await)
        } else {
            None
        };
        let mut tx = destination.begin().await?;
        if let Some(created)=created {
            let exists:i64=sqlx::query_scalar("SELECT count(*) FROM docs WHERE key=?")
                .bind(format!("session:{sid}")).fetch_one(&mut *tx).await?;
            if exists!=0 {return Err(anyhow!("Session identifier already exists"));}
            // Write only the newly prepared session/mode before message guards.
            // Rollback also removes these records and both notification stages.
            for prefix in ["session","session_mode"] {
                let name=format!("{prefix}:{sid}");
                let position=docs.iter().position(|(key,_)|key==&name)
                    .ok_or_else(||anyhow!("First-message session records missing"))?;
                let (name,value)=docs.remove(position);
                self.write_document(&mut tx,&name,&value).await?;
            }
            self.stage_notification(&mut tx,&format!("created:{sid}"),"session.created",sid,None,None).await?;
            sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,'session_created',?)")
                .bind(sid).bind(self.encode_user_record(created)?).execute(&mut *tx).await?;
        }
        if matches!(kind,"message"|"manager_submission") && payload["role"] == "user" {
            sqlx::query("UPDATE docs SET key=key WHERE key=?").bind(format!("session:{sid}")).execute(&mut *tx).await?;
            self.require_active_session(&mut tx, sid).await?;
            let existing:i64=sqlx::query_scalar("SELECT COUNT(*) FROM run_index WHERE session_id=? AND id!=? AND status NOT IN ('completed','cancelled','failed')")
                .bind(sid).bind(payload["run_id"].as_str().unwrap_or("")).fetch_one(&mut *tx).await?;
            if existing>0 {return Err(anyhow!("Session already has an active run"));}
            let pending:i64=sqlx::query_scalar("SELECT COUNT(*) FROM docs WHERE key=?")
                .bind(format!("manager_pending:{sid}")).fetch_one(&mut *tx).await?;
            if pending!=0 {return Err(anyhow!("Resolve the pending message review first"));}
        }
        for (_, run) in docs.iter().filter(|(key,_)|key.starts_with("run:")) {
            if !matches!(run["status"].as_str(), Some("completed" | "cancelled" | "failed")) {
                self.require_active_session(&mut tx, run["session_id"].as_str().ok_or_else(||anyhow!("Run has no session"))?).await?;
            }
        }
        if let Some((_, run))=docs.iter_mut().find(|(key,_)|key.starts_with("run:")) {
            let rid=run["id"].as_str().ok_or_else(||anyhow!("Run has no id"))?.to_owned();
            let expected=run["run_revision"].as_u64().unwrap_or(0);
            if let Some(raw)=sqlx::query_scalar::<_,Vec<u8>>("SELECT value FROM docs WHERE key=?")
                .bind(format!("run:{rid}")).fetch_optional(&mut *tx).await? {
                let previous=self.decode_document(&mut tx,&raw).await?;
                if previous["history_removed"] == true { return Err(anyhow!("Run removed by Undo")); }
                if previous["generation"]!=run["generation"]
                    || previous["lease_epoch"]!=run["lease_epoch"]
                    || previous["run_revision"].as_u64().unwrap_or(0)!=expected
                    || (matches!(previous["status"].as_str(),Some("completed"|"cancelled"|"failed"))
                        && previous!=*run)
                {
                    return Err(anyhow!("Run fence rejected stale command"));
                }
            } else if expected!=0 {
                return Err(anyhow!("Run revision missing"));
            }
            run["run_revision"]=json!(expected+1);
        }
        let command_id=docs.iter().find(|(key,_)|key.starts_with("receipt:"))
            .map(|(key,_)|key.clone());
        for (key, value) in docs {
            self.write_document(&mut tx,&key,&value).await?;
            if key.starts_with("run:") {
                self.index_run(&mut tx, &value).await?;
                self.stage_run_notification(&mut tx,&value).await?;
            }
        }
        if matches!(kind,"message"|"manager_submission"|"human_message") && payload["role"] == "user" {
            let run_id = payload["run_id"]
                .as_str()
                .ok_or_else(|| anyhow!("User message has no run ID"))?;
            let text = payload["content"]
                .as_str()
                .ok_or_else(|| anyhow!("User message has no content"))?;
            if payload["module_silent"]!=true {self.insert_chat(&mut tx, sid, run_id, "user", &json!(text))
                .await?;}
            self.write_document(&mut tx,&format!("message_meta:{run_id}"),
                &json!({"author_id":payload["author_id"].as_str().unwrap_or(self.current_user_id()),"context_eligible":kind=="message","queued":false})).await?;
            if kind=="message" {
                self.begin_session_naming(&mut tx, sid, run_id, payload).await?;
            }
            // Chat shows the author's message; model context also includes
            // the validated attachment text, committed in this same transaction.
            let context_content = payload["context_content"].as_str().unwrap_or(text);
            if kind=="message" {
                self.insert_interaction(&mut tx,sid,run_id,"user",
                    &json!({"role":"user","content":context_content})).await?;
            }
        }
        if kind == "session_created" {
            self.stage_notification(&mut tx,&format!("created:{sid}"),"session.created",sid,None,None).await?;
        }
        if kind != "message" {
            sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,?,?)")
                .bind(sid)
                .bind(kind)
                .bind(self.encode_for_pool(destination, payload)?)
                .execute(&mut *tx)
                .await?;
        }
        if !sid.is_empty() && (kind=="message" || kind=="run_status") {
            let run_id=payload["run_id"].as_str().or_else(||payload["id"].as_str())
                .ok_or_else(||anyhow!("Run command has no id"))?;
            let command_id=command_id.unwrap_or_else(||format!("{kind}:{run_id}"));
            sqlx::query("INSERT INTO loop_commands(command_id,run_id,schema_version,kind,payload,created_at) VALUES(?,?,1,?,?,strftime('%s','now'))")
                .bind(command_id).bind(run_id).bind(kind)
                .bind(self.encode_user_record(&json!({"schema_version":1,"run_id":run_id,
                    "kind":kind,"status":payload["status"]}))?)
                .execute(&mut *tx).await?;
            sqlx::query("INSERT INTO loop_events(run_id,schema_version,kind,payload,created_at) VALUES(?,1,?,?,strftime('%s','now'))")
                .bind(run_id).bind(kind).bind(self.encode_user_record(&json!({"schema_version":1,
                    "run_id":run_id,"kind":kind}))?).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        self.cache.lock().unwrap().invalidate(&touched);
        if touches_session {
            let mut context_cache = self.context_cache.lock().unwrap();
            for key in &touched {
                if let Some(sid) = key.strip_prefix("session:") {
                    context_cache.invalidate(&self.context_cache_key(sid));
                }
            }
        }
        Ok(())
    }
}
#[path = "../../crates/modules/manager/host/repository.rs"]
mod message_admission;
#[cfg(test)]
mod tests {
    /// Verify only the fixed administrator profiles retain worker-readable directory and database modes after reopen.
    #[tokio::test]
    async fn only_fixed_admin_profiles_keep_worker_permissions_on_reopen() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        for (relative, directory_mode, file_mode) in [
            ("user_data/admin/admin.db", 0o770, 0o660),
            ("agent_data/shared_default/shared_default.db", 0o770, 0o660),
            ("user_data/other/other.db", 0o700, 0o600),
            ("agent_data/other/other.db", 0o700, 0o600),
            ("db/app.db", 0o770, 0o660),
        ] {
            let path = root.path().join(relative);
            for _ in 0..2 {
                let pool = super::open_pool(&path, false, false).await.unwrap();
                assert_eq!(std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, directory_mode);
                assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, file_mode);
                pool.close().await;
            }
        }
    }

    /// Verify legacy model, disabled ability and encrypted prompt values survive the canonical-table migration.
    #[tokio::test]
    async fn legacy_agent_docs_and_prompt_slots_migrate_into_canonical_tables() {
        let root = tempfile::tempdir().unwrap();
        let store = super::Store::open(root.path()).await.unwrap();
        store.put("agent:shared_default:model",
            &serde_json::json!({"provider":"openai_subscription","model":"gpt-6-sol"}))
            .await.unwrap();
        store.put("agent_abilities:shared_default",
            &serde_json::json!({"revision":3,"visibility":{"codebase_admin":"off"},
                "skill_visibility":{"codebase_admin":"discoverable"}}))
            .await.unwrap();
        sqlx::query("INSERT INTO prompt_slots(slot_name,order_index,locked,merge_mode,content,revision,updated_at) VALUES(?,?,?,?,?,?,?)")
            .bind("legacy").bind(10_i64).bind(1_i64).bind("replace")
            .bind(store.seal(&serde_json::json!("saved prompt")).unwrap())
            .bind(2_i64).bind(100_i64).execute(&store.agent_pool).await.unwrap();
        sqlx::query("DELETE FROM agents WHERE id='shared_default'")
            .execute(&store.agent_pool).await.unwrap();
        store.app_pool.close().await;
        store.pool.close().await;
        store.agent_pool.close().await;
        let migrated = super::Store::open(root.path()).await.unwrap();
        assert_eq!(migrated.agent_model().await.unwrap()["model"], "gpt-6-sol");
        assert_eq!(migrated.agent_abilities().await.unwrap()["visibility"]["codebase_admin"], "off");
        assert_eq!(migrated.agent_abilities().await.unwrap()["skill_visibility"]["codebase_admin"], "discoverable");
        assert_eq!(migrated.get_prompt_slot("legacy").await.unwrap().unwrap()["content"], "saved prompt");
        let enabled: i64 = sqlx::query_scalar("SELECT enabled FROM agent_connections WHERE id='ability:codebase_admin'")
            .fetch_one(&migrated.agent_pool).await.unwrap();
        assert_eq!(enabled, 0);
    }
    use super::*;
    /// Verify agent/session controls stay in separate encrypted authorities and survive reopening.
    #[tokio::test]
    async fn ability_controls_use_encrypted_authorities_and_survive_reopen() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        let agent = json!({"revision":1,"visibility":{"codebase_admin":"visible"}});
        let session = json!({"revision":1,"active":[],"suppressed":["codebase_admin"]});
        store
            .put("agent_abilities:shared_default", &agent)
            .await
            .unwrap();
        store
            .put("session_ability_state:s1", &session)
            .await
            .unwrap();
        let agent_blob: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind("agent_abilities:shared_default")
            .fetch_one(&store.agent_pool)
            .await
            .unwrap();
        let session_blob: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind("session_ability_state:s1")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert!(!agent_blob
            .windows(b"codebase_admin".len())
            .any(|part| part == b"codebase_admin"));
        assert!(!session_blob
            .windows(b"codebase_admin".len())
            .any(|part| part == b"codebase_admin"));
        let wrong_agent: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind("agent_abilities:shared_default")
            .fetch_optional(&store.pool)
            .await
            .unwrap();
        let wrong_session: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
                .bind("session_ability_state:s1")
                .fetch_optional(&store.agent_pool)
                .await
                .unwrap();
        assert!(wrong_agent.is_none() && wrong_session.is_none());
        store.app_pool.close().await;
        store.pool.close().await;
        store.agent_pool.close().await;
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(
            reopened
                .get("agent_abilities:shared_default")
                .await
                .unwrap(),
            Some(agent)
        );
        assert_eq!(
            reopened.get("session_ability_state:s1").await.unwrap(),
            Some(session)
        );
    }

    /// Verify session mode routes to encrypted user storage without contaminating application records.
    #[tokio::test]
    async fn session_mode_is_encrypted_in_user_storage_and_survives_reopen() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .put("session:s1", &json!({"id":"s1","title":"Test session"}))
            .await
            .unwrap();
        let mode = json!({"mode":"coding","revision":1});
        store.put("session_mode:s1", &mode).await.unwrap();
        assert_eq!(store.list("session:").await.unwrap().len(), 1);
        assert_eq!(
            store.list("session_mode:").await.unwrap(),
            vec![mode.clone()]
        );
        let sealed: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind("session_mode:s1")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert!(!sealed
            .windows(b"coding".len())
            .any(|part| part == b"coding"));
        let app_copy: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind("session_mode:s1")
            .fetch_optional(&store.app_pool)
            .await
            .unwrap();
        assert!(app_copy.is_none());
        store.app_pool.close().await;
        store.pool.close().await;
        store.agent_pool.close().await;

        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(reopened.get("session_mode:s1").await.unwrap(), Some(mode));
        assert_eq!(reopened.list("session:").await.unwrap().len(), 1);
    }

    /// Verify prompt ordering, inherited metadata, CAS rejection and encrypted durability through slot updates and deletion.
    #[tokio::test]
    async fn prompt_slots_are_ordered_revisioned_encrypted_and_durable() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        let first = store
            .upsert_prompt_slot("core", "private prompt", None, None, None, 0)
            .await
            .unwrap();
        assert_eq!(first["revision"], 1);
        assert_eq!(first["order_index"], 10);
        assert_eq!(first["lock"], false);
        assert_eq!(first["merge_mode"], "replace");
        let second = store
            .upsert_prompt_slot("style", "formal", Some(5), Some(true), Some("append"), 0)
            .await
            .unwrap();
        assert_eq!(second["order_index"], 5);
        assert_eq!(
            store
                .list_prompt_slots()
                .await
                .unwrap()
                .iter()
                .map(|slot| slot["slot_name"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>(),
            vec!["style", "core"]
        );
        let updated = store
            .upsert_prompt_slot("style", "brief", None, None, None, 1)
            .await
            .unwrap();
        assert_eq!(updated["revision"], 2);
        assert_eq!(updated["order_index"], 5);
        assert_eq!(updated["lock"], true);
        assert_eq!(updated["merge_mode"], "append");
        assert_eq!(
            store.get_prompt_slot("style").await.unwrap().unwrap(),
            updated
        );
        assert_eq!(
            store
                .upsert_prompt_slot("style", "stale", None, None, None, 1)
                .await
                .unwrap_err()
                .to_string(),
            "Prompt slot revision mismatch"
        );
        assert_eq!(
            store
                .delete_prompt_slot("style", 1)
                .await
                .unwrap_err()
                .to_string(),
            "Prompt slot revision mismatch"
        );
        assert_eq!(
            store
                .delete_prompt_slot("absent", 1)
                .await
                .unwrap_err()
                .to_string(),
            "Prompt slot not found"
        );
        let encrypted: Vec<u8> =
            sqlx::query_scalar("SELECT content FROM agent_prompts WHERE slot_name='core'")
                .fetch_one(&store.agent_pool)
                .await
                .unwrap();
        assert!(!encrypted
            .windows(b"private prompt".len())
            .any(|window| window == b"private prompt"));
        store.app_pool.close().await;
        store.pool.close().await;
        store.agent_pool.close().await;
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(
            reopened.get_prompt_slot("core").await.unwrap().unwrap()["content"],
            "private prompt"
        );
        assert_eq!(
            reopened.get_prompt_slot("style").await.unwrap().unwrap()["revision"],
            2
        );
        assert!(reopened.delete_prompt_slot("style", 2).await.unwrap());
        assert!(reopened.get_prompt_slot("style").await.unwrap().is_none());
    }

    /// Exercise prompt-slot identity, metadata and aggregate capacity rejection through the real Store.
    #[tokio::test]
    async fn prompt_slots_enforce_name_size_count_total_and_metadata_bounds() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        for name in ["", " bad", "bad\nname", "__skills__"] {
            assert!(store
                .upsert_prompt_slot(name, "", None, None, None, 0)
                .await
                .is_err());
        }
        assert!(store
            .upsert_prompt_slot(&"x".repeat(129), "", None, None, None, 0)
            .await
            .is_err());
        assert!(store
            .upsert_prompt_slot(
                "bad-content",
                &"x".repeat(16 * 1024 + 1),
                None,
                None,
                None,
                0
            )
            .await
            .is_err());
        assert!(store
            .upsert_prompt_slot("bad-mode", "", None, None, Some("prepend"), 0)
            .await
            .is_err());
        assert!(store
            .upsert_prompt_slot("bad-order", "", Some(-1), None, None, 0)
            .await
            .is_err());
        assert!(store
            .upsert_prompt_slot("bad-revision", "", None, None, None, -1)
            .await
            .is_err());
        for i in 0..4 {
            store
                .upsert_prompt_slot(
                    &format!("large-{i}"),
                    &"x".repeat(16 * 1024),
                    None,
                    None,
                    None,
                    0,
                )
                .await
                .unwrap();
        }
        assert!(store
            .upsert_prompt_slot("over-total", "x", None, None, None, 0)
            .await
            .unwrap_err()
            .to_string()
            .contains("total exceeds limit"));
        for i in 4..32 {
            store
                .upsert_prompt_slot(&format!("slot-{i}"), "", None, None, None, 0)
                .await
                .unwrap();
        }
        assert_eq!(store.list_prompt_slots().await.unwrap().len(), 32);
        assert!(store
            .upsert_prompt_slot("over-count", "", None, None, None, 0)
            .await
            .unwrap_err()
            .to_string()
            .contains("count exceeds limit"));
        assert_eq!(store.list_prompt_slots().await.unwrap().len(), 32);
    }

    /// Verify cached context appends only interactions beyond its retained sequence cursor.
    #[tokio::test]
    async fn context_cache_appends_only_new_interactions() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .put(
                "session:s",
                &json!({"id":"s","checkpoint_input":[],"checkpoint_through":0}),
            )
            .await
            .unwrap();
        store
            .atomic_docs_event(
                vec![],
                "s",
                "message",
                &json!({"role":"user","content":"first","run_id":"r1"}),
            )
            .await
            .unwrap();
        assert_eq!(store.context("s").await.unwrap()[0]["content"], "first");
        let first_cursor = store.context_cache.lock().unwrap().get(&store.context_cache_key("s"), 0, 0).unwrap().1;
        store
            .atomic_docs_event(
                vec![],
                "s",
                "message",
                &json!({"role":"user","content":"second","run_id":"r2"}),
            )
            .await
            .unwrap();
        let input = store.context("s").await.unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["content"], "first");
        assert_eq!(input[1]["content"], "second");
        let second_cursor = store.context_cache.lock().unwrap().get(&store.context_cache_key("s"), 0, 0).unwrap().1;
        assert!(second_cursor > first_cursor);
    }

    /// Verify compaction replaces a cached checkpoint even at the same cursor and remains canonical after reopen.
    #[tokio::test]
    async fn context_cache_invalidates_same_cursor_checkpoint_and_rebuilds_after_reopen() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .put(
                "session:s",
                &json!({"id":"s","checkpoint_input":[{"role":"user","content":"old"}],"checkpoint_through":0}),
            )
            .await
            .unwrap();
        assert_eq!(store.context("s").await.unwrap()[0]["content"], "old");
        let summary = json!({"role":"user","content":"summary"});
        let source=json!([{"role":"user","content":"old"}]);
        let mut checkpoint=json!({"id":"r1","session_id":"s","status":"running",
            "input":[summary.clone()],"compactions":1,"updated_at":1});
        store
            .save_run(
                &mut checkpoint,
                "context_compacted",
                &json!({"run_id":"r1","segment_id":"r1-1","source_digest":crate::digest(&source),
                    "source":source,"dropped_items":1}),
            )
            .await
            .unwrap();
        assert_eq!(store.context("s").await.unwrap(), vec![summary.clone()]);
        checkpoint["status"] = json!("completed");
        store.save_run(&mut checkpoint, "run_status", &json!({"run_id":"r1","status":"completed"}))
            .await.unwrap();
        assert_eq!(store.context("s").await.unwrap(), vec![summary.clone()]);
        store
            .atomic_docs_event(
                vec![],
                "s",
                "message",
                &json!({"role":"user","content":"new","run_id":"r2"}),
            )
            .await
            .unwrap();
        assert_eq!(store.context("s").await.unwrap().len(), 2);
        store.pool.close().await;
        store.app_pool.close().await;
        store.agent_pool.close().await;
        let reopened = Store::open(root.path()).await.unwrap();
        let rebuilt = reopened.context("s").await.unwrap();
        assert_eq!(rebuilt.len(), 2);
        assert_eq!(rebuilt[0], summary);
        assert_eq!(rebuilt[1]["content"], "new");
    }

    /// Race context readers with compaction and verify old readers cannot repopulate a discarded checkpoint.
    #[tokio::test]
    async fn concurrent_context_readers_cannot_restore_precheckpoint_cache() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .put(
                "session:s",
                &json!({"id":"s","checkpoint_input":[{"role":"user","content":"old"}],"checkpoint_through":0}),
            )
            .await
            .unwrap();
        store.context("s").await.unwrap();
        let mut readers = Vec::new();
        for _ in 0..16 {
            let clone = store.clone();
            readers.push(tokio::spawn(
                async move { clone.context("s").await.unwrap() },
            ));
        }
        let mut checkpoint=json!({"id":"r1","session_id":"s","status":"running",
            "input":[],"compactions":1,"updated_at":1});
        checkpoint["input"]=json!([{"role":"user","content":"new checkpoint"}]);
        let source=json!([{"role":"user","content":"old"}]);
        store
            .save_run(
                &mut checkpoint,
                "context_compacted",
                &json!({"run_id":"r1","segment_id":"r1-1","source_digest":crate::digest(&source),
                    "source":source,"dropped_items":1}),
            )
            .await
            .unwrap();
        for reader in readers {
            reader.await.unwrap();
        }
        for _ in 0..4 {
            assert_eq!(
                store.context("s").await.unwrap()[0]["content"],
                "new checkpoint"
            );
        }
    }

    /// Verify version-one migration preserves newer authorities while importing legacy history and stable event mappings.
    #[tokio::test]
    async fn split_storage_preserves_legacy_history_and_retries_version_one_marker() {
        let root = tempfile::tempdir().unwrap();
        let first = Store::open(root.path()).await.unwrap();
        first
            .put("settings", &json!({"model":"new"}))
            .await
            .unwrap();
        first
            .put("storage:split_migrated", &json!({"version":1}))
            .await
            .unwrap();
        first.put("policy", &json!({"version":4})).await.unwrap();
        first
            .put("provider_tokens", &json!({"access_token":"new"}))
            .await
            .unwrap();
        first
            .put("agent:shared_default:model", &json!({"model":"new-agent"}))
            .await
            .unwrap();
        for ordinal in 1..=4 {
            first
                .event("old", "split_event", &json!({"ordinal":ordinal}))
                .await
                .unwrap();
        }
        sqlx::query(
            "INSERT INTO events(seq,session_id,kind,payload) VALUES(9,'','split_policy',?)",
        )
        .bind(first.seal(&json!({"version":4})).unwrap())
        .execute(&first.app_pool)
        .await
        .unwrap();
        let legacy_path = root.path().join("runtime.sqlite");
        let legacy = SqlitePool::connect(&format!("sqlite://{}?mode=rwc", legacy_path.display()))
            .await
            .unwrap();
        sqlx::raw_sql("CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB NOT NULL);
            CREATE TABLE events(seq INTEGER PRIMARY KEY AUTOINCREMENT,session_id TEXT NOT NULL,kind TEXT NOT NULL,payload BLOB NOT NULL);
            CREATE TABLE run_index(id TEXT PRIMARY KEY,session_id TEXT NOT NULL,status TEXT NOT NULL,updated_at INTEGER NOT NULL);")
            .execute(&legacy).await.unwrap();
        for (key, value) in [
            ("", json!({"empty":"key"})),
            ("admin", json!({"username":"admin"})),
            ("provider_tokens", json!({"access_token":"old"})),
            ("settings", json!({"model":"chosen"})),
            ("policy", json!({"version":3})),
            ("control", json!({"shutoff":false})),
            (
                "agent:shared_default:model",
                json!({"model":"chosen-agent"}),
            ),
            ("receipt:message:old:req", json!({"result":{"run_id":"r1"}})),
            (
                "session:old",
                json!({
                    "id":"old","created_at":10,
                    "messages":[
                        {"role":"user","content":"hello","run_id":"r1"},
                        {"role":"assistant","content":"world","run_id":"r1"}
                    ],
                    "context":[{"role":"user","content":"hello"},{"role":"assistant","content":"world"}]
                }),
            ),
            (
                "run:r1",
                json!({"id":"r1","session_id":"old","status":"completed","updated_at":11,"input":[]}),
            ),
            (
                "run_view:r1",
                json!({"id":"r1","session_id":"old","status":"completed","updated_at":11}),
            ),
        ] {
            sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
                .bind(key)
                .bind(first.seal(&value).unwrap())
                .execute(&legacy)
                .await
                .unwrap();
        }
        for ordinal in 0..130 {
            sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
                .bind(format!("extra:{ordinal:03}"))
                .bind(first.seal(&json!({"ordinal":ordinal})).unwrap())
                .execute(&legacy)
                .await
                .unwrap();
        }
        for (seq, sid, kind, payload) in [
            (
                4,
                "old",
                "message",
                json!({"role":"user","content":"hello","run_id":"r1"}),
            ),
            (
                7,
                "old",
                "run_status",
                json!({"run_id":"r1","status":"completed"}),
            ),
            (9, "", "policy_changed", json!({"version":3})),
        ] {
            sqlx::query("INSERT INTO events(seq,session_id,kind,payload) VALUES(?,?,?,?)")
                .bind(seq)
                .bind(sid)
                .bind(kind)
                .bind(first.seal(&payload).unwrap())
                .execute(&legacy)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO run_index(id,session_id,status,updated_at) VALUES('r1','old','completed',11)")
            .execute(&legacy).await.unwrap();
        legacy.close().await;
        first.app_pool.close().await;
        first.pool.close().await;
        first.agent_pool.close().await;

        let migrated = Store::open(root.path()).await.unwrap();
        assert_eq!(migrated.get("").await.unwrap().unwrap()["empty"], "key");
        assert_eq!(
            migrated.get("extra:129").await.unwrap().unwrap()["ordinal"],
            129
        );
        assert_eq!(
            migrated.get("settings").await.unwrap().unwrap()["model"],
            "new"
        );
        assert_eq!(
            migrated
                .get("agent:shared_default:model")
                .await
                .unwrap()
                .unwrap()["model"],
            "new-agent"
        );
        assert_eq!(
            migrated
                .get("receipt:message:old:req")
                .await
                .unwrap()
                .unwrap()["result"]["run_id"],
            "r1"
        );
        assert_eq!(
            migrated.get("run:r1").await.unwrap().unwrap()["status"],
            "completed"
        );
        assert_eq!(migrated.context("old").await.unwrap().len(), 2);
        let (messages, cursor) = migrated.chat_snapshot("old").await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["content"], "hello");
        assert_eq!(messages[1]["content"], "world");
        assert_eq!(migrated.recent_runs(Some("old")).await.unwrap().len(), 1);
        let events = migrated.events("old", 0).await.unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event["seq"].as_i64().unwrap())
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 6]
        );
        assert_eq!(migrated.events("old", 4).await.unwrap().len(), 2);
        assert_eq!(events[4]["payload"]["content"], "hello");
        let mapped: i64 =
            sqlx::query_scalar("SELECT new_seq FROM legacy_event_map WHERE legacy_seq=4")
                .fetch_one(&migrated.pool)
                .await
                .unwrap();
        assert_eq!(mapped, 5);
        let app_event: Vec<u8> = sqlx::query_scalar("SELECT payload FROM events WHERE seq=10")
            .fetch_one(&migrated.app_pool)
            .await
            .unwrap();
        assert_eq!(migrated.unseal(&app_event).unwrap()["version"], 3);
        assert_eq!(migrated.get("policy").await.unwrap().unwrap()["version"], 4);
        assert_eq!(
            migrated.get("provider_tokens").await.unwrap().unwrap()["access_token"],
            "new"
        );
        assert_eq!(
            migrated
                .get("storage:split_migrated")
                .await
                .unwrap()
                .unwrap()["version"],
            2
        );
        migrated.app_pool.close().await;
        migrated.pool.close().await;
        migrated.agent_pool.close().await;
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(reopened.chat_snapshot("old").await.unwrap().1, cursor);
        assert_eq!(reopened.events("old", 0).await.unwrap().len(), 6);
        assert!(legacy_path.exists());
    }
    /// Verify a conflicting initial migration leaves no false completion marker.
    #[tokio::test]
    async fn split_storage_rejects_conflicting_destination_without_completion_marker() {
        let root = tempfile::tempdir().unwrap();
        let first = Store::open(root.path()).await.unwrap();
        first
            .put("settings", &json!({"model":"new"}))
            .await
            .unwrap();
        let legacy_path = root.path().join("runtime.sqlite");
        let legacy = SqlitePool::connect(&format!("sqlite://{}?mode=rwc", legacy_path.display()))
            .await
            .unwrap();
        sqlx::raw_sql("CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB NOT NULL);
            CREATE TABLE events(seq INTEGER PRIMARY KEY AUTOINCREMENT,session_id TEXT NOT NULL,kind TEXT NOT NULL,payload BLOB NOT NULL);
            CREATE TABLE run_index(id TEXT PRIMARY KEY,session_id TEXT NOT NULL,status TEXT NOT NULL,updated_at INTEGER NOT NULL);")
            .execute(&legacy).await.unwrap();
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind("settings")
            .bind(first.seal(&json!({"model":"old"})).unwrap())
            .execute(&legacy)
            .await
            .unwrap();
        legacy.close().await;
        first.app_pool.close().await;
        first.pool.close().await;
        first.agent_pool.close().await;
        let err = Store::open(root.path()).await.err().unwrap();
        assert!(err.to_string().contains("Conflicting migrated document"));
        let app = open_pool(&root.path().join("db/app.db"), true, false)
            .await
            .unwrap();
        assert!(sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT value FROM docs WHERE key='storage:split_migrated'"
        )
        .fetch_optional(&app)
        .await
        .unwrap()
        .is_none());
    }
    /// Verify conflicting run identity/status blocks migration without overwriting destination state.
    #[tokio::test]
    async fn version_one_migration_rejects_conflicting_run_index() {
        let root = tempfile::tempdir().unwrap();
        let first = Store::open(root.path()).await.unwrap();
        first
            .put("storage:split_migrated", &json!({"version":1}))
            .await
            .unwrap();
        sqlx::query("INSERT INTO run_index(id,session_id,status,updated_at) VALUES('r1','new','running',20)")
            .execute(&first.pool).await.unwrap();
        let path = root.path().join("runtime.sqlite");
        let legacy = SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))
            .await
            .unwrap();
        sqlx::raw_sql("CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB NOT NULL);
            CREATE TABLE events(seq INTEGER PRIMARY KEY AUTOINCREMENT,session_id TEXT NOT NULL,kind TEXT NOT NULL,payload BLOB NOT NULL);
            CREATE TABLE run_index(id TEXT PRIMARY KEY,session_id TEXT NOT NULL,status TEXT NOT NULL,updated_at INTEGER NOT NULL);")
            .execute(&legacy).await.unwrap();
        sqlx::query("INSERT INTO run_index(id,session_id,status,updated_at) VALUES('r1','old','completed',10)")
            .execute(&legacy).await.unwrap();
        legacy.close().await;
        first.app_pool.close().await;
        first.pool.close().await;
        first.agent_pool.close().await;
        let err = Store::open(root.path()).await.err().unwrap();
        assert!(err
            .to_string()
            .contains("Conflicting migrated run index: r1"));
        let user = open_pool(&root.path().join("user_data/admin/admin.db"), true, true)
            .await
            .unwrap();
        let status: String = sqlx::query_scalar("SELECT status FROM run_index WHERE id='r1'")
            .fetch_one(&user)
            .await
            .unwrap();
        assert_eq!(status, "running");
        let app = open_pool(&root.path().join("db/app.db"), true, false)
            .await
            .unwrap();
        let marker: Vec<u8> =
            sqlx::query_scalar("SELECT value FROM docs WHERE key='storage:split_migrated'")
                .fetch_one(&app)
                .await
                .unwrap();
        assert_eq!(first.unseal(&marker).unwrap()["version"], 1);
    }
    /// Verify resumed migration allocates new sequences when an older event already owns the tempting destination sequence.
    #[tokio::test]
    async fn version_one_retry_does_not_reuse_an_already_mapped_event_sequence() {
        let root = tempfile::tempdir().unwrap();
        let first = Store::open(root.path()).await.unwrap();
        first
            .put("storage:split_migrated", &json!({"version":1}))
            .await
            .unwrap();
        for ordinal in 1..=4 {
            first
                .event("old", "split", &json!({"ordinal":ordinal}))
                .await
                .unwrap();
        }
        let user_payload = json!({"kind":"legacy-user"});
        let app_payload = json!({"kind":"legacy-app"});
        sqlx::query("INSERT INTO events(seq,session_id,kind,payload) VALUES(5,'old','legacy',?)")
            .bind(first.seal(&user_payload).unwrap())
            .execute(&first.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO legacy_event_map(legacy_seq,new_seq) VALUES(4,5)")
            .execute(&first.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO events(seq,session_id,kind,payload) VALUES(9,'','split',?)")
            .bind(first.seal(&json!({"kind":"split-app"})).unwrap())
            .execute(&first.app_pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO events(seq,session_id,kind,payload) VALUES(10,'','legacy',?)")
            .bind(first.seal(&app_payload).unwrap())
            .execute(&first.app_pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO legacy_event_map(legacy_seq,new_seq) VALUES(9,10)")
            .execute(&first.app_pool)
            .await
            .unwrap();
        let path = root.path().join("runtime.sqlite");
        let legacy = SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))
            .await
            .unwrap();
        sqlx::raw_sql("CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB NOT NULL);
            CREATE TABLE events(seq INTEGER PRIMARY KEY AUTOINCREMENT,session_id TEXT NOT NULL,kind TEXT NOT NULL,payload BLOB NOT NULL);
            CREATE TABLE run_index(id TEXT PRIMARY KEY,session_id TEXT NOT NULL,status TEXT NOT NULL,updated_at INTEGER NOT NULL);")
            .execute(&legacy).await.unwrap();
        for (seq, sid, payload) in [
            (4, "old", &user_payload),
            (5, "old", &user_payload),
            (9, "", &app_payload),
            (10, "", &app_payload),
        ] {
            sqlx::query("INSERT INTO events(seq,session_id,kind,payload) VALUES(?,?,?,?)")
                .bind(seq)
                .bind(sid)
                .bind("legacy")
                .bind(first.seal(payload).unwrap())
                .execute(&legacy)
                .await
                .unwrap();
        }
        legacy.close().await;
        first.app_pool.close().await;
        first.pool.close().await;
        first.agent_pool.close().await;
        let migrated = Store::open(root.path()).await.unwrap();
        let user_map: i64 =
            sqlx::query_scalar("SELECT new_seq FROM legacy_event_map WHERE legacy_seq=5")
                .fetch_one(&migrated.pool)
                .await
                .unwrap();
        let app_map: i64 =
            sqlx::query_scalar("SELECT new_seq FROM legacy_event_map WHERE legacy_seq=10")
                .fetch_one(&migrated.app_pool)
                .await
                .unwrap();
        assert_eq!((user_map, app_map), (6, 11));
        assert_eq!(migrated.events("old", 0).await.unwrap().len(), 6);
        migrated.app_pool.close().await;
        migrated.pool.close().await;
        migrated.agent_pool.close().await;
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(reopened.events("old", 0).await.unwrap().len(), 6);
        let app_count: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
            .fetch_one(&reopened.app_pool)
            .await
            .unwrap();
        assert_eq!(app_count, 3);
    }
    /// Verify bounded state inspection preserves session isolation and omits pending/provider secrets.
    #[tokio::test]
    async fn typed_state_inspection_is_session_scoped_bounded_and_redacted() {
        let root = tempfile::tempdir().unwrap();
        let s = Store::open(root.path()).await.unwrap();
        s.put(
            "session:a",
            &json!({"id":"a","title":"Alpha","created_at":12}),
        )
        .await
        .unwrap();
        s.put(
            "session:b",
            &json!({"id":"b","title":"Beta","created_at":13}),
        )
        .await
        .unwrap();
        for (seq, sid) in [(2, "a"), (4, "a"), (5, "b"), (6, "a")] {
            sqlx::query("INSERT INTO chat(seq,id,session_id,run_id,role,content,created_at) VALUES(?,?,?,?,?,?,?)")
                .bind(seq).bind(format!("message:{seq}")).bind(sid)
                .bind(format!("run:{seq}")).bind("user")
                .bind(s.seal(&json!(format!("text {seq}"))).unwrap())
                .bind(100 + seq).execute(&s.pool).await.unwrap();
        }
        let summary = s.session_summary("a").await.unwrap();
        assert_eq!(
            summary,
            json!({"id":"a","title":"Alpha","created_at":12,"chat_count":3,"chat_cursor":6})
        );
        let first = s.chat_page("a", 0, 2).await.unwrap();
        assert_eq!(first["messages"].as_array().unwrap().len(), 2);
        assert_eq!(first["messages"][0]["seq"], 2);
        assert_eq!(first["messages"][1]["seq"], 4);
        assert_eq!(first["next_seq"], 4);
        assert_eq!(first["has_more"], true);
        let second = s.chat_page("a", 4, 2).await.unwrap();
        assert_eq!(second["messages"].as_array().unwrap().len(), 1);
        assert_eq!(second["messages"][0]["seq"], 6);
        assert_eq!(second["next_seq"], 6);
        assert_eq!(second["has_more"], false);
        let empty = s.chat_page("a", 6, 2).await.unwrap();
        assert_eq!(empty["next_seq"], 6);
        assert_eq!(empty["has_more"], false);
        assert!(empty["messages"].as_array().unwrap().is_empty());
        for (after, limit) in [(-1, 1), (0, 0), (0, 51)] {
            assert!(s.chat_page("a", after, limit).await.is_err());
        }
        assert!(s.session_summary("missing").await.is_err());
        assert!(s.chat_page("missing", 0, 1).await.is_err());

        for (id, sid, updated_at) in [("r1", "a", 10), ("r2", "a", 20), ("r3", "b", 30)] {
            s.put(&format!("run:{id}"), &json!({
                "id":id,"session_id":sid,"status":"completed","updated_at":updated_at,
                "error":null,"uncertain":false,"turns":2,
                "pending":{"secret":"do not expose"},"provider_selection":{"token":"do not expose"}
            })).await.unwrap();
        }
        let runs = s.run_summaries("a", 2).await.unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0]["id"], "r2");
        assert_eq!(runs[1]["id"], "r1");
        let keys = runs[0]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            vec!["error", "id", "status", "turns", "uncertain", "updated_at"]
        );
        assert!(!serde_json::to_string(&runs)
            .unwrap()
            .contains("do not expose"));
        assert_eq!(s.run_summaries("a", 1).await.unwrap().len(), 1);
        assert!(s.run_summaries("a", 0).await.is_err());
        assert!(s.run_summaries("a", 51).await.is_err());
        assert!(s.run_summaries("missing", 1).await.is_err());
    }
    /// Verify encrypted records survive reopen and reject ciphertext tampering.
    #[tokio::test]
    async fn encrypted_records_survive_reopen_and_tampering_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let s = Store::open(root.path()).await.unwrap();
        s.put("secret", &json!({"content":"private text"}))
            .await
            .unwrap();
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key='secret'")
            .fetch_one(&s.app_pool)
            .await
            .unwrap();
        assert!(!raw.windows(12).any(|w| w == b"private text"));
        let mut corrupt = raw.clone();
        corrupt[15] ^= 1;
        assert!(s.unseal(&corrupt).is_err());
        s.app_pool.close().await;
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(
            reopened.get("secret").await.unwrap().unwrap()["content"],
            "private text"
        );
    }
    /// Verify session event replay resumes strictly after the returned sequence.
    #[tokio::test]
    async fn replay_cursor_has_no_duplicate_events() {
        let root = tempfile::tempdir().unwrap();
        let s = Store::open(root.path()).await.unwrap();
        s.event("a", "message", &json!({"text":"one"}))
            .await
            .unwrap();
        let events = s.events("a", 0).await.unwrap();
        assert_eq!(events.len(), 1);
        assert!(s
            .events("a", events[0]["seq"].as_i64().unwrap())
            .await
            .unwrap()
            .is_empty());
        assert!(s.events("b", 0).await.unwrap().is_empty());
    }
    /// Verify reopening existing data with a missing key fails without publishing a replacement.
    #[tokio::test]
    async fn missing_key_does_not_create_unusable_replacement() {
        let root = tempfile::tempdir().unwrap();
        let s = Store::open(root.path()).await.unwrap();
        s.put("secret", &json!({"content":"private text"}))
            .await
            .unwrap();
        s.pool.close().await;
        std::fs::remove_file(root.path().join("encryption.key")).unwrap();
        let err = Store::open(root.path()).await.err().unwrap();
        assert!(err.to_string().contains("encryption key is missing"));
        assert!(!root.path().join("encryption.key").exists());
    }
    /// Race key creators and verify every caller observes the same complete published key.
    #[test]
    fn concurrent_key_creation_publishes_one_complete_key() {
        let root = tempfile::tempdir().unwrap();
        let handles = (0..8)
            .map(|_| {
                let path = root.path().to_path_buf();
                std::thread::spawn(move || load_or_create_key(&path).unwrap())
            })
            .collect::<Vec<_>>();
        let keys = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert!(keys.iter().all(|key| key == &keys[0]));
        assert_eq!(
            std::fs::read(root.path().join("encryption.key")).unwrap(),
            keys[0]
        );
    }
    /// Verify progress publication is nonterminal, idempotent by stable ID and rejects conflicting text.
    #[tokio::test]
    async fn working_message_is_atomic_nonterminal_and_idempotent() {
        let root=tempfile::tempdir().unwrap();
        let store=Store::open(root.path()).await.unwrap();
        let mut run=json!({"id":"working-run","session_id":"working-session",
            "status":"running","generation":"generation","lease_epoch":0,
            "input":[{"type":"function_call_output","call_id":"call-1","output":"ok"}],
            "turns":1});
        store.put("session:working-session", &json!({"id":"working-session","created_at":1})).await.unwrap();
        let payload=json!({"id":"working:working-run:call-1","text":"Progress update"});
        store.save_run(&mut run,"working_message",&payload).await.unwrap();
        store.save_run(&mut run,"working_message",&payload).await.unwrap();
        let summary=store.session_read_summary("working-session").await.unwrap();
        assert!(summary["last_active_at"].as_i64().unwrap()>1);
        assert_eq!(summary["unread_count"],1);
        let rows=sqlx::query("SELECT id,terminal_run_id,content FROM chat WHERE run_id=?")
            .bind("working-run").fetch_all(&store.pool).await.unwrap();
        assert_eq!(rows.len(),1);
        assert_eq!(rows[0].get::<String,_>("id"),"working:working-run:call-1");
        assert!(rows[0].get::<Option<String>,_>("terminal_run_id").is_none());
        assert_eq!(store.unseal(&rows[0].get::<Vec<u8>,_>("content")).unwrap(),
            json!("Progress update"));
        let conflict=json!({"id":"working:working-run:call-1","text":"Different"});
        assert!(store.save_run(&mut run,"working_message",&conflict).await.is_err());
    }
    /// Verify automatic working bubbles retain model order and reject oversized publication without partial changes.
    #[tokio::test]
    async fn auto_promotion_is_ordered_atomic_and_idempotent() {
        let root=tempfile::tempdir().unwrap();
        let store=Store::open(root.path()).await.unwrap();
        store.put("session:auto-session",&json!({"id":"auto-session","created_at":1})).await.unwrap();
        let mut run=json!({"id":"auto-run","session_id":"auto-session","status":"running",
            "generation":"generation","lease_epoch":0,"input":[],"turns":1});
        let output=json!([
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"First"}]},
            {"type":"function_call","call_id":"read-1","name":"file_read","arguments":"{}"},
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"Second"}]}
        ]);
        let payload=json!({"output":output,"auto_promote":true});
        store.save_run(&mut run,"model_output",&payload).await.unwrap();
        store.save_run(&mut run,"model_output",&payload).await.unwrap();
        let (chat,_)=store.chat_snapshot("auto-session").await.unwrap();
        assert_eq!(chat.len(),2);
        assert_eq!(chat[0]["id"],"working:auto-run:auto:1:0");
        assert_eq!(chat[0]["content"],"First");
        assert_eq!(chat[1]["id"],"working:auto-run:auto:1:2");
        assert_eq!(chat[1]["content"],"Second");
        let activity=store.private_activity("auto-session",0).await.unwrap();
        assert_eq!(activity["events"].as_array().unwrap().len(),3);
        assert_eq!(activity["segments"].as_array().unwrap().len(),2);
        let mut oversized=json!({"id":"too-large","session_id":"auto-session","status":"running",
            "generation":"generation","lease_epoch":0,"input":[],"turns":1});
        let bad=json!({"output":[{"type":"message","content":[{"text":"x".repeat(16385)}]}],
            "auto_promote":true});
        assert!(store.save_run(&mut oversized,"model_output",&bad).await.is_err());
        let (unchanged,_)=store.chat_snapshot("auto-session").await.unwrap();
        assert_eq!(unchanged.len(),2);
    }
    /// Create a real claimed run with substantial split input for lease ownership and writer-race tests.
    async fn renewal_fixture() -> (tempfile::TempDir, Store, Value) {
        let root=tempfile::tempdir().unwrap();
        let store=Store::open(root.path()).await.unwrap();
        store.create_admin_account("root","long-hash-placeholder","generation").await.unwrap();
        store.put("session:renew-session",&json!({"id":"renew-session","agent_id":"shared_default","status":"active"})).await.unwrap();
        let mut run=json!({"id":"renew-run","session_id":"renew-session","status":"queued",
            "generation":"","run_revision":0,"turns":1,"updated_at":1,
            "input":(0..64).map(|i|json!({"role":"user","content":format!("{i}:{}","x".repeat(4096))})).collect::<Vec<_>>()});
        store.put("run:renew-run",&run).await.unwrap();
        assert!(store.claim_run(&mut run,"renew-owner","renew-generation").await.unwrap());
        (root,store,run)
    }

    /// Verify renewal uses small metadata and rejects wrong generation, owner, epoch and stopped runs.
    #[tokio::test]
    async fn renewal_metadata_keeps_large_input_and_rejects_stale_ownership() {
        let (_root,store,run)=renewal_fixture().await;
        let raw:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key='run:renew-run'").fetch_one(&store.pool).await.unwrap();
        let metadata=store.unseal(&raw).unwrap();
        assert!(metadata["input"].is_null());assert_eq!(metadata["_recovery_input"]["count"],64);
        assert_eq!(metadata["generation"],"renew-generation");assert_eq!(metadata["lease_epoch"],run["lease_epoch"]);
        sqlx::query("UPDATE run_index SET lease_expires_at=0 WHERE id='renew-run'").execute(&store.pool).await.unwrap();
        let epoch=run["lease_epoch"].as_i64().unwrap();
        assert!(store.renew_run_lease("renew-run","renew-generation","renew-owner",epoch).await.unwrap());
        let before:i64=sqlx::query_scalar("SELECT lease_expires_at FROM run_index WHERE id='renew-run'").fetch_one(&store.pool).await.unwrap();
        assert!(before>crate::now() as i64);
        for (generation,owner,attempt_epoch) in [("wrong-generation","renew-owner",epoch),
            ("renew-generation","wrong-owner",epoch),("renew-generation","renew-owner",epoch+1)] {
            assert!(!store.renew_run_lease("renew-run",generation,owner,attempt_epoch).await.unwrap());
            let after:i64=sqlx::query_scalar("SELECT lease_expires_at FROM run_index WHERE id='renew-run'").fetch_one(&store.pool).await.unwrap();
            assert_eq!(after,before);
        }
        assert!(!store.renew_run_lease("missing-run","renew-generation","renew-owner",epoch).await.unwrap());
        let mut stopped=run.clone();stopped["status"]=json!("stopped");
        store.save_run(&mut stopped,"run_status",&json!({})).await.unwrap();
        assert!(!store.renew_run_lease("renew-run","renew-generation","renew-owner",epoch).await.unwrap());
    }

    /// Demonstrate the obsolete deferred renewal snapshot fails after a real queue commit, then exercise the reserved-writer path.
    #[tokio::test]
    async fn renewal_deferred_snapshot_reproduces_concurrent_queue_write_conflict() {
        let (_root,store,run)=renewal_fixture().await;
        let mut old=store.pool.begin().await.unwrap();
        let _:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key='run:renew-run'").fetch_one(&mut *old).await.unwrap();
        let entry=json!({"id":"queued-followup","message_id":"user:queued-followup","session_id":"renew-session",
            "agent_id":"shared_default","revision":0,"status":"pending","review_status":"acceptable","choice":"queue",
            "content":"ordinary followup","context_content":"ordinary followup","attachments":[]});
        // A real queue submission commits between the old renewal read/write.
        store.followup_submit("renew-session",entry,vec![],"queued-followup","digest",&json!({})).await.unwrap();
        let failure=sqlx::query("UPDATE run_index SET lease_expires_at=? WHERE id=? AND lease_owner=? AND lease_epoch=?")
            .bind(crate::now() as i64+30).bind("renew-run").bind("renew-owner").bind(run["lease_epoch"].as_i64().unwrap())
            .execute(&mut *old).await.unwrap_err();
        assert_eq!(failure.as_database_error().and_then(|e|e.code()).as_deref(),Some("517"));
        old.rollback().await.unwrap();
        assert!(store.renew_run_lease("renew-run","renew-generation","renew-owner",run["lease_epoch"].as_i64().unwrap()).await.unwrap());
        assert_eq!(store.followup_queue("renew-session").await.unwrap()["entries"][0]["status"],"pending");
    }

    /// Verify lease renewal waits behind an active writer without spuriously abandoning ownership.
    #[tokio::test]
    async fn renewal_waits_for_queue_writer_instead_of_cancelling_owned_run() {
        let (_root,store,run)=renewal_fixture().await;
        let epoch=run["lease_epoch"].as_i64().unwrap();
        for revision in 1..=3 {
            let mut queue_writer=store.pool.begin().await.unwrap();
            let queue=json!({"schema_version":1,"session_id":"renew-session","revision":revision,
                "held":false,"reason":null,"entries":[]});
            sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
                .bind("followup_queue:renew-session").bind(store.seal(&queue).unwrap()).execute(&mut *queue_writer).await.unwrap();
            let renewed_store=store.clone();
            let (started,ready)=tokio::sync::oneshot::channel();
            let renewal=tokio::spawn(async move {
                started.send(()).unwrap();
                renewed_store.renew_run_lease("renew-run","renew-generation","renew-owner",epoch).await
            });
            ready.await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            assert!(!renewal.is_finished(),"Renewal must wait behind an existing writer, not fail a read-to-write upgrade");
            queue_writer.commit().await.unwrap();
            assert!(tokio::time::timeout(std::time::Duration::from_secs(10),renewal).await.unwrap().unwrap().unwrap());
            assert_eq!(store.followup_queue("renew-session").await.unwrap()["revision"],revision);
            assert_eq!(store.get("run:renew-run").await.unwrap().unwrap()["generation"],"renew-generation");
        }
    }

    /// Verify command acknowledgment, takeover and lease expiry preserve exactly one authorized terminal publication.
    #[tokio::test]
    async fn command_ack_and_lease_takeover_fence_terminal_publication() {
        let root=tempfile::tempdir().unwrap();
        let store=Store::open(root.path()).await.unwrap();
        if store.account_by_id("admin").await.unwrap().is_none(){store.create_admin_account("root","long-hash-placeholder","generation").await.unwrap();}

        store.put("session:s1",&json!({"id":"s1","agent_id":"shared_default","status":"active"})).await.unwrap();
        let run=json!({"id":"r1","session_id":"s1","status":"queued","generation":"",
            "input":[],"turns":1,"updated_at":1});
        store.atomic_docs_event(vec![
            ("run:r1".into(),run.clone()),
            ("receipt:message:s1:one".into(),json!({"result":{"run_id":"r1"}})),
        ],"s1","message",&json!({"role":"user","content":"hello","run_id":"r1"}))
            .await.unwrap();
        assert_eq!(store.acknowledge_loop_commands().await.unwrap(),1);
        assert_eq!(store.acknowledge_loop_commands().await.unwrap(),0);
        let mut first=store.get("run:r1").await.unwrap().unwrap();
        assert!(store.claim_run(&mut first,"loop-a","generation-a").await.unwrap());
        let mut stale_http=first.clone();
        first["pending"]=json!({"state":"executing","name":"file_write"});
        store.save_run(&mut first,"run_status",&json!({})).await.unwrap();
        stale_http["status"]=json!("stopped");
        assert!(store.save_run(&mut stale_http,"run_status",&json!({})).await.is_err());
        assert_eq!(store.get("run:r1").await.unwrap().unwrap()["pending"]["state"],
            "executing");
        let rejected=store.atomic_docs_event(vec![
            ("run:r1".into(),stale_http.clone()),
            ("receipt:run:r1:cancel:stale".into(),json!({"result":{}})),
        ],"s1","run_status",&json!({"id":"r1","status":"stopped"})).await;
        assert!(rejected.is_err());
        assert!(store.get("receipt:run:r1:cancel:stale").await.unwrap().is_none());
        first["status"]=json!("queued");
        store.save_run(&mut first,"run_status",&json!({})).await.unwrap();
        let mut second=store.get("run:r1").await.unwrap().unwrap();
        assert!(store.claim_run(&mut second,"loop-b","generation-b").await.unwrap());
        sqlx::query("UPDATE run_index SET lease_expires_at=0 WHERE id='r1'")
            .execute(&store.pool).await.unwrap();
        let mut expired=second.clone();
        expired["status"]=json!("completed");
        assert!(store.save_run(&mut expired,"model_output",
            &json!({"output":[],"terminal_text":"expired"})).await.is_err());
        sqlx::query("UPDATE run_index SET lease_expires_at=strftime('%s','now')+30 WHERE id='r1'")
            .execute(&store.pool).await.unwrap();
        first["status"]=json!("completed");
        assert!(store.save_run(&mut first,"model_output",
            &json!({"output":[],"terminal_text":"stale"})).await.is_err());
        let policy=crate::workflow::Policy::resolve(None).unwrap();
        crate::publication::terminal(&store,&mut second,&[],"accepted",&policy)
            .await.unwrap();
        crate::publication::terminal(&store,&mut second,&[],"accepted",&policy)
            .await.unwrap();
        let chat_count:i64=sqlx::query_scalar(
            "SELECT count(*) FROM chat WHERE terminal_run_id='r1'")
            .fetch_one(&store.pool).await.unwrap();
        let outbox_count:i64=sqlx::query_scalar(
            "SELECT count(*) FROM publication_outbox WHERE run_id='r1'")
            .fetch_one(&store.pool).await.unwrap();
        assert_eq!((chat_count,outbox_count),(1,1));
        let observer=Store::open(root.path()).await.unwrap();
        assert_eq!(observer.get("run:r1").await.unwrap().unwrap()["status"],"completed");
    }
    /// Status-only saves must leave large history ciphertext untouched, while
    /// append and replacement publish complete recovery state atomically.
    #[tokio::test]
    async fn recovery_input_deltas_survive_reopen_and_reject_incomplete_rows() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store.put("session:s",&json!({"id":"s","agent_id":"shared_default","status":"active"})).await.unwrap();
        let mut run = json!({"id":"delta","session_id":"s","status":"queued",
            "input":[{"role":"user","content":"雪".repeat(1000)}]});
        store.put("run:delta", &run).await.unwrap();
        let original: Vec<u8> = sqlx::query_scalar("SELECT item FROM recovery_input WHERE run_id='delta' AND ordinal=0")
            .fetch_one(&store.pool).await.unwrap();
        store.save_run(&mut run,"run_status",&json!({})).await.unwrap();
        let unchanged: Vec<u8> = sqlx::query_scalar("SELECT item FROM recovery_input WHERE run_id='delta' AND ordinal=0")
            .fetch_one(&store.pool).await.unwrap();
        assert_eq!(original,unchanged);
        run["input"].as_array_mut().unwrap().push(json!({"type":"function_call_output","call_id":"c","output":"ok"}));
        store.save_run(&mut run,"tool_result",&json!({})).await.unwrap();
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(reopened.get("run:delta").await.unwrap().unwrap(),run);
        assert_eq!(reopened.list("run:").await.unwrap(),vec![run.clone()]);
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key='run:delta'")
            .fetch_one(&store.pool).await.unwrap();
        let metadata=store.unseal(&raw).unwrap();
        assert!(metadata.get("input").is_none());
        assert_eq!(metadata["_recovery_input"]["count"],2);
        // A failed terminal save must roll back metadata and any item changes.
        let before=run.clone();
        run["status"]=json!("completed"); run["turns"]=json!(1);
        assert!(store.save_run(&mut run,"model_output",&json!({"output":[],"terminal_text":""})).await.is_err());
        assert_eq!(store.get("run:delta").await.unwrap().unwrap(),before);
        run=before; run["input"]=json!([{"role":"user","content":"replacement"}]);
        store.save_run(&mut run,"run_status",&json!({})).await.unwrap();
        assert_eq!(store.get("run:delta").await.unwrap().unwrap(),run);
        sqlx::query("DELETE FROM recovery_input WHERE run_id='delta'").execute(&store.pool).await.unwrap();
        assert!(store.get("run:delta").await.unwrap_err().to_string().contains("Incomplete recovery"));
        // RECOVERY-WRITE-REGRESSION: corruption cannot be healed by a status save.
        assert!(store.save_run(&mut run,"run_status",&json!({})).await.is_err());
        store.delete("run:delta").await.unwrap();
        assert!(store.get("run:delta").await.unwrap().is_none());
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM recovery_input WHERE run_id='delta'")
            .fetch_one(&store.pool).await.unwrap(),0);
    }

    /// Verify old inline input remains readable and upgrades through the normal checkpoint writer.
    #[tokio::test]
    async fn legacy_inline_recovery_remains_readable_and_upgrades_on_save() {
        let root=tempfile::tempdir().unwrap(); let store=Store::open(root.path()).await.unwrap();
        store.put("session:s",&json!({"id":"s","agent_id":"shared_default","status":"active"})).await.unwrap();
        let mut run=json!({"id":"legacy-input","session_id":"s","status":"queued","input":[{"role":"user","content":"old"}]});
        sqlx::query("INSERT INTO docs(key,value) VALUES('run:legacy-input',?)")
            .bind(store.seal(&run).unwrap()).execute(&store.pool).await.unwrap();
        assert_eq!(store.get("run:legacy-input").await.unwrap().unwrap(),run);
        store.save_run(&mut run,"run_status",&json!({})).await.unwrap();
        assert_eq!(store.get("run:legacy-input").await.unwrap().unwrap(),run);
    }

    /// RECOVERY-HASH-REGRESSION: streaming and buffered hashes agree for nested
    /// objects, escapes, Unicode, numeric values, empty input and large results.
    #[test]
    fn recovery_streaming_digest_matches_compact_json() {
        for item in [Value::Null, json!([]), json!({"text":"雪\n\"", "nested":[true,1,1.25]}),
            json!({"output":"x".repeat(100_000)})] {
            assert_eq!(recovery_item_digest(&item).unwrap(),
                recovery_digest(&serde_json::to_vec(&item).unwrap()));
        }
    }

    /// RECOVERY-MANIFEST-REGRESSION: exercise old split records, authenticated
    /// commitments, corruption rejection, and rollback using temporary state only.
    #[tokio::test]
    async fn recovery_manifest_upgrades_legacy_split_and_rejects_corruption() {
        let root=tempfile::tempdir().unwrap(); let store=Store::open(root.path()).await.unwrap();
        store.put("session:s",&json!({"id":"s","agent_id":"shared_default","status":"active"})).await.unwrap();
        let mut run=json!({"id":"manifest","session_id":"s","status":"queued",
            "input":[{"role":"user","content":"雪"},{"role":"assistant","content":"reply"}]});
        store.put("run:manifest",&run).await.unwrap();
        let raw:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key='run:manifest'")
            .fetch_one(&store.pool).await.unwrap();
        let mut legacy=store.unseal(&raw).unwrap();
        legacy["_recovery_input"].as_object_mut().unwrap().remove("manifest");
        sqlx::query("UPDATE docs SET value=? WHERE key='run:manifest'")
            .bind(store.seal(&legacy).unwrap()).execute(&store.pool).await.unwrap();
        let before:Vec<Vec<u8>>=sqlx::query_scalar("SELECT item FROM recovery_input WHERE run_id='manifest' ORDER BY ordinal")
            .fetch_all(&store.pool).await.unwrap();
        assert_eq!(store.get("run:manifest").await.unwrap().unwrap(),run);
        store.save_run(&mut run,"run_status",&json!({})).await.unwrap();
        let metadata_raw:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key='run:manifest'")
            .fetch_one(&store.pool).await.unwrap();
        let metadata=store.unseal(&metadata_raw).unwrap();
        assert_eq!(metadata["_recovery_input"]["manifest"].as_array().unwrap().len(),2);
        let after:Vec<Vec<u8>>=sqlx::query_scalar("SELECT item FROM recovery_input WHERE run_id='manifest' ORDER BY ordinal")
            .fetch_all(&store.pool).await.unwrap();
        assert_eq!(before,after);
        // A valid ciphertext from another ordinal must also fail, not just junk.
        for damaged in [vec![0u8;10],before[1].clone()] {
            sqlx::query("UPDATE recovery_input SET item=? WHERE run_id='manifest' AND ordinal=0")
                .bind(damaged).execute(&store.pool).await.unwrap();
            assert!(store.get("run:manifest").await.is_err());
            let revision=run["revision"].clone();
            assert!(store.save_run(&mut run,"run_status",&json!({})).await.is_err());
            assert_eq!(run["revision"],revision);
            let retained:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key='run:manifest'")
                .fetch_one(&store.pool).await.unwrap();
            assert_eq!(metadata_raw,retained);
        }
        sqlx::query("UPDATE recovery_input SET item=? WHERE run_id='manifest' AND ordinal=0")
            .bind(&before[0]).execute(&store.pool).await.unwrap();
        let mut malformed=metadata.clone(); malformed["_recovery_input"]["manifest"]=json!([]);
        sqlx::query("UPDATE docs SET value=? WHERE key='run:manifest'")
            .bind(store.seal(&malformed).unwrap()).execute(&store.pool).await.unwrap();
        assert!(store.get("run:manifest").await.is_err());
        assert!(store.save_run(&mut run,"run_status",&json!({})).await.is_err());
    }

    /// One retained LRU serves all accounts; identical session IDs remain isolated.
    #[tokio::test]
    async fn shared_context_cache_is_account_scoped_and_reconfiguration_is_global() {
        let root=tempfile::tempdir().unwrap(); let store=Store::open(root.path()).await.unwrap();
        // Fixture-only account creation; exercise the real lazy user-handle path.
        sqlx::query("INSERT INTO user_accounts(user_id,username,value) VALUES('other','other',?)")
            .bind(store.seal(&json!({"user_id":"other","username":"other"})).unwrap())
            .execute(&store.app_pool).await.unwrap();
        let other=store.for_user("other").await.unwrap();
        assert!(Arc::ptr_eq(&store.context_cache,&other.context_cache));
        assert!(Arc::ptr_eq(&store.cache,&other.cache));
        for (handle,text) in [(&store,"admin text"),(&other,"other text")] {
            handle.put("session:s",&json!({"id":"s","checkpoint_input":[{"role":"user","content":text}],"checkpoint_through":0})).await.unwrap();
            assert_eq!(handle.context("s").await.unwrap()[0]["content"],text);
        }
        assert_eq!(store.context("s").await.unwrap()[0]["content"],"admin text");
        other.put("session:s",&json!({"id":"s","checkpoint_input":[],"checkpoint_through":0})).await.unwrap();
        assert!(store.context_cache.lock().unwrap().get(&store.context_cache_key("s"), 0, 0).is_some());
        other.configure_cache(0,0);
        assert!(store.context_cache.lock().unwrap().get(&store.context_cache_key("s"), 0, 0).is_none());
        assert_eq!(store.context("s").await.unwrap()[0]["content"],"admin text");
        assert!(store.context_cache.lock().unwrap().get(&store.context_cache_key("s"), 0, 0).is_none());
    }

}

#[cfg(test)] mod recovery_measurements {
    use super::*;
    // RECOVERY-SAVE-BENCHMARK: opt-in, temporary encrypted state only. Measure
    // real transactional saves and reopen, not provider latency or total RSS.
    #[tokio::test]
    #[ignore = "repeatable recovery timing fixture"]
    async fn repeated_recovery_save_measurement() {
        let root=tempfile::tempdir().unwrap();
        let store=Store::open(root.path()).await.unwrap();
        let input=(0..512).map(|i|json!({"role":"user","content":format!("{i}:{}","x".repeat(100))})).collect::<Vec<_>>();
        let mut run=json!({"id":"bench","session_id":"bench-session","status":"queued","input":input});
        store.put("run:bench",&run).await.unwrap();
        let original:Vec<Vec<u8>>=sqlx::query_scalar("SELECT item FROM recovery_input WHERE run_id='bench' ORDER BY ordinal").fetch_all(&store.pool).await.unwrap();
        let start=std::time::Instant::now();
        for i in 0..50 {
            run["updated_at"]=json!(i);
            store.save_run(&mut run,"run_status",&json!({})).await.unwrap();
        }
        let elapsed=start.elapsed();
        let retained:Vec<Vec<u8>>=sqlx::query_scalar("SELECT item FROM recovery_input WHERE run_id='bench' ORDER BY ordinal").fetch_all(&store.pool).await.unwrap();
        assert_eq!(original,retained);
        let start=std::time::Instant::now();
        let reopened=Store::open(root.path()).await.unwrap();
        assert_eq!(reopened.get("run:bench").await.unwrap().unwrap(),run);
        println!("recovery measurement: items=512 status_saves=50 save_total={elapsed:?} reopen_and_read={:?} unchanged_ciphertext_rows=512",start.elapsed());
    }
}

#[cfg(test)]
mod admin_storage_policy_tests {
    use super::*;
    /// Write an isolated administrator encryption policy for plaintext-envelope compatibility tests.
    fn configure(root: &Path, enabled: bool) {
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::write(root.join("config/app-settings.json"),
            serde_json::to_vec(&json!({"admin_database_encryption":{"enabled":enabled}})).unwrap()).unwrap();
    }
    /// Verify live administrator plaintext policy changes preserve authentication and never weaken other storage planes.
    #[tokio::test]
    async fn admin_plaintext_is_scoped_authenticated_and_backward_compatible() {
        let root = tempfile::tempdir().unwrap();
        let old = Store::open(root.path()).await.unwrap();
        let value = json!({"private":"readable admin fixture"});
        old.put("memory:old", &value).await.unwrap();
        configure(root.path(), false);
        let store = Store::open(root.path()).await.unwrap();
        assert_eq!(store.get("memory:old").await.unwrap(), Some(value.clone()));
        store.put("memory:new", &value).await.unwrap();
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key='memory:new'").fetch_one(&store.pool).await.unwrap();
        let mut envelope: Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(envelope["value"], value);
        configure(root.path(), true);
        store.put("memory:live_encrypted", &value).await.unwrap();
        let live_encrypted: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key='memory:live_encrypted'").fetch_one(&store.pool).await.unwrap();
        assert!(serde_json::from_slice::<Value>(&live_encrypted).is_err());
        configure(root.path(), false);
        store.put("memory:live_plaintext", &value).await.unwrap();
        let live_plaintext: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key='memory:live_plaintext'").fetch_one(&store.pool).await.unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&live_plaintext).unwrap()["value"], value);
        envelope["value"]["private"] = json!("tampered");
        assert!(store.unseal(&serde_json::to_vec(&envelope).unwrap()).is_err());
        store.put("credential:fixture", &value).await.unwrap();
        let secret: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key='credential:fixture'").fetch_one(&store.app_pool).await.unwrap();
        assert!(serde_json::from_slice::<Value>(&secret).is_err());
        store.put("agent:shared_default", &value).await.unwrap();
        let agent: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key='agent:shared_default'").fetch_one(&store.agent_pool).await.unwrap();
        assert!(serde_json::from_slice::<Value>(&agent).is_err());
        let mut other = store.clone(); other.user_id = "other".into();
        assert!(serde_json::from_slice::<Value>(&other.encode_user_record(&value).unwrap()).is_err());
        assert!(serde_json::from_slice::<Value>(&store.seal(&value).unwrap()).is_err());
        configure(root.path(), true);
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(reopened.get("memory:new").await.unwrap(), Some(value.clone()));
        reopened.put("memory:encrypted_again", &value).await.unwrap();
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key='memory:encrypted_again'").fetch_one(&reopened.pool).await.unwrap();
        assert!(serde_json::from_slice::<Value>(&raw).is_err());
    }
    /// Verify an invalid encryption-policy type prevents storage initialization.
    #[tokio::test]
    async fn invalid_admin_storage_policy_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("config")).unwrap();
        std::fs::write(root.path().join("config/app-settings.json"), br#"{"admin_database_encryption":{"enabled":"false"}}"#).unwrap();
        assert!(Store::open(root.path()).await.is_err());
    }
}

// Access borrows the existing cipher, identities and pools; no resource is duplicated.
impl webagent_access::storage::RecordCodec for Store {
    fn seal(&self,value:&Value)->Result<Vec<u8>> {Store::seal(self,value)}
    fn unseal(&self,value:&[u8])->Result<Value> {Store::unseal(self,value)}
    fn encode_application(&self,value:&Value)->Result<Vec<u8>> {self.encode_for_pool(&self.app_pool,value)}
}
impl Store {
    pub(crate) fn access_repository(&self)->webagent_access::storage::Repository<'_,Self> {
        webagent_access::storage::Repository::new(&self.root,&self.app_pool,&self.agent_pool,
            &self.agent_id,&self.user_id,self)
    }
}

impl webagent_models::storage::RuntimeStorage for Store {
    fn credential_get<'a>(&'a self,key:&'a str)->impl std::future::Future<Output=Result<Option<Value>>>+Send+'a {self.get(key)}
    fn credential_put<'a>(&'a self,key:&'a str,value:&'a Value)->impl std::future::Future<Output=Result<()>>+Send+'a {self.put(key,value)}
    fn agent_pool<'a>(&'a self,agent:&'a str)->impl std::future::Future<Output=Result<SqlitePool>>+Send+'a {
        async move {Ok(self.for_agent(agent).await?.agent_pool)}
    }
}
impl Store {
    pub(crate) fn models_repository(&self)->webagent_models::storage::Repository<'_,Self> {
        webagent_models::storage::Repository::new(&self.root,&self.app_pool,&self.agent_pool,&self.agent_id,&self.user_id,self)
    }
}
