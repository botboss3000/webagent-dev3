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
//! - User database: `session_revisions` per session: `record` (every write of the session
//!   record), `mode` (`session_mode:`), `abilities` (`session_ability_state:`). The session
//!   record is rewritten for many reasons (activity time, flags, names), so access decisions
//!   compare the plain `session_header` instead: its status, agent and participation, kept
//!   beside the `record` counter it was read at (`session_header` below).
//! - User database (stream B6): `run_revisions` per run: `record` moves with every write of the
//!   `run:` document and of its split input rows (`recovery_input`), so a decoded run record,
//!   input included, can be reused while it stands. `session_index`: plain session facts
//!   (status, agent, parent, flags, times) kept beside the `record` counter they were read at.
//!   `doc_owner`: the plain ownership ids of a document (which session or run it belongs to),
//!   recorded by a session deletion that decrypted it; every write or delete of the document
//!   removes the row in the writing statement, so a row exists only for an unchanged document.
//! - Application database: `plain_revisions` rows `model_catalog` and `platform_catalog` (the
//!   platform roster `platform_catalog:v1` and every `platform_credential:v1:` record: a
//!   withdrawn or unpublished platform model, or a rotated key, refuses the model grant).
//! - Counters that never restart for a re-created id: the agent database keeps a random
//!   `agent_instance` set when the database is created, and each `user_accounts` row has an
//!   `account_instance` number from a sequence that is never reused (set by triggers on the
//!   row's insert, so a deleted and re-created account reads as a new one). Decisions compare
//!   (instance, counter), never the counter alone.
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
/// The platform model roster in the application database (`webagent_models` platform catalog).
pub const PLATFORM_CATALOG_KEY: &str = "platform_catalog:v1";
/// Key prefix of the platform credential records beside it.
pub const PLATFORM_CREDENTIAL_PREFIX: &str = "platform_credential:v1:";

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

/// Statements that move run `id`'s counter (an SQL expression), only when `when` holds.
fn run_bump(id: &str, when: &str) -> String {
    format!(
        "INSERT INTO run_revisions(run_id) SELECT {id} WHERE {when}
            AND NOT EXISTS(SELECT 1 FROM run_revisions WHERE run_id={id});
        UPDATE run_revisions SET record=record+1 WHERE run_id={id} AND {when};"
    )
}

/// The split run-input table, created here exactly as `pool::open_pool` creates it, so its
/// triggers can be installed first (`IF NOT EXISTS`: whichever runs first creates it).
const RECOVERY_TABLE: &str = "CREATE TABLE IF NOT EXISTS recovery_input(run_id TEXT NOT NULL,ordinal INTEGER NOT NULL,item BLOB NOT NULL,PRIMARY KEY(run_id,ordinal));";

