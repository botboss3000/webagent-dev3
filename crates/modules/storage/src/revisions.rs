//! Plain revision numbers for the records access decisions read (stream B4).
//!
//! Each record family has one writer of its revision: a set of SQLite triggers installed by one
//! function below. A trigger runs inside the statement, so the revision moves in the same
//! transaction as every write of the family, whichever code issued it (the families are written
//! by many raw statements and generic document writes). Nothing else may write the revision
//! tables or drop these triggers; `revision_tables_have_one_writer` searches the workspace for that.
//!
//! - Agent database: `agent_epoch` (one row) moves with every change of the agent row (metadata:
//!   ownership, capability levels, modes, ability settings, permissions; status, model), its
//!   memberships, its ability connections and its main-chat state.
//! - User database: `session_revisions` per session: `record` (the session record: status,
//!   agent, participation generation), `mode` (`session_mode:`), `abilities`
//!   (`session_ability_state:`).
//! - Application database: `plain_revisions` row `model_catalog`.
//!
//! Reads fail closed: a missing table, row or value reads as `None`, which callers treat as
//! "changed". Re-sealing an unchanged value counts as a change (the stored bytes differ); that
//! only costs a rebuild.
use sqlx::{Row, SqlitePool};

/// Trigger names start with this prefix; the guard refuses any `DROP TRIGGER` of them.
pub const TRIGGER_PREFIX: &str = "rev_";

/// Session families in the user database: key prefix and the counter it moves.
const SESSION_FAMILIES: [(&str, &str); 3] = [
    ("session:", "record"),
    ("session_mode:", "mode"),
    ("session_ability_state:", "abilities"),
];

/// The key of the model catalog document in the application database.
pub const MODEL_CATALOG_KEY: &str = "model_selection:catalog:v1";
/// The agent database's main-chat state document (its schedule approval decides modes).
pub const MAIN_CHAT_KEY: &str = "main_chat";

fn session_matches(side: &str) -> String {
    SESSION_FAMILIES
        .iter()
        .map(|(prefix, _)| format!("{side}.key GLOB '{prefix}*'"))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// Statements that move the counters of the session named by `side`'s key (`NEW` or `OLD`),
/// only when `extra` holds.
fn session_bump(side: &str, extra: &str) -> String {
    let mut id = String::from("CASE");
    let mut sets = Vec::new();
    for (prefix, column) in SESSION_FAMILIES {
        id.push_str(&format!(
            " WHEN {side}.key GLOB '{prefix}*' THEN substr({side}.key,{})",
            prefix.len() + 1
        ));
        sets.push(format!("{column}={column}+({side}.key GLOB '{prefix}*')"));
    }
    id.push_str(" END");
    let matches = format!("({}) AND {extra}", session_matches(side));
    format!(
        "INSERT INTO session_revisions(session_id) SELECT {id} WHERE ({matches})
            AND NOT EXISTS(SELECT 1 FROM session_revisions WHERE session_id=({id}));
        UPDATE session_revisions SET {} WHERE session_id=({id}) AND ({matches});",
        sets.join(",")
    )
}

fn user_script() -> String {
    let new_matches = session_matches("NEW");
    let old_matches = session_matches("OLD");
    let new_bump = session_bump("NEW", "1");
    let insert_bump = new_bump.clone();
    // An update moves the old key's session too only when the key itself changed.
    let old_bump = session_bump("OLD", "OLD.key IS NOT NEW.key");
    let delete_bump = session_bump("OLD", "1");
    // The seed lists existing sessions once, so a session written before the triggers existed
    // still has a revision; the triggers keep it from then on.
    format!(
        "CREATE TABLE IF NOT EXISTS session_revisions(session_id TEXT PRIMARY KEY,
            record INTEGER NOT NULL DEFAULT 0, mode INTEGER NOT NULL DEFAULT 0,
            abilities INTEGER NOT NULL DEFAULT 0);
        CREATE TRIGGER IF NOT EXISTS rev_session_insert AFTER INSERT ON docs
            WHEN {new_matches} BEGIN {insert_bump} END;
        CREATE TRIGGER IF NOT EXISTS rev_session_update AFTER UPDATE ON docs
            WHEN ({new_matches} OR {old_matches})
                AND (OLD.key IS NOT NEW.key OR OLD.value IS NOT NEW.value)
            BEGIN {new_bump} {old_bump} END;
        CREATE TRIGGER IF NOT EXISTS rev_session_delete AFTER DELETE ON docs
            WHEN {old_matches} BEGIN {delete_bump} END;
        INSERT OR IGNORE INTO session_revisions(session_id)
            SELECT substr(key,9) FROM docs WHERE key GLOB 'session:*';
        INSERT OR IGNORE INTO session_revisions(session_id)
            SELECT substr(key,14) FROM docs WHERE key GLOB 'session_mode:*';
        INSERT OR IGNORE INTO session_revisions(session_id)
            SELECT substr(key,23) FROM docs WHERE key GLOB 'session_ability_state:*';"
    )
}

