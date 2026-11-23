use super::*;
use std::path::{Path, PathBuf};

async fn pool(root: &Path, name: &str) -> SqlitePool {
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(root.join(name))
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(5));
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE IF NOT EXISTS docs (key TEXT PRIMARY KEY, value BLOB NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    pool
}

async fn agent_pool(root: &Path, name: &str) -> SqlitePool {
    let pool = pool(root, name).await;
    sqlx::raw_sql("CREATE TABLE IF NOT EXISTS agent_memberships(user_id TEXT NOT NULL,agent_id TEXT NOT NULL,value BLOB NOT NULL,PRIMARY KEY(user_id,agent_id));
        CREATE TABLE IF NOT EXISTS agents(id TEXT PRIMARY KEY, name TEXT NOT NULL, description TEXT NOT NULL,
        provider TEXT NOT NULL, model TEXT NOT NULL, status TEXT NOT NULL, metadata BLOB NOT NULL,
        revision INTEGER NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS agent_connections(id TEXT PRIMARY KEY, agent_id TEXT NOT NULL,
        connection_type TEXT NOT NULL, section TEXT NOT NULL, enabled INTEGER NOT NULL, config BLOB NOT NULL,
        revision INTEGER NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);")
        .execute(&pool)
        .await
        .unwrap();
    pool
}

async fn run(pool: &SqlitePool, sql: &str) {
    sqlx::raw_sql(sql).execute(pool).await.unwrap();
}

#[tokio::test]
async fn session_families_move_their_own_counter_in_the_writing_transaction() {
    let root = tempfile::tempdir().unwrap();
    let user = pool(root.path(), "user.db").await;
    // A session written before the triggers existed is seeded, not missing.
    run(&user, "INSERT INTO docs VALUES('session:old',x'01')").await;
    install_user(&user).await.unwrap();
    install_user(&user).await.unwrap();
    assert_eq!(
        session(&user, "old").await,
        Some(SessionRevisions {
            record: 0,
            mode: 0,
            abilities: 0
        })
    );
    // Another process: a second pool on the same file.
    let other = pool(root.path(), "user.db").await;
    run(&other, "INSERT INTO docs VALUES('session:s1',x'01')").await;
    let first = session(&user, "s1").await.unwrap();
    assert_eq!(first.record, 1);
    // Locks change nothing.
    run(&other, "UPDATE docs SET value=value WHERE key='session:s1'").await;
    run(&other, "UPDATE docs SET key=key WHERE key='session:s1'").await;
    assert_eq!(session(&user, "s1").await, Some(first));
    // Every write form moves the record counter (a replace may move it more than once).
    let mut last = first;
    for sql in [
        "UPDATE docs SET value=x'02' WHERE key='session:s1'",
        "INSERT OR REPLACE INTO docs VALUES('session:s1',x'03')",
        "INSERT INTO docs VALUES('session:s1',x'04') ON CONFLICT(key) DO UPDATE SET value=excluded.value",
    ] {
        run(&other, sql).await;
        let now = session(&user, "s1").await.unwrap();
        assert!(now.record > last.record && now.mode == 0 && now.abilities == 0, "{sql}");
        last = now;
    }
    run(&other, "INSERT INTO docs VALUES('session_mode:s1',x'01')").await;
    run(
        &other,
        "UPDATE docs SET value=x'02' WHERE key='session_mode:s1'",
    )
    .await;
    run(
        &other,
        "INSERT INTO docs VALUES('session_ability_state:s1',x'01')",
    )
    .await;
    let now = session(&user, "s1").await.unwrap();
    assert_eq!((now.record, now.mode, now.abilities), (last.record, 2, 1));
    // Another session and unrelated keys leave s1 alone.
    run(&other, "INSERT INTO docs VALUES('session:s2',x'01'); INSERT INTO docs VALUES('session_chat_controls:s1',x'01'); INSERT INTO docs VALUES('run:s1',x'01')").await;
    assert_eq!(session(&user, "s1").await, Some(now));
    // A rolled-back write moves nothing.
    let mut tx = other.begin().await.unwrap();
    sqlx::query("UPDATE docs SET value=x'09' WHERE key='session:s1'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(session(&user, "s1").await, Some(now));
    run(&other, "DELETE FROM docs WHERE key='session:s1'").await;
    assert_eq!(session(&user, "s1").await.unwrap().record, now.record + 1);
    // A renamed key moves both sessions.
    run(
        &other,
        "UPDATE docs SET key='session:s3' WHERE key='session:s2'",
    )
    .await;
    assert_eq!(session(&user, "s2").await.unwrap().record, 2);
    assert_eq!(session(&user, "s3").await.unwrap().record, 1);
}

#[tokio::test]
async fn missing_revisions_read_as_unknown() {
    let root = tempfile::tempdir().unwrap();
    let bare = pool(root.path(), "bare.db").await;
    assert_eq!(session(&bare, "s1").await, None);
    assert_eq!(agent(&bare).await, None);
    assert_eq!(model_catalog(&bare).await, None);
    assert_eq!(principals(&bare, "u", "a").await, None);
    install_user(&bare).await.unwrap();
    assert_eq!(session(&bare, "never-written").await, None);
}

#[tokio::test]
async fn agent_epoch_moves_with_every_agent_family() {
    let root = tempfile::tempdir().unwrap();
    let pool = agent_pool(root.path(), "agent.db").await;
    install_agent(&pool).await.unwrap();
    install_agent(&pool).await.unwrap();
    let other = agent_pool(root.path(), "agent.db").await;
    let steps: [(&str, bool); 19] = [
        ("INSERT INTO agents VALUES('a','A','','p','m','active',x'01',1,0,0)", true),
        ("UPDATE agents SET revision=revision WHERE id='a'", false),
        ("UPDATE agents SET updated_at=5 WHERE id='a'", false),
        ("UPDATE agents SET metadata=x'02' WHERE id='a'", true),
        ("UPDATE agents SET status='archived',revision=revision+1 WHERE id='a'", true),
        ("UPDATE agents SET provider='q',model='n' WHERE id='a'", true),
        ("INSERT INTO agent_memberships VALUES('u','a',x'01')", true),
        ("UPDATE agent_memberships SET value=x'02' WHERE user_id='u'", true),
        ("INSERT INTO agent_memberships VALUES('u','a',x'03') ON CONFLICT(user_id,agent_id) DO UPDATE SET value=excluded.value", true),
        ("DELETE FROM agent_memberships WHERE user_id='u'", true),
        ("INSERT INTO agent_connections VALUES('ability:x','a','x','ability',1,x'01',1,0,0)", true),
        ("UPDATE agent_connections SET enabled=0 WHERE id='ability:x'", true),
        ("UPDATE agent_connections SET updated_at=9 WHERE id='ability:x'", false),
        ("DELETE FROM agent_connections", true),
        ("INSERT INTO docs VALUES('main_chat',x'01')", true),
        ("UPDATE docs SET value=x'02' WHERE key='main_chat'", true),
        ("INSERT INTO docs VALUES('agent:a:memory',x'01')", false),
        ("DELETE FROM docs WHERE key='main_chat'", true),
        ("DELETE FROM agents WHERE id='a'", true),
    ];
    let mut last = agent(&pool).await.unwrap();
    for (sql, expect) in steps {
        run(&other, sql).await;
        let now = agent(&pool).await.unwrap();
        assert_eq!(now != last, expect, "{sql}");
        last = now;
    }
}

#[tokio::test]
async fn model_catalog_revision_moves_only_with_the_catalog_document() {
    let root = tempfile::tempdir().unwrap();
    let app = pool(root.path(), "app.db").await;
    install_app(&app).await.unwrap();
    assert_eq!(model_catalog(&app).await, Some(0));
    let other = pool(root.path(), "app.db").await;
    run(
        &other,
        "INSERT INTO docs VALUES('model_selection:catalog:v1',x'01')",
    )
    .await;
    run(
        &other,
        "UPDATE docs SET value=value WHERE key='model_selection:catalog:v1'",
    )
    .await;
    run(&other, "INSERT INTO docs VALUES('other',x'01')").await;
    assert_eq!(model_catalog(&app).await, Some(1));
    run(
        &other,
        "UPDATE docs SET value=x'02' WHERE key='model_selection:catalog:v1'",
    )
    .await;
    run(
        &other,
        "DELETE FROM docs WHERE key='model_selection:catalog:v1'",
    )
    .await;
    assert_eq!(model_catalog(&app).await, Some(3));
}

#[tokio::test]
async fn principals_read_owner_and_both_epochs_or_nothing() {
    let root = tempfile::tempdir().unwrap();
    let app = pool(root.path(), "app.db").await;
    run(&app, "CREATE TABLE agent_directory(id TEXT PRIMARY KEY, owner_id TEXT NOT NULL);
        CREATE TABLE user_accounts(user_id TEXT PRIMARY KEY, auth_epoch INTEGER NOT NULL DEFAULT 0);
        INSERT INTO agent_directory VALUES('a','owner'); INSERT INTO user_accounts VALUES('caller',3);").await;
    // The owner's account is missing: unknown.
    assert_eq!(principals(&app, "caller", "a").await, None);
    run(&app, "INSERT INTO user_accounts VALUES('owner',7)").await;
    assert_eq!(
        principals(&app, "caller", "a").await,
        Some(Principals {
            owner_id: "owner".into(),
            caller_epoch: 3,
            owner_epoch: 7
        })
    );
    assert_eq!(principals(&app, "nobody", "a").await, None);
    assert_eq!(principals(&app, "caller", "missing").await, None);
}

/// Statements that would write a revision outside its triggers, or remove the triggers or the
/// tables they watch. Only `revisions.rs` may contain them outside tests.
const FORBIDDEN: [&str; 20] = [
    "INTO session_revisions",
    "UPDATE session_revisions",
    "DELETE FROM session_revisions",
    "INTO agent_epoch",
    "UPDATE agent_epoch",
    "DELETE FROM agent_epoch",
    "INTO plain_revisions",
    "UPDATE plain_revisions",
    "DELETE FROM plain_revisions",
    "DROP TABLE session_revisions",
    "DROP TABLE agent_epoch",
    "DROP TABLE plain_revisions",
    "DROP TRIGGER rev_",
    "DROP TRIGGER IF EXISTS rev_",
    "TRIGGER IF NOT EXISTS rev_",
    "CREATE TRIGGER rev_",
    "DROP TABLE docs",
    "DROP TABLE agents",
    "DROP TABLE agent_memberships",
    "DROP TABLE agent_connections",
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !matches!(name.as_str(), "target" | "tests" | ".git" | "node_modules") {
                rust_files(&path, out);
            }
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

/// The file without its test-only code (same rule as the accounts guard).
fn production_part(path: &Path, text: &str) -> Option<String> {
    let name = path.file_name()?.to_string_lossy();
    if name == "tests.rs" || name.ends_with("_tests.rs") || name == "fixtures.rs" {
        return None;
    }
    let mut kept = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("#[cfg(test)]") {
        kept.push_str(&rest[..start]);
        let item = &rest[start + "#[cfg(test)]".len()..];
        rest = match (item.find(';'), item.find('{')) {
            (Some(semi), Some(open)) if semi < open => &item[semi + 1..],
            (Some(semi), None) => &item[semi + 1..],
            (_, Some(open)) => {
                let mut depth = 0usize;
                let mut end = item.len();
                for (offset, byte) in item[open..].bytes().enumerate() {
                    match byte {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                end = open + offset + 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                &item[end..]
            }
            (None, None) => "",
        };
    }
    kept.push_str(rest);
    Some(kept)
}

/// Offenders among `files`: production code outside `own` containing a forbidden statement
/// (case-insensitive, whitespace collapsed, `IF EXISTS` variants included).
fn offenders(files: &[PathBuf], own: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for path in files {
        if path.canonicalize().ok().as_deref() == Some(own) {
            continue;
        }
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let Some(code) = production_part(path, &text) else {
            continue;
        };
        let code = code
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_uppercase()
            .replace("DROP TABLE IF EXISTS ", "DROP TABLE ")
            .replace("INSERT OR IGNORE INTO ", "INTO ")
            .replace("INSERT OR REPLACE INTO ", "INTO ");
        if FORBIDDEN
            .iter()
            .any(|statement| code.contains(&statement.to_uppercase()))
        {
            found.push(path.display().to_string());
        }
    }
    found
}

#[test]
fn revision_tables_have_one_writer() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../../..");
    let own = manifest.join("src/revisions.rs").canonicalize().unwrap();
    let mut files = Vec::new();
    for dir in ["src", "crates", "abilities"] {
        rust_files(&root.join(dir), &mut files);
    }
    assert!(files.len() > 100, "workspace sources not found");
    let found = offenders(&files, &own);
    assert!(
        found.is_empty(),
        "revision tables or their triggers written outside revisions.rs: {found:?}"
    );
}

#[test]
fn the_guard_catches_a_write_that_skips_the_triggers() {
    let root = tempfile::tempdir().unwrap();
    let own = root.path().join("revisions.rs");
    std::fs::write(&own, "fn f() { \"UPDATE agent_epoch SET epoch=0\" }").unwrap();
    let cases = [
        "fn f() { sqlx::query(\"update agent_epoch set epoch=0\") }",
        "fn f() { sqlx::query(\"DROP TRIGGER IF EXISTS rev_agents_update\") }",
        "fn f() { sqlx::query(\"INSERT OR REPLACE INTO session_revisions VALUES(?,0,0,0)\") }",
        "fn f() { sqlx::raw_sql(\"DROP TABLE IF EXISTS agents\") }",
    ];
    for (index, case) in cases.iter().enumerate() {
        let path = root.path().join(format!("stray{index}.rs"));
        std::fs::write(&path, case).unwrap();
        let found = offenders(&[path.clone(), own.clone()], &own.canonicalize().unwrap());
        assert_eq!(found.len(), 1, "{case}");
    }
    // Test-only code is ignored.
    let test_only = root.path().join("helper.rs");
    std::fs::write(
        &test_only,
        "#[cfg(test)]\nmod tests { fn f() { \"UPDATE agent_epoch SET epoch=0\" } }\n",
    )
    .unwrap();
    assert!(offenders(&[test_only], &own.canonicalize().unwrap()).is_empty());
}