fn user_script() -> String {
    let run_new = run_bump("substr(NEW.key,5)", "1");
    let run_old = run_bump("substr(OLD.key,5)", "1");
    let run_new_if = run_bump("substr(NEW.key,5)", "NEW.key GLOB 'run:*'");
    let run_old_if = run_bump(
        "substr(OLD.key,5)",
        "OLD.key GLOB 'run:*' AND OLD.key IS NOT NEW.key",
    );
    let input_new = run_bump("NEW.run_id", "1");
    let input_old = run_bump("OLD.run_id", "1");
    let input_old_if = run_bump("OLD.run_id", "OLD.run_id IS NOT NEW.run_id");
    let recovery_table = RECOVERY_TABLE;
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
        CREATE TABLE IF NOT EXISTS session_header(session_id TEXT PRIMARY KEY,
            checked INTEGER NOT NULL, status TEXT, agent_id TEXT, participation TEXT NOT NULL);
        CREATE TRIGGER IF NOT EXISTS rev_session_insert AFTER INSERT ON docs
            WHEN {new_matches} BEGIN {insert_bump} END;
        CREATE TRIGGER IF NOT EXISTS rev_session_update AFTER UPDATE ON docs
            WHEN ({new_matches} OR {old_matches})
                AND (OLD.key IS NOT NEW.key OR OLD.value IS NOT NEW.value)
            BEGIN {new_bump} {old_bump} END;
        CREATE TRIGGER IF NOT EXISTS rev_session_delete AFTER DELETE ON docs
            WHEN {old_matches} BEGIN {delete_bump} END;
        CREATE TABLE IF NOT EXISTS session_index(session_id TEXT PRIMARY KEY,
            checked INTEGER NOT NULL, status TEXT, agent_id TEXT, parent_id TEXT,
            created_at INTEGER NOT NULL, flags INTEGER NOT NULL, deleted_since INTEGER);
        CREATE TABLE IF NOT EXISTS run_revisions(run_id TEXT PRIMARY KEY,
            record INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS doc_owner(key TEXT PRIMARY KEY, owners TEXT NOT NULL);
        CREATE TRIGGER IF NOT EXISTS rev_owner_insert AFTER INSERT ON docs
            BEGIN DELETE FROM doc_owner WHERE key=NEW.key; END;
        CREATE TRIGGER IF NOT EXISTS rev_owner_update AFTER UPDATE ON docs
            WHEN OLD.key IS NOT NEW.key OR OLD.value IS NOT NEW.value
            BEGIN DELETE FROM doc_owner WHERE key=OLD.key OR key=NEW.key; END;
        CREATE TRIGGER IF NOT EXISTS rev_owner_delete AFTER DELETE ON docs
            BEGIN DELETE FROM doc_owner WHERE key=OLD.key; END;
        {recovery_table}
        CREATE TRIGGER IF NOT EXISTS rev_run_insert AFTER INSERT ON docs
            WHEN NEW.key GLOB 'run:*' BEGIN {run_new} END;
        CREATE TRIGGER IF NOT EXISTS rev_run_update AFTER UPDATE ON docs
            WHEN (NEW.key GLOB 'run:*' OR OLD.key GLOB 'run:*')
                AND (OLD.key IS NOT NEW.key OR OLD.value IS NOT NEW.value)
            BEGIN {run_new_if} {run_old_if} END;
        CREATE TRIGGER IF NOT EXISTS rev_run_delete AFTER DELETE ON docs
            WHEN OLD.key GLOB 'run:*' BEGIN {run_old} END;
        CREATE TRIGGER IF NOT EXISTS rev_run_input_insert AFTER INSERT ON recovery_input
            BEGIN {input_new} END;
        CREATE TRIGGER IF NOT EXISTS rev_run_input_update AFTER UPDATE ON recovery_input
            BEGIN {input_new} {input_old_if} END;
        CREATE TRIGGER IF NOT EXISTS rev_run_input_delete AFTER DELETE ON recovery_input
            BEGIN {input_old} END;
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
        INSERT OR IGNORE INTO agent_epoch(singleton,epoch) VALUES(1,0);
        CREATE TABLE IF NOT EXISTS agent_instance(singleton INTEGER PRIMARY KEY CHECK(singleton=1),
            instance TEXT NOT NULL);
        INSERT OR IGNORE INTO agent_instance(singleton,instance)
            VALUES(1,lower(hex(randomblob(16))));",
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
const PLATFORM_BUMP: &str = "INSERT INTO plain_revisions(name,revision) SELECT 'platform_catalog',0
        WHERE NOT EXISTS(SELECT 1 FROM plain_revisions WHERE name='platform_catalog');
    UPDATE plain_revisions SET revision=revision+1 WHERE name='platform_catalog';";

/// Whether `side`'s key (`NEW` or `OLD`) is a platform catalog record.
fn platform_key(side: &str) -> String {
    format!(
        "({side}.key='{PLATFORM_CATALOG_KEY}' OR {side}.key GLOB '{PLATFORM_CREDENTIAL_PREFIX}*')"
    )
}

fn app_script() -> String {
    let new_platform = platform_key("NEW");
    let old_platform = platform_key("OLD");
    format!(
        "CREATE TABLE IF NOT EXISTS plain_revisions(name TEXT PRIMARY KEY, revision INTEGER NOT NULL);
        INSERT OR IGNORE INTO plain_revisions(name,revision) VALUES('model_catalog',0);
        INSERT OR IGNORE INTO plain_revisions(name,revision) VALUES('platform_catalog',0);
        CREATE TRIGGER IF NOT EXISTS rev_catalog_insert AFTER INSERT ON docs
            WHEN NEW.key='{MODEL_CATALOG_KEY}' BEGIN {CATALOG_BUMP} END;
        CREATE TRIGGER IF NOT EXISTS rev_catalog_update AFTER UPDATE ON docs
            WHEN (OLD.key='{MODEL_CATALOG_KEY}' OR NEW.key='{MODEL_CATALOG_KEY}')
                AND (OLD.key IS NOT NEW.key OR OLD.value IS NOT NEW.value)
            BEGIN {CATALOG_BUMP} END;
        CREATE TRIGGER IF NOT EXISTS rev_catalog_delete AFTER DELETE ON docs
            WHEN OLD.key='{MODEL_CATALOG_KEY}' BEGIN {CATALOG_BUMP} END;
        CREATE TRIGGER IF NOT EXISTS rev_platform_insert AFTER INSERT ON docs
            WHEN {new_platform} BEGIN {PLATFORM_BUMP} END;
        CREATE TRIGGER IF NOT EXISTS rev_platform_update AFTER UPDATE ON docs
            WHEN ({old_platform} OR {new_platform})
                AND (OLD.key IS NOT NEW.key OR OLD.value IS NOT NEW.value)
            BEGIN {PLATFORM_BUMP} END;
        CREATE TRIGGER IF NOT EXISTS rev_platform_delete AFTER DELETE ON docs
            WHEN {old_platform} BEGIN {PLATFORM_BUMP} END;"
    )
}

// A row's instance comes from the AUTOINCREMENT key, which SQLite never hands out twice in
// one database, so a deleted and re-created account never reads its old (instance, epoch).
// The row is replaced on every insert of the account and on a change of its id.
const ACCOUNT_INSTANCE: &str = "DELETE FROM account_instance WHERE user_id=NEW.user_id;
    INSERT INTO account_instance(user_id) VALUES(NEW.user_id);";

fn account_script() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS account_instance(instance INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id TEXT NOT NULL UNIQUE);
        CREATE TRIGGER IF NOT EXISTS rev_account_insert AFTER INSERT ON user_accounts
            BEGIN {ACCOUNT_INSTANCE} END;
        CREATE TRIGGER IF NOT EXISTS rev_account_update AFTER UPDATE ON user_accounts
            WHEN OLD.user_id IS NOT NEW.user_id
            BEGIN DELETE FROM account_instance WHERE user_id=OLD.user_id; {ACCOUNT_INSTANCE} END;
        CREATE TRIGGER IF NOT EXISTS rev_account_delete AFTER DELETE ON user_accounts
            BEGIN DELETE FROM account_instance WHERE user_id=OLD.user_id; END;
        INSERT INTO account_instance(user_id) SELECT user_id FROM user_accounts
            WHERE user_id NOT IN (SELECT user_id FROM account_instance) ORDER BY user_id;"
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

/// Counts the family's triggers and tables (`objects`); the script runs unless all exist.
fn present_query(objects: &[&str]) -> String {
    let names = objects
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "SELECT count(*) FROM sqlite_master WHERE type IN ('trigger','table') AND name IN ({names})"
    )
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

const USER_TRIGGERS: [&str; 16] = [
    "rev_session_insert",
    "rev_session_update",
    "rev_session_delete",
    "session_header",
    "session_index",
    "run_revisions",
    "rev_run_insert",
    "rev_run_update",
    "rev_run_delete",
    "rev_run_input_insert",
    "rev_run_input_update",
    "rev_run_input_delete",
    "doc_owner",
    "rev_owner_insert",
    "rev_owner_update",
    "rev_owner_delete",
];
const AGENT_TRIGGERS: [&str; 13] = [
    "agent_instance",
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
const APP_TRIGGERS: [&str; 6] = [
    "rev_catalog_insert",
    "rev_catalog_update",
    "rev_catalog_delete",
    "rev_platform_insert",
    "rev_platform_update",
    "rev_platform_delete",
];
const ACCOUNT_TRIGGERS: [&str; 4] = [
    "account_instance",
    "rev_account_insert",
    "rev_account_update",
    "rev_account_delete",
];

static USER: std::sync::OnceLock<Texts> = std::sync::OnceLock::new();
static AGENT: std::sync::OnceLock<Texts> = std::sync::OnceLock::new();
static APP: std::sync::OnceLock<Texts> = std::sync::OnceLock::new();
static ACCOUNTS: std::sync::OnceLock<Texts> = std::sync::OnceLock::new();

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
/// Application database, after `user_accounts` exists (Accounts initialization).
pub async fn install_accounts(pool: &SqlitePool) -> anyhow::Result<()> {
    let texts = texts(&ACCOUNTS, &ACCOUNT_TRIGGERS, account_script);
    install(pool, &texts.present, ACCOUNT_TRIGGERS.len(), &texts.script).await
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

/// The write counter that moves with every write of document `key`, as `executor` sees it
/// (in a transaction: that transaction's snapshot): `session:` (record), `session_mode:`,
/// `session_ability_state:` and `run:` (the run document and its split input rows). `None`
/// for any other key, and when the counter is missing or unreadable (read the document again).
pub async fn document_counter<'c, E: sqlx::SqliteExecutor<'c>>(
    executor: E,
    key: &str,
) -> Option<i64> {
    let (query, id) = counted(key)?;
    sqlx::query_scalar(query)
        .bind(id)
        .fetch_optional(executor)
        .await
        .ok()?
}

/// The counter query and id for a counted document key (see [`document_counter`]).
pub fn counted(key: &str) -> Option<(&'static str, &str)> {
    if let Some(id) = key.strip_prefix("run:") {
        return Some(("SELECT record FROM run_revisions WHERE run_id=?", id));
    }
    for (prefix, column) in SESSION_FAMILIES {
        if let Some(id) = key.strip_prefix(prefix) {
            let query = match column {
                "record" => "SELECT record FROM session_revisions WHERE session_id=?",
                "mode" => "SELECT mode FROM session_revisions WHERE session_id=?",
                _ => "SELECT abilities FROM session_revisions WHERE session_id=?",
            };
            return Some((query, id));
        }
    }
    None
}

/// The access-relevant part of a session record, in plain values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionHeader {
    pub status: Option<String>,
    pub agent_id: Option<String>,
    /// The session's participation state (active, revision, generation) as JSON text.
    pub participation: String,
}

impl SessionHeader {
    /// The header of a session record, with the participation default of
    /// `webagent_chat::shared::participation_state`.
    pub fn of(session: &serde_json::Value) -> Self {
        let participation = session
            .get("participation")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"active":true,"revision":0,"generation":0}));
        Self {
            status: session["status"].as_str().map(str::to_owned),
            agent_id: session["agent_id"].as_str().map(str::to_owned),
            participation: participation.to_string(),
        }
    }
}