// Inside a trigger the outer statement's conflict policy replaces the body's own (an outer
// upsert would turn `OR IGNORE` into a failure), so absent rows are added with `NOT EXISTS`.
const AGENT_BUMP: &str = "INSERT INTO agent_epoch(singleton,epoch) SELECT 1,0
        WHERE NOT EXISTS(SELECT 1 FROM agent_epoch WHERE singleton=1);
    UPDATE agent_epoch SET epoch=epoch+1 WHERE singleton=1;";

fn agent_script() -> String {
    let mut script = String::from(
        "CREATE TABLE IF NOT EXISTS agent_epoch(singleton INTEGER PRIMARY KEY CHECK(singleton=1),
            epoch INTEGER NOT NULL);
        INSERT OR IGNORE INTO agent_epoch(singleton,epoch) VALUES(1,0);",
    );
    // A lock statement (`SET revision=revision`) changes no column and moves nothing.
    let tables = [
        (
            "agents",
            "OLD.id IS NOT NEW.id OR OLD.name IS NOT NEW.name OR OLD.description IS NOT NEW.description
            OR OLD.provider IS NOT NEW.provider OR OLD.model IS NOT NEW.model
            OR OLD.status IS NOT NEW.status OR OLD.metadata IS NOT NEW.metadata
            OR OLD.revision IS NOT NEW.revision",
            "1",
        ),
        (
            "agent_memberships",
            "OLD.user_id IS NOT NEW.user_id OR OLD.agent_id IS NOT NEW.agent_id
            OR OLD.value IS NOT NEW.value",
            "1",
        ),
        (
            "agent_connections",
            "OLD.id IS NOT NEW.id OR OLD.agent_id IS NOT NEW.agent_id
            OR OLD.connection_type IS NOT NEW.connection_type OR OLD.section IS NOT NEW.section
            OR OLD.enabled IS NOT NEW.enabled OR OLD.config IS NOT NEW.config
            OR OLD.revision IS NOT NEW.revision",
            "1",
        ),
        (
            "docs",
            "(OLD.key IS NOT NEW.key OR OLD.value IS NOT NEW.value)
            AND (OLD.key='main_chat' OR NEW.key='main_chat')",
            "{side}.key='main_chat'",
        ),
    ];
    for (table, changed, when) in tables {
        let new_when = when.replace("{side}", "NEW");
        let old_when = when.replace("{side}", "OLD");
        script.push_str(&format!(
            "CREATE TRIGGER IF NOT EXISTS rev_{table}_insert AFTER INSERT ON {table}
                WHEN {new_when} BEGIN {AGENT_BUMP} END;
            CREATE TRIGGER IF NOT EXISTS rev_{table}_update AFTER UPDATE ON {table}
                WHEN {changed} BEGIN {AGENT_BUMP} END;
            CREATE TRIGGER IF NOT EXISTS rev_{table}_delete AFTER DELETE ON {table}
                WHEN {old_when} BEGIN {AGENT_BUMP} END;"
        ));
    }
    script
}

const CATALOG_BUMP: &str = "INSERT INTO plain_revisions(name,revision) SELECT 'model_catalog',0
        WHERE NOT EXISTS(SELECT 1 FROM plain_revisions WHERE name='model_catalog');
    UPDATE plain_revisions SET revision=revision+1 WHERE name='model_catalog';";

fn app_script() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS plain_revisions(name TEXT PRIMARY KEY, revision INTEGER NOT NULL);
        INSERT OR IGNORE INTO plain_revisions(name,revision) VALUES('model_catalog',0);
        CREATE TRIGGER IF NOT EXISTS rev_catalog_insert AFTER INSERT ON docs
            WHEN NEW.key='{MODEL_CATALOG_KEY}' BEGIN {CATALOG_BUMP} END;
        CREATE TRIGGER IF NOT EXISTS rev_catalog_update AFTER UPDATE ON docs
            WHEN (OLD.key='{MODEL_CATALOG_KEY}' OR NEW.key='{MODEL_CATALOG_KEY}')
                AND (OLD.key IS NOT NEW.key OR OLD.value IS NOT NEW.value)
            BEGIN {CATALOG_BUMP} END;
        CREATE TRIGGER IF NOT EXISTS rev_catalog_delete AFTER DELETE ON docs
            WHEN OLD.key='{MODEL_CATALOG_KEY}' BEGIN {CATALOG_BUMP} END;"
    )
}

/// Runs `script` once per database: when every named trigger exists it reads one row and
/// returns. Otherwise it creates tables, triggers and seeds in one `BEGIN IMMEDIATE`
/// transaction (every statement is idempotent, so two processes racing do the same thing).
/// Both texts are built once per process and are `'static`, as sqlx's executor bounds need.
async fn install(
    pool: &SqlitePool,
    present: &'static str,
    count: usize,
    script: &'static str,
) -> anyhow::Result<()> {
    let found: i64 = sqlx::query_scalar(present).fetch_one(pool).await?;
    if found == count as i64 {
        return Ok(());
    }
    // One script on one connection (as `pool::widen_chat_roles` does); the script carries
    // its own `BEGIN IMMEDIATE` and `COMMIT`.
    sqlx::raw_sql(script).execute(pool).await?;
    Ok(())
}

