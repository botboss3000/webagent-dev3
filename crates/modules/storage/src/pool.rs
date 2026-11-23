//! Scoped SQLite authorities: one WAL pool per application, user and agent
//! database, with profile permissions and the plane schema their owners need.
use crate::cache::ContextCache;
use anyhow::{anyhow, Result};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous},
    Row, SqlitePool,
};
use std::{
    collections::HashMap,
    path::Path,
    str::FromStr,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::RwLock;
use webagent_platform::fs::PermissionsExt;

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
    // The application plane (events without runs: app.db) is shared by every account's
    // sign-in, directory, scan and relay access, so it gets twice the connections, keeps one
    // open and closes idle ones after 60 s; user and agent planes keep 4.
    let application = events && !runs;
    let idle = std::time::Duration::from_secs(if application { 60 } else { 5 });
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        // IDLE-POOL-BOUND: release idle connections without closing pools that
        // active Store clones still use. Each connection holds an OS thread, and scans visit
        // every account every 30 s, so user and agent connections must close within 5 s: at
        // 60 s the loop held one thread per account and passed the container's 1024-task limit.
        .min_connections(u32::from(application))
        .idle_timeout(idle)
        .max_connections(if application { 8 } else { 4 })
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
            CREATE INDEX IF NOT EXISTS events_session_kind ON events(session_id,kind,seq);
            CREATE TABLE IF NOT EXISTS legacy_event_map(legacy_seq INTEGER PRIMARY KEY,new_seq INTEGER NOT NULL UNIQUE);")
            .execute(&pool).await?;
        let event_columns = sqlx::query("PRAGMA table_info(events)")
            .fetch_all(&pool)
            .await?;
        if !event_columns
            .iter()
            .any(|row| row.get::<String, _>("name") == "run_id")
        {
            sqlx::query("ALTER TABLE events ADD COLUMN run_id TEXT")
                .execute(&pool)
                .await?;
        }
        // Each progress update is a full run snapshot, so a new one replaces the older ones.
        sqlx::raw_sql(
            "CREATE INDEX IF NOT EXISTS events_run ON events(run_id,seq) WHERE run_id IS NOT NULL;
            CREATE TRIGGER IF NOT EXISTS events_newest_run_status AFTER INSERT ON events
            WHEN NEW.kind='run_status' AND NEW.run_id IS NOT NULL BEGIN
            DELETE FROM events WHERE run_id=NEW.run_id AND kind='run_status' AND seq<NEW.seq; END;",
        )
        .execute(&pool)
        .await?;
    }
    // Plain revisions of the records access decisions read (`revisions`): the model catalog's
    // in the application database, the sessions' in each user database.
    if events && !runs {
        crate::revisions::install_app(&pool).await?;
    }
    if runs {
        crate::revisions::install_user(&pool).await?;
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
            CREATE TABLE IF NOT EXISTS activity_summaries(run_id TEXT NOT NULL,anchor TEXT NOT NULL,session_id TEXT NOT NULL,boundary_item_id TEXT,first_seq INTEGER NOT NULL,last_seq INTEGER NOT NULL,summary TEXT NOT NULL,PRIMARY KEY(run_id,anchor));
            CREATE INDEX IF NOT EXISTS activity_summaries_session ON activity_summaries(session_id,first_seq);
            CREATE TABLE IF NOT EXISTS activity_summary_marks(session_id TEXT PRIMARY KEY,through_seq INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS chat_context_boundaries(message_id TEXT PRIMARY KEY,through_seq INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS activity_boundary_item ON activity_boundaries(boundary_item_id);
            CREATE TABLE IF NOT EXISTS chat(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,session_id TEXT NOT NULL,run_id TEXT NOT NULL,role TEXT NOT NULL CHECK(role IN ('user','assistant','notice')),content BLOB NOT NULL,created_at INTEGER NOT NULL,terminal_run_id TEXT UNIQUE);
            CREATE INDEX IF NOT EXISTS chat_session ON chat(session_id,seq);
            CREATE TABLE IF NOT EXISTS loop_commands(seq INTEGER PRIMARY KEY AUTOINCREMENT,command_id TEXT NOT NULL UNIQUE,run_id TEXT NOT NULL,schema_version INTEGER NOT NULL CHECK(schema_version=1),kind TEXT NOT NULL,payload BLOB NOT NULL,created_at INTEGER NOT NULL,acknowledged_at INTEGER);
            CREATE INDEX IF NOT EXISTS loop_commands_run ON loop_commands(run_id,seq);
            CREATE INDEX IF NOT EXISTS loop_commands_open ON loop_commands(seq) WHERE acknowledged_at IS NULL;
            CREATE TABLE IF NOT EXISTS loop_events(seq INTEGER PRIMARY KEY AUTOINCREMENT,run_id TEXT NOT NULL,schema_version INTEGER NOT NULL CHECK(schema_version=1),kind TEXT NOT NULL,payload BLOB NOT NULL,created_at INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS loop_events_run ON loop_events(run_id,seq);
            CREATE TRIGGER IF NOT EXISTS loop_events_newest_run_status AFTER INSERT ON loop_events
            WHEN NEW.kind='run_status' BEGIN
            DELETE FROM loop_events WHERE run_id=NEW.run_id AND kind='run_status' AND seq<NEW.seq; END;
            CREATE TABLE IF NOT EXISTS publication_outbox(run_id TEXT PRIMARY KEY,schema_version INTEGER NOT NULL CHECK(schema_version=1),payload BLOB NOT NULL,created_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS session_artifacts(session_id TEXT NOT NULL,artifact_id TEXT NOT NULL,kind TEXT NOT NULL,revision INTEGER NOT NULL,run_id TEXT NOT NULL,bytes INTEGER NOT NULL,body BLOB NOT NULL,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,PRIMARY KEY(session_id,artifact_id));")
            .execute(&pool).await?;
        // Recovery input is encrypted item-by-item, separate from small run
        // metadata. Both are committed atomically; interactions remain untouched.
        sqlx::query("CREATE TABLE IF NOT EXISTS recovery_input(run_id TEXT NOT NULL,ordinal INTEGER NOT NULL,item BLOB NOT NULL,PRIMARY KEY(run_id,ordinal))")
            .execute(&pool).await?;
        sqlx::query("CREATE TABLE IF NOT EXISTS reasoning_cleanup(segment_id TEXT PRIMARY KEY,session_id TEXT NOT NULL,payload BLOB NOT NULL)")
            .execute(&pool).await?;
        // Background task index and delivery marks (`Repository::background_index`), here
        // once per open instead of before every background read.
        sqlx::raw_sql("CREATE TABLE IF NOT EXISTS background_task_index(id TEXT PRIMARY KEY,session_id TEXT NOT NULL,revision INTEGER NOT NULL,status TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS background_task_delivery(id TEXT PRIMARY KEY,run_id TEXT NOT NULL);")
            .execute(&pool).await?;
        // Per-account app notifications (storage warnings, automatic cleanups).
        sqlx::query("CREATE TABLE IF NOT EXISTS user_notices(id INTEGER PRIMARY KEY AUTOINCREMENT,kind TEXT NOT NULL,severity TEXT NOT NULL,body BLOB NOT NULL,created_at INTEGER NOT NULL,read_at INTEGER)")
            .execute(&pool).await?;
        // Which dictionary and row each idle session was last repacked with.
        sqlx::query("CREATE TABLE IF NOT EXISTS compress_marks(session_id TEXT PRIMARY KEY,dictionary TEXT NOT NULL,through INTEGER NOT NULL)")
            .execute(&pool).await?;
        // How far each session's tool calls are trimmed to their tools' retention.
        sqlx::query("CREATE TABLE IF NOT EXISTS retention_marks(session_id TEXT PRIMARY KEY,through INTEGER NOT NULL)")
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
        // Unfinished runs only (covering for the scans' existence test), so scans skip the never-pruned
        // finished rows.
        // Queries must repeat this exact predicate for SQLite to use the index.
        sqlx::query("CREATE INDEX IF NOT EXISTS run_index_open ON run_index(enqueue_seq,status) WHERE status NOT IN ('completed','cancelled','failed')")
            .execute(&pool)
            .await?;
        widen_chat_roles(&pool).await?;
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

/// Rebuilds a chat table created before run-status `notice` rows existed.
/// SQLite cannot change a CHECK constraint in place, so the rows move to a
/// copy with the same columns, ids and seq values, and the AUTOINCREMENT
/// counter keeps its high-water mark so seq values are never reused.
async fn widen_chat_roles(pool: &SqlitePool) -> Result<()> {
    let sql: Option<String> =
        sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type='table' AND name='chat'")
            .fetch_optional(pool)
            .await?;
    if sql.is_none_or(|sql| sql.contains("'notice'")) {
        return Ok(());
    }
    // One script on one connection; the temp table carries the counter across the drop.
    sqlx::raw_sql("BEGIN IMMEDIATE;
        CREATE TEMP TABLE chat_counter AS SELECT seq FROM sqlite_sequence WHERE name='chat';
        CREATE TABLE chat_rebuild(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,session_id TEXT NOT NULL,run_id TEXT NOT NULL,role TEXT NOT NULL CHECK(role IN ('user','assistant','notice')),content BLOB NOT NULL,created_at INTEGER NOT NULL,terminal_run_id TEXT UNIQUE);
        INSERT INTO chat_rebuild(seq,id,session_id,run_id,role,content,created_at,terminal_run_id) SELECT seq,id,session_id,run_id,role,content,created_at,terminal_run_id FROM chat;
        DROP TABLE chat;
        ALTER TABLE chat_rebuild RENAME TO chat;
        CREATE INDEX IF NOT EXISTS chat_session ON chat(session_id,seq);
        INSERT INTO sqlite_sequence(name,seq) SELECT 'chat',0 WHERE EXISTS(SELECT 1 FROM temp.chat_counter) AND NOT EXISTS(SELECT 1 FROM sqlite_sequence WHERE name='chat');
        UPDATE sqlite_sequence SET seq=MAX(seq,(SELECT seq FROM temp.chat_counter)) WHERE name='chat' AND EXISTS(SELECT 1 FROM temp.chat_counter);
        DROP TABLE temp.chat_counter;
        COMMIT;")
        .execute(pool)
        .await?;
    Ok(())
}

/// One account database with its disposable caches and context invalidation
/// gate, shared by every Store clone bound to that account.
#[derive(Clone)]
pub struct UserPlane {
    pub pool: SqlitePool,
    pub context_cache: Arc<Mutex<ContextCache>>,
    pub context_gate: Arc<RwLock<()>>,
    /// Set once this process has verified the account's one-time migration markers.
    pub migrated: Arc<AtomicBool>,
    /// When this process last selected the account ([`UserPlane::touch`]); orders eviction.
    pub last_use: Arc<AtomicU64>,
}

impl UserPlane {
    /// Keep one user pool and its cache invalidation gate shared across Store clones.
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            context_cache: Arc::new(Mutex::new(ContextCache::new(16 * 1024 * 1024))),
            context_gate: Arc::new(RwLock::new(())),
            migrated: Arc::new(AtomicBool::new(false)),
            last_use: Arc::new(AtomicU64::new(USE_SEQUENCE.fetch_add(1, Ordering::Relaxed))),
        }
    }

    /// Record that a Store handle was just made for this account.
    pub fn touch(&self) {
        self.last_use.store(
            USE_SEQUENCE.fetch_add(1, Ordering::Relaxed),
            Ordering::Relaxed,
        );
    }
}

static USE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Account databases that may hold open connections at once unless
/// `config/storage/storage-settings.json` `user_database_pools.max_connected` says otherwise.
pub const DEFAULT_MAX_CONNECTED_USERS: usize = 64;
/// The lowest accepted bound; below it ordinary concurrent work would evict itself.
pub const MIN_CONNECTED_USERS: usize = 8;
static MAX_CONNECTED_USERS: AtomicUsize = AtomicUsize::new(DEFAULT_MAX_CONNECTED_USERS);

/// Set the process-wide bound on connected account databases (read once at `Store::open`).
pub fn set_max_connected_users(limit: usize) {
    MAX_CONNECTED_USERS.store(limit.max(MIN_CONNECTED_USERS), Ordering::Relaxed);
}

/// The process-wide bound on connected account databases.
pub fn max_connected_users() -> usize {
    MAX_CONNECTED_USERS.load(Ordering::Relaxed)
}

/// Registered account databases that hold at least one open connection.
pub fn connected_users(states: &HashMap<String, UserPlane>) -> usize {
    states
        .values()
        .filter(|state| state.pool.size() > 0)
        .count()
}

/// CONNECTED-USER-BOUND: every SQLite connection holds an OS thread and an idle one lives
/// 5-10 s, so a pass that opens every account quickly (start-up, storage limits, an
/// administrator page listing every account) used to hold a thread per account and reach the
/// container's 1,024-task limit ("Resource temporarily unavailable"). Past `limit` connected
/// entries, the idle ones used longest ago are removed down to three quarters of it. A removed
/// pool closes its connections when its last handle drops (the caller drops the returned
/// planes after releasing the registry lock); the next visit reopens it. Like
/// [`retain_active_users`], entries with a live Store clone and the administrator are kept.
#[must_use]
pub fn evict_idle_connected(
    states: &mut HashMap<String, UserPlane>,
    limit: usize,
) -> Vec<UserPlane> {
    let connected = connected_users(states);
    if connected <= limit {
        return Vec::new();
    }
    let mut idle: Vec<(u64, String)> = states
        .iter()
        .filter(|(id, state)| {
            *id != "admin" && state.pool.size() > 0 && Arc::strong_count(&state.context_gate) == 1
        })
        .map(|(id, state)| (state.last_use.load(Ordering::Relaxed), id.clone()))
        .collect();
    idle.sort_unstable();
    idle.into_iter()
        .take(connected - limit * 3 / 4)
        .filter_map(|(_, id)| states.remove(&id))
        .collect()
}

/// Registered user databases kept before idle ones are dropped. An idle entry holds no
/// connection (pools keep none idle), so keeping many is cheap; dropping one makes the next
/// visit reopen it, rerun its schema statements and recheck its migration markers.
pub const MAX_USER_PLANES: usize = 4096;

/// INACTIVE-USER-HANDLES: past [`MAX_USER_PLANES`] entries, drop idle ones until three
/// quarters remain, not every idle one at once (that made every scheduler pass reopen the
/// databases of accounts it had just visited). The gate's sole owner is the registry only
/// when no Store clone can still use this user. Never evict active gates or close their
/// pools; that would split context invalidation synchronization.
pub fn retain_active_users(states: &mut HashMap<String, UserPlane>) {
    if states.len() < MAX_USER_PLANES {
        return;
    }
    let excess = states.len() - MAX_USER_PLANES * 3 / 4;
    let idle: Vec<String> = states
        .iter()
        .filter(|(id, state)| *id != "admin" && Arc::strong_count(&state.context_gate) == 1)
        .map(|(id, _)| id.clone())
        .take(excess)
        .collect();
    for id in idle {
        states.remove(&id);
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    fn planes(count: usize) -> HashMap<String, UserPlane> {
        (0..count)
            .map(|i| {
                let pool = SqlitePool::connect_lazy("sqlite::memory:").unwrap();
                (format!("user-{i}"), UserPlane::new(pool))
            })
            .collect()
    }

    #[tokio::test]
    async fn connected_accounts_stay_under_the_bound_oldest_idle_first() {
        let root = tempfile::tempdir().unwrap();
        let mut states = HashMap::new();
        for i in 0..12 {
            let pool = open_pool(
                &root.path().join(format!("user_data/u{i}/u{i}.db")),
                true,
                true,
            )
            .await
            .unwrap();
            states.insert(format!("u{i}"), UserPlane::new(pool));
        }
        let admin = open_pool(&root.path().join("user_data/admin/admin.db"), true, true)
            .await
            .unwrap();
        states.insert("admin".to_owned(), UserPlane::new(admin));
        // u0 is in use by a Store clone; u1 was selected most recently.
        let in_use = states["u0"].clone();
        states["u1"].touch();
        assert_eq!(connected_users(&states), 13);
        // At or under the bound nothing goes.
        assert!(evict_idle_connected(&mut states, 13).is_empty());
        let removed = evict_idle_connected(&mut states, 8);
        // Down to three quarters of the bound: 13 - 6 = 7 removed.
        assert_eq!(removed.len(), 7);
        assert_eq!(connected_users(&states), 6);
        for kept in ["admin", "u0", "u1"] {
            assert!(states.contains_key(kept), "{kept} must stay");
        }
        // The oldest idle entries went first.
        for gone in ["u2", "u3", "u4", "u5", "u6", "u7", "u8"] {
            assert!(!states.contains_key(gone), "{gone} should be evicted first");
        }
        drop(in_use);
        drop(removed);
    }

    #[test]
    fn the_bound_never_goes_below_its_floor() {
        set_max_connected_users(1);
        assert_eq!(max_connected_users(), MIN_CONNECTED_USERS);
        set_max_connected_users(DEFAULT_MAX_CONNECTED_USERS);
        assert_eq!(max_connected_users(), DEFAULT_MAX_CONNECTED_USERS);
    }

    #[tokio::test]
    async fn registry_keeps_idle_users_below_the_limit() {
        let mut states = planes(1000);
        retain_active_users(&mut states);
        assert_eq!(states.len(), 1000);
    }
}