/// The session's header at its current `record` counter. When the stored header was read at
/// that counter it is used as is (one indexed read, no decryption). Otherwise the session
/// record is read and decrypted in the same read transaction as the counter, and the header
/// is stored with that counter (newest first) for the next reader. Not a timed cache: it is
/// used only while the counter, moved by every write of the record, is unchanged. `None`
/// (read as changed) when the counter, the record or its decryption is missing.
pub async fn session_header(
    pool: &SqlitePool,
    sid: &str,
    unseal: impl Fn(&[u8]) -> anyhow::Result<serde_json::Value>,
) -> Option<SessionHeader> {
    let mut tx = pool.begin().await.ok()?;
    let record: i64 = sqlx::query_scalar("SELECT record FROM session_revisions WHERE session_id=?")
        .bind(sid)
        .fetch_optional(&mut *tx)
        .await
        .ok()??;
    let stored = sqlx::query(
        "SELECT checked,status,agent_id,participation FROM session_header WHERE session_id=?",
    )
    .bind(sid)
    .fetch_optional(&mut *tx)
    .await
    .ok()?;
    if let Some(row) = stored {
        if row.try_get::<i64, _>("checked").ok()? == record {
            let header = SessionHeader {
                status: row.try_get("status").ok()?,
                agent_id: row.try_get("agent_id").ok()?,
                participation: row.try_get("participation").ok()?,
            };
            tx.commit().await.ok()?;
            return Some(header);
        }
    }
    let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(format!("session:{sid}"))
        .fetch_optional(&mut *tx)
        .await
        .ok()??;
    tx.commit().await.ok()?;
    let header = SessionHeader::of(&unseal(&raw).ok()?);
    // Best effort: a failed write only means the next reader decrypts again.
    let _ = sqlx::query(
        "INSERT INTO session_header(session_id,checked,status,agent_id,participation)
        VALUES(?,?,?,?,?) ON CONFLICT(session_id) DO UPDATE SET checked=excluded.checked,
        status=excluded.status,agent_id=excluded.agent_id,participation=excluded.participation
        WHERE excluded.checked>session_header.checked",
    )
    .bind(sid)
    .bind(record)
    .bind(&header.status)
    .bind(&header.agent_id)
    .bind(&header.participation)
    .execute(pool)
    .await;
    Some(header)
}

