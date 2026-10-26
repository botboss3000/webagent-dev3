//! Scoped SQLite authorities: one WAL pool per application, user and agent
//! database, with profile permissions and the plane schema their owners need.
use crate::cache::{Cache, ContextCache};
use anyhow::{anyhow, Result};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous},
    Row, SqlitePool,
};
use std::{
    collections::HashMap,
    os::unix::fs::PermissionsExt,
    path::Path,
    str::FromStr,
    sync::{Arc, Mutex},
};
use tokio::sync::RwLock;

/// Open a durable WAL authority with bounded connections, profile permissions and the tables its owner needs.
/// `events` adds the event log (user and application planes); `runs` adds the user-plane run schema.
/// Notification staging tables are added by their owner after this returns.
pub async fn open_pool(path: &Path, events: bool, runs: bool) -> Result<SqlitePool> {
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
    std::fs::set_permissions(
        parent,
        std::fs::Permissions::from_mode(if private_profile { 0o700 } else { 0o770 }),
    )?;
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
    std::fs::set_permissions(
        path,
        std::fs::Permissions::from_mode(if private_profile { 0o600 } else { 0o660 }),
    )?;
    sqlx::query("CREATE TABLE IF NOT EXISTS docs (key TEXT PRIMARY KEY, value BLOB NOT NULL)")
        .execute(&pool)
        .await?;
    if events {
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
            CREATE TABLE IF NOT EXISTS chat(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,session_id TEXT NOT NULL,run_id TEXT NOT NULL,role TEXT NOT NULL CHECK(role IN ('user','assistant')),content BLOB NOT NULL,created_at INTEGER NOT NULL,terminal_run_id TEXT UNIQUE);
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
        let interaction_columns = sqlx::query("PRAGMA table_info(interactions)")
            .fetch_all(&pool)
            .await?;
        if !interaction_columns
            .iter()
            .any(|row| row.get::<String, _>("name") == "execution_metadata")
        {
            sqlx::query("ALTER TABLE interactions ADD COLUMN execution_metadata BLOB")
                .execute(&pool)
                .await?;
        }
        let columns = sqlx::query("PRAGMA table_info(run_index)")
            .fetch_all(&pool)
            .await?;
        if !columns
            .iter()
            .any(|r| r.get::<String, _>("name") == "enqueue_seq")
        {
            sqlx::query("ALTER TABLE run_index ADD COLUMN enqueue_seq INTEGER")
                .execute(&pool)
                .await?;
        }
        sqlx::query("UPDATE run_index SET enqueue_seq=rowid WHERE enqueue_seq IS NULL")
            .execute(&pool)
            .await?;
        for (name, declaration) in [
            ("lease_epoch", "INTEGER NOT NULL DEFAULT 0"),
            ("lease_owner", "TEXT"),
            ("lease_expires_at", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            if !columns.iter().any(|r| r.get::<String, _>("name") == name) {
                sqlx::query(&format!(
                    "ALTER TABLE run_index ADD COLUMN {name} {declaration}"
                ))
                .execute(&pool)
                .await?;
            }
        }
    }
    Ok(pool)
}

/// One account database with its disposable caches and context invalidation
/// gate, shared by every Store clone bound to that account.
#[derive(Clone)]
pub struct UserPlane {
    pub pool: SqlitePool,
    pub cache: Arc<Mutex<Cache>>,
    pub context_cache: Arc<Mutex<ContextCache>>,
    pub context_gate: Arc<RwLock<()>>,
}

impl UserPlane {
    /// Keep one user pool and its cache invalidation gate shared across Store clones.
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            cache: Arc::new(Mutex::new(Cache::new(64 * 1024 * 1024))),
            context_cache: Arc::new(Mutex::new(ContextCache::new(16 * 1024 * 1024))),
            context_gate: Arc::new(RwLock::new(())),
        }
    }
}

/// INACTIVE-USER-HANDLES: keep at most 64 idle registry entries. The gate's
/// sole owner is the registry only when no Store clone can still use this
/// user. Never evict active gates or close their pools; that would split
/// context invalidation synchronization.
pub fn retain_active_users(states: &mut HashMap<String, UserPlane>) {
    if states.len() >= 64 {
        states.retain(|id, state| id == "admin" || Arc::strong_count(&state.context_gate) > 1);
    }
}