fn present_query(triggers: &[&str]) -> String {
    let names = triggers
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(",");
    format!("SELECT count(*) FROM sqlite_master WHERE type='trigger' AND name IN ({names})")
}

/// The installation texts of one family, built once.
struct Texts {
    present: String,
    script: String,
}

fn texts(
    cell: &'static std::sync::OnceLock<Texts>,
    triggers: &[&str],
    script: fn() -> String,
) -> &'static Texts {
    cell.get_or_init(|| Texts {
        present: present_query(triggers),
        script: format!("BEGIN IMMEDIATE; {} COMMIT;", script()),
    })
}

const USER_TRIGGERS: [&str; 3] = [
    "rev_session_insert",
    "rev_session_update",
    "rev_session_delete",
];
const AGENT_TRIGGERS: [&str; 12] = [
    "rev_agents_insert",
    "rev_agents_update",
    "rev_agents_delete",
    "rev_agent_memberships_insert",
    "rev_agent_memberships_update",
    "rev_agent_memberships_delete",
    "rev_agent_connections_insert",
    "rev_agent_connections_update",
    "rev_agent_connections_delete",
    "rev_docs_insert",
    "rev_docs_update",
    "rev_docs_delete",
];
const APP_TRIGGERS: [&str; 3] = [
    "rev_catalog_insert",
    "rev_catalog_update",
    "rev_catalog_delete",
];

static USER: std::sync::OnceLock<Texts> = std::sync::OnceLock::new();
static AGENT: std::sync::OnceLock<Texts> = std::sync::OnceLock::new();
static APP: std::sync::OnceLock<Texts> = std::sync::OnceLock::new();

/// User database (`docs` must exist).
pub async fn install_user(pool: &SqlitePool) -> anyhow::Result<()> {
    let texts = texts(&USER, &USER_TRIGGERS, user_script);
    install(pool, &texts.present, USER_TRIGGERS.len(), &texts.script).await
}
/// Agent database, after `agents`, `agent_memberships`, `agent_connections` and `docs` exist.
pub async fn install_agent(pool: &SqlitePool) -> anyhow::Result<()> {
    let texts = texts(&AGENT, &AGENT_TRIGGERS, agent_script);
    install(pool, &texts.present, AGENT_TRIGGERS.len(), &texts.script).await
}
/// Application database (`docs` must exist).
pub async fn install_app(pool: &SqlitePool) -> anyhow::Result<()> {
    let texts = texts(&APP, &APP_TRIGGERS, app_script);
    install(pool, &texts.present, APP_TRIGGERS.len(), &texts.script).await
}

/// The session's three counters; `None` when unreadable (read as changed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionRevisions {
    pub record: i64,
    pub mode: i64,
    pub abilities: i64,
}

pub async fn session(pool: &SqlitePool, sid: &str) -> Option<SessionRevisions> {
    let row = sqlx::query("SELECT record,mode,abilities FROM session_revisions WHERE session_id=?")
        .bind(sid)
        .fetch_optional(pool)
        .await
        .ok()??;
    Some(SessionRevisions {
        record: row.try_get("record").ok()?,
        mode: row.try_get("mode").ok()?,
        abilities: row.try_get("abilities").ok()?,
    })
}

/// The agent database's epoch; `None` when unreadable (read as changed).
pub async fn agent(pool: &SqlitePool) -> Option<i64> {
    sqlx::query_scalar("SELECT epoch FROM agent_epoch WHERE singleton=1")
        .fetch_optional(pool)
        .await
        .ok()?
}

/// The model catalog's plain revision; `None` when unreadable (read as changed).
pub async fn model_catalog(app: &SqlitePool) -> Option<i64> {
    sqlx::query_scalar("SELECT revision FROM plain_revisions WHERE name='model_catalog'")
        .fetch_optional(app)
        .await
        .ok()?
}

/// The agent's owner and both accounts' `auth_epoch`, in one application-database read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principals {
    pub owner_id: String,
    pub caller_epoch: i64,
    pub owner_epoch: i64,
}

/// `None` (read as changed) when the directory row, either account or either epoch is missing.
pub async fn principals(app: &SqlitePool, caller: &str, agent: &str) -> Option<Principals> {
    let row = sqlx::query(
        "SELECT d.owner_id AS owner_id,
            (SELECT auth_epoch FROM user_accounts WHERE user_id=?) AS caller_epoch,
            (SELECT auth_epoch FROM user_accounts WHERE user_id=d.owner_id) AS owner_epoch
        FROM agent_directory d WHERE d.id=?",
    )
    .bind(caller)
    .bind(agent)
    .fetch_optional(app)
    .await
    .ok()??;
    Some(Principals {
        owner_id: row.try_get("owner_id").ok()?,
        caller_epoch: row.try_get::<Option<i64>, _>("caller_epoch").ok()??,
        owner_epoch: row.try_get::<Option<i64>, _>("owner_epoch").ok()??,
    })
}

#[cfg(test)]
#[path = "revisions_tests.rs"]
mod tests;