/// `SessionFacts::flags`: the record is the agent's Main Chat (`main_chat` true).
pub const FLAG_MAIN_CHAT: i64 = 1;
/// `SessionFacts::flags`: a module's silent session (`module_silent` true).
pub const FLAG_MODULE_SILENT: i64 = 2;
/// `SessionFacts::flags`: a sub-agent session (`parent_session_id` is a string).
pub const FLAG_CHILD: i64 = 4;
/// `SessionFacts::flags`: naming still waits for the first message (`auto_naming.stage`).
pub const FLAG_AWAITING_MESSAGE: i64 = 8;

/// The plain facts of one session record that counting, drafts, sub-agent lookups and the
/// purge of deleted sessions need (stream B6): ids, status, flags and times only, no content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionFacts {
    pub id: String,
    pub status: Option<String>,
    /// The record's `agent_id` as stored (callers apply the `shared_default` default).
    pub agent_id: Option<String>,
    pub parent_id: Option<String>,
    pub created_at: i64,
    pub flags: i64,
    /// When the record was first seen deleted: its `deleted_at`, else the first read that saw
    /// the tombstone (so a purge waits at least the retention period after either).
    pub deleted_since: Option<i64>,
}

impl SessionFacts {
    pub fn of(
        id: &str,
        session: &serde_json::Value,
        earlier_deleted: Option<i64>,
        now: i64,
    ) -> Self {
        let status = session["status"].as_str().map(str::to_owned);
        let flag = |on: bool, bit: i64| if on { bit } else { 0 };
        let flags = flag(session["main_chat"] == true, FLAG_MAIN_CHAT)
            | flag(session["module_silent"] == true, FLAG_MODULE_SILENT)
            | flag(session["parent_session_id"].is_string(), FLAG_CHILD)
            | flag(
                session["auto_naming"]["stage"] == "awaiting_message",
                FLAG_AWAITING_MESSAGE,
            );
        let deleted_since = (status.as_deref() == Some("deleted")).then(|| {
            earlier_deleted
                .or_else(|| session["deleted_at"].as_i64())
                .unwrap_or(now)
        });
        Self {
            id: id.to_owned(),
            status,
            agent_id: session["agent_id"].as_str().map(str::to_owned),
            parent_id: session["parent_session_id"].as_str().map(str::to_owned),
            created_at: session["created_at"].as_i64().unwrap_or(0),
            flags,
            deleted_since,
        }
    }

    pub fn has(&self, flag: i64) -> bool {
        self.flags & flag != 0
    }
}

/// The facts of every `session:` record in the user database, in key order, without
/// decrypting any record whose stored facts were read at its current `record` counter. The
/// list comes from the record keys themselves, so it holds exactly the records that exist;
/// a record whose facts are missing or older than its counter (or that has no counter) is read
/// and decrypted in the same read transaction, and its facts are stored with that counter for
/// the next reader (newest first, best effort). `None` when anything cannot be read: callers
/// then decrypt the records as before.
pub async fn session_facts(
    pool: &SqlitePool,
    unseal: impl Fn(&[u8]) -> anyhow::Result<serde_json::Value>,
    now: i64,
) -> Option<Vec<SessionFacts>> {
    let mut tx = pool.begin().await.ok()?;
    let rows = sqlx::query(
        "SELECT substr(d.key,9) AS id, r.record, i.checked, i.status, i.agent_id, i.parent_id,
            i.created_at, i.flags, i.deleted_since
        FROM docs d LEFT JOIN session_revisions r ON r.session_id=substr(d.key,9)
            LEFT JOIN session_index i ON i.session_id=substr(d.key,9)
        WHERE d.key>=? AND d.key<? ORDER BY d.key",
    )
    .bind("session:")
    .bind(format!("session:{}", char::MAX))
    .fetch_all(&mut *tx)
    .await
    .ok()?;
    let mut facts = Vec::with_capacity(rows.len());
    let mut refreshed = Vec::new();
    for row in rows {
        let id: String = row.try_get("id").ok()?;
        let record: Option<i64> = row.try_get("record").ok()?;
        let checked: Option<i64> = row.try_get("checked").ok()?;
        let stored_status: Option<String> = row.try_get("status").ok()?;
        let stored_since: Option<i64> = row.try_get("deleted_since").ok()?;
        if record.is_some() && checked == record {
            facts.push(SessionFacts {
                id,
                status: stored_status,
                agent_id: row.try_get("agent_id").ok()?,
                parent_id: row.try_get("parent_id").ok()?,
                created_at: row.try_get("created_at").ok()?,
                flags: row.try_get("flags").ok()?,
                deleted_since: stored_since,
            });
            continue;
        }
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(format!("session:{id}"))
            .fetch_one(&mut *tx)
            .await
            .ok()?;
        let earlier = (stored_status.as_deref() == Some("deleted"))
            .then_some(stored_since)
            .flatten();
        let fact = SessionFacts::of(&id, &unseal(&raw).ok()?, earlier, now);
        if let Some(record) = record {
            refreshed.push((record, fact.clone()));
        }
        facts.push(fact);
    }
    tx.commit().await.ok()?;
    if !refreshed.is_empty() {
        // Best effort: a failed write only means the next reader decrypts again.
        let _ = store_session_facts(pool, &refreshed).await;
    }
    Some(facts)
}

async fn store_session_facts(
    pool: &SqlitePool,
    facts: &[(i64, SessionFacts)],
) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    for (record, fact) in facts {
        sqlx::query(
            "INSERT INTO session_index(session_id,checked,status,agent_id,parent_id,created_at,
                flags,deleted_since) VALUES(?,?,?,?,?,?,?,?)
            ON CONFLICT(session_id) DO UPDATE SET checked=excluded.checked,status=excluded.status,
                agent_id=excluded.agent_id,parent_id=excluded.parent_id,
                created_at=excluded.created_at,flags=excluded.flags,
                deleted_since=excluded.deleted_since
            WHERE excluded.checked>session_index.checked",
        )
        .bind(&fact.id)
        .bind(record)
        .bind(&fact.status)
        .bind(&fact.agent_id)
        .bind(&fact.parent_id)
        .bind(fact.created_at)
        .bind(fact.flags)
        .bind(fact.deleted_since)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Drop the stored facts of a session whose records were purged, inside the purging
/// transaction. The counters in `session_revisions` stay, so a counter never goes back.
pub async fn forget_session_facts(
    connection: &mut sqlx::SqliteConnection,
    sid: &str,
) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM session_index WHERE session_id=?")
        .bind(sid)
        .execute(connection)
        .await?;
    Ok(())
}

/// Every document key of the user database in key order, with its recorded ownership ids
/// (`doc_owner.owners`, JSON text) when the document is unchanged since they were recorded.
/// Read in the caller's transaction.
pub async fn document_owners(
    connection: &mut sqlx::SqliteConnection,
) -> anyhow::Result<Vec<(String, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT d.key, o.owners FROM docs d LEFT JOIN doc_owner o ON o.key=d.key ORDER BY d.key",
    )
    .fetch_all(connection)
    .await?)
}

/// Record a document's ownership ids, read from its current value in the caller's write
/// transaction (which must hold the writer lock, so the value cannot change before commit).
pub async fn record_document_owners(
    connection: &mut sqlx::SqliteConnection,
    key: &str,
    owners: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO doc_owner(key,owners) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET owners=excluded.owners",
    )
    .bind(key)
    .bind(owners)
    .execute(connection)
    .await?;
    Ok(())
}

/// Whether `user` is a shared participant of session `sid` (`shared_participants`, application
/// database). No table yet means no participants; any other failure is `None` (changed).
pub async fn shared_participant(app: &SqlitePool, sid: &str, user: &str) -> Option<bool> {
    match sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM shared_participants WHERE session_id=? AND user_id=?",
    )
    .bind(sid)
    .bind(user)
    .fetch_optional(app)
    .await
    {
        Ok(row) => Some(row.is_some()),
        Err(error) if error.to_string().contains("no such table") => Some(false),
        Err(_) => None,
    }
}

/// The agent database's epoch; `None` when unreadable (read as changed).
pub async fn agent(pool: &SqlitePool) -> Option<i64> {
    sqlx::query_scalar("SELECT epoch FROM agent_epoch WHERE singleton=1")
        .fetch_optional(pool)
        .await
        .ok()?
}

/// The agent database's instance (random, set when the database was created) and epoch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRevision {
    pub instance: String,
    pub epoch: i64,
}

/// Both in one read; `None` when either is missing or unreadable (read as changed).
pub async fn agent_revision(pool: &SqlitePool) -> Option<AgentRevision> {
    let row = sqlx::query(
        "SELECT i.instance AS instance, e.epoch AS epoch FROM agent_epoch e, agent_instance i
        WHERE e.singleton=1 AND i.singleton=1",
    )
    .fetch_optional(pool)
    .await
    .ok()??;
    Some(AgentRevision {
        instance: row.try_get("instance").ok()?,
        epoch: row.try_get("epoch").ok()?,
    })
}

/// The model catalog's plain revision; `None` when unreadable (read as changed).
pub async fn model_catalog(app: &SqlitePool) -> Option<i64> {
    sqlx::query_scalar("SELECT revision FROM plain_revisions WHERE name='model_catalog'")
        .fetch_optional(app)
        .await
        .ok()?
}

/// The platform catalog's plain revision (roster and credentials); `None` when unreadable.
pub async fn platform_catalog(app: &SqlitePool) -> Option<i64> {
    sqlx::query_scalar("SELECT revision FROM plain_revisions WHERE name='platform_catalog'")
        .fetch_optional(app)
        .await
        .ok()?
}

/// The agent's owner and both accounts' (`account_instance`, `auth_epoch`), in one
/// application-database read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principals {
    pub owner_id: String,
    pub caller_epoch: i64,
    pub owner_epoch: i64,
    pub caller_instance: i64,
    pub owner_instance: i64,
}

/// `None` (read as changed) when the directory row, either account, either epoch or either
/// instance (`install_accounts` not run) is missing.
pub async fn principals(app: &SqlitePool, caller: &str, agent: &str) -> Option<Principals> {
    let row = sqlx::query(
        "SELECT d.owner_id AS owner_id,
            (SELECT auth_epoch FROM user_accounts WHERE user_id=?1) AS caller_epoch,
            (SELECT auth_epoch FROM user_accounts WHERE user_id=d.owner_id) AS owner_epoch,
            (SELECT instance FROM account_instance WHERE user_id=?1) AS caller_instance,
            (SELECT instance FROM account_instance WHERE user_id=d.owner_id) AS owner_instance
        FROM agent_directory d WHERE d.id=?2",
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
        caller_instance: row.try_get::<Option<i64>, _>("caller_instance").ok()??,
        owner_instance: row.try_get::<Option<i64>, _>("owner_instance").ok()??,
    })
}

#[cfg(test)]
#[path = "revisions_tests.rs"]
mod tests;
