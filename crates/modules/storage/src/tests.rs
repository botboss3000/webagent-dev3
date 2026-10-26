//! Real-file and real-SQLite tests for the storage primitives.
use crate::{
    codec::{self, AgentCipher, Codec},
    docs, migration, pool, recovery,
    routing::{self, Plane, Planes},
    settings, ScopedCodec,
};
use anyhow::Result;
use serde_json::{json, Value};
use std::{os::unix::fs::PermissionsExt, path::Path};

/// A bound handle: installation codec, account identity and optional agent key.
struct Handle {
    codec: Codec,
    root: std::path::PathBuf,
    user: &'static str,
    agent: Option<(String, AgentCipher)>,
}
impl ScopedCodec for Handle {
    fn seal(&self, value: &Value) -> Result<Vec<u8>> {
        self.codec.seal(value)
    }
    fn unseal(&self, data: &[u8]) -> Result<Value> {
        if codec::is_agent_envelope(data) {
            let (id, cipher) = self
                .agent
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Agent key unavailable"))?;
            return cipher.unseal(id, data);
        }
        self.codec.unseal(data)
    }
    fn encode_user_record(&self, value: &Value) -> Result<Vec<u8>> {
        self.codec.encode_user_record(&self.root, self.user, value)
    }
}

fn handle(root: &Path, user: &'static str) -> Handle {
    Handle {
        codec: Codec::open(root).unwrap(),
        root: root.to_owned(),
        user,
        agent: None,
    }
}

fn write_settings(root: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(root.join("config/app-settings.json"), bytes).unwrap();
}

#[test]
fn installation_key_is_private_stable_and_never_replaced_for_existing_data() {
    let root = tempfile::tempdir().unwrap();
    let key = codec::load_or_create_key(root.path()).unwrap();
    assert_eq!(key.len(), 32);
    let mode = std::fs::metadata(root.path().join("encryption.key"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(codec::load_or_create_key(root.path()).unwrap(), key);
    std::fs::remove_file(root.path().join("encryption.key")).unwrap();
    std::fs::create_dir_all(root.path().join("db")).unwrap();
    std::fs::write(root.path().join("db/app.db"), b"").unwrap();
    assert!(codec::load_or_create_key(root.path()).is_err());
    assert!(!root.path().join("encryption.key").exists());
    std::fs::write(root.path().join("encryption.key"), [0u8; 16]).unwrap();
    assert!(codec::load_or_create_key(root.path()).is_err());
}

#[test]
fn sealed_records_round_trip_and_reject_tampering() {
    let root = tempfile::tempdir().unwrap();
    let codec = Codec::open(root.path()).unwrap();
    let value = json!({"note":"storage-fixture","n":[1,2,3]});
    let mut sealed = codec.seal(&value).unwrap();
    assert!(!String::from_utf8_lossy(&sealed).contains("storage-fixture"));
    assert_eq!(codec.unseal(&sealed).unwrap(), value);
    let last = sealed.len() - 1;
    sealed[last] ^= 1;
    assert!(codec.unseal(&sealed).is_err());
    assert!(codec.unseal(&[1, 2, 3]).is_err());
    let other = Codec::new(&[7u8; 32]).unwrap();
    assert!(other.unseal(&codec.seal(&value).unwrap()).is_err());
}

#[test]
fn administrator_plaintext_policy_follows_app_settings() {
    let root = tempfile::tempdir().unwrap();
    let admin = handle(root.path(), "admin");
    let user = handle(root.path(), "someone");
    let value = json!({"text":"plaintext-policy-fixture"});
    // Missing settings keep encryption on.
    assert!(
        !String::from_utf8_lossy(&admin.encode_user_record(&value).unwrap())
            .contains("plaintext-policy-fixture")
    );
    write_settings(
        root.path(),
        br#"{"admin_database_encryption":{"enabled":false}}"#,
    );
    let plain = admin.encode_user_record(&value).unwrap();
    let envelope: Value = serde_json::from_slice(&plain).unwrap();
    assert_eq!(envelope["format"], "v3-admin-plaintext-v1");
    assert_eq!(envelope["value"], value);
    assert_eq!(admin.unseal(&plain).unwrap(), value);
    // Other accounts and master sealing stay encrypted under the same policy.
    assert!(
        !String::from_utf8_lossy(&user.encode_user_record(&value).unwrap())
            .contains("plaintext-policy-fixture")
    );
    assert!(
        !String::from_utf8_lossy(&admin.seal(&value).unwrap()).contains("plaintext-policy-fixture")
    );
    // The authentication tag binds the visible value.
    let mut forged = envelope.clone();
    forged["value"] = json!({"text":"changed"});
    assert!(admin.unseal(&serde_json::to_vec(&forged).unwrap()).is_err());
    assert!(Codec::new(&[9u8; 32]).unwrap().unseal(&plain).is_err());
    write_settings(
        root.path(),
        br#"{"admin_database_encryption":{"enabled":true}}"#,
    );
    let sealed = admin.encode_user_record(&value).unwrap();
    assert!(serde_json::from_slice::<Value>(&sealed).is_err());
    assert_eq!(
        admin.unseal(&plain).unwrap(),
        value,
        "existing plaintext records stay readable"
    );
    write_settings(
        root.path(),
        br#"{"admin_database_encryption":{"enabled":"false"}}"#,
    );
    assert!(admin.encode_user_record(&value).is_err());
    assert!(Codec::open(root.path()).is_err());
    write_settings(root.path(), b"[]");
    assert!(settings::read_app_settings(root.path()).is_err());
}

#[test]
fn agent_envelopes_bind_key_and_identity() {
    let first = AgentCipher::new(&[1u8; 32]).unwrap();
    let second = AgentCipher::new(&[2u8; 32]).unwrap();
    let value = json!({"setting":"agent-fixture"});
    let sealed = first.seal("alpha", &value).unwrap();
    assert!(codec::is_agent_envelope(&sealed));
    assert_eq!(first.unseal("alpha", &sealed).unwrap(), value);
    assert!(first.unseal("beta", &sealed).is_err());
    assert!(second.unseal("alpha", &sealed).is_err());
    assert!(first.unseal("alpha", b"WAAGENT1short").is_err());
}

async fn agent_pool(root: &Path, id: &str) -> sqlx::SqlitePool {
    let pool = pool::open_pool(
        &root.join("agent_data").join(id).join(format!("{id}.db")),
        false,
        false,
    )
    .await
    .unwrap();
    sqlx::raw_sql("CREATE TABLE IF NOT EXISTS agents(id TEXT PRIMARY KEY,metadata BLOB NOT NULL,revision INTEGER NOT NULL)").execute(&pool).await.unwrap();
    pool
}

#[tokio::test]
async fn agent_key_is_created_once_converts_records_and_fails_closed() {
    let root = tempfile::tempdir().unwrap();
    let codec = Codec::open(root.path()).unwrap();
    let pool = agent_pool(root.path(), "alpha").await;
    let metadata = json!({"owner_id":"admin"});
    sqlx::query("INSERT INTO agents VALUES('alpha',?,1)")
        .bind(codec.seal(&metadata).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO docs VALUES('agent:alpha:setting',?)")
        .bind(codec.seal(&json!(1)).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let cipher = codec::initialize_agent_key(&pool, &codec, "alpha")
        .await
        .unwrap();
    let stored: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(cipher.unseal("alpha", &stored).unwrap(), metadata);
    let doc: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key='agent:alpha:setting'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(cipher.unseal("alpha", &doc).unwrap(), json!(1));
    // Reopen loads the same key and leaves converted rows unchanged.
    let again = codec::initialize_agent_key(&pool, &codec, "alpha")
        .await
        .unwrap();
    assert_eq!(again.unseal("alpha", &stored).unwrap(), metadata);
    let after: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(after, stored);
    // Another agent's database gets its own key.
    let beta_pool = agent_pool(root.path(), "beta").await;
    sqlx::query("INSERT INTO agents VALUES('beta',?,1)")
        .bind(codec.seal(&metadata).unwrap())
        .execute(&beta_pool)
        .await
        .unwrap();
    let beta = codec::initialize_agent_key(&beta_pool, &codec, "beta")
        .await
        .unwrap();
    assert!(beta.unseal("alpha", &stored).is_err());
    // A key envelope naming another agent is rejected.
    assert!(codec::initialize_agent_key(&beta_pool, &codec, "alpha")
        .await
        .is_err());
    // A converted database without its envelope never gets a replacement key.
    sqlx::query("DELETE FROM agent_key_envelopes")
        .execute(&pool)
        .await
        .unwrap();
    assert!(codec::initialize_agent_key(&pool, &codec, "alpha")
        .await
        .is_err());
    let unchanged: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(unchanged, stored);
}

#[tokio::test]
async fn pools_create_plane_schema_with_profile_permissions() {
    let root = tempfile::tempdir().unwrap();
    let admin = pool::open_pool(&root.path().join("user_data/admin/admin.db"), true, true)
        .await
        .unwrap();
    let private = pool::open_pool(&root.path().join("user_data/u1/u1.db"), true, true)
        .await
        .unwrap();
    let app = pool::open_pool(&root.path().join("db/app.db"), true, false)
        .await
        .unwrap();
    let mode = |p: &str| {
        std::fs::metadata(root.path().join(p))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode("user_data/admin"), 0o770);
    assert_eq!(mode("user_data/admin/admin.db"), 0o660);
    assert_eq!(mode("user_data/u1"), 0o700);
    assert_eq!(mode("user_data/u1/u1.db"), 0o600);
    let tables = |pool: sqlx::SqlitePool| async move {
        sqlx::query_scalar::<_, String>("SELECT name FROM sqlite_master WHERE type='table'")
            .fetch_all(&pool)
            .await
            .unwrap()
    };
    let user_tables = tables(private.clone()).await;
    for table in [
        "docs",
        "events",
        "run_index",
        "interactions",
        "chat",
        "loop_commands",
        "recovery_input",
        "action_index",
    ] {
        assert!(user_tables.iter().any(|t| t == table), "{table}");
    }
    assert!(!user_tables.iter().any(|t| t == "loop_heartbeat"));
    let app_tables = tables(app.clone()).await;
    assert!(
        app_tables.iter().any(|t| t == "loop_heartbeat")
            && !app_tables.iter().any(|t| t == "run_index")
    );
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('run_index')")
            .fetch_all(&admin)
            .await
            .unwrap();
    for column in [
        "enqueue_seq",
        "lease_epoch",
        "lease_owner",
        "lease_expires_at",
    ] {
        assert!(columns.iter().any(|c| c == column));
    }
    // Reopening an existing database is idempotent.
    drop(admin);
    pool::open_pool(&root.path().join("user_data/admin/admin.db"), true, true)
        .await
        .unwrap();
}

#[tokio::test]
async fn user_plane_registry_keeps_active_and_admin_handles() {
    let root = tempfile::tempdir().unwrap();
    let pool = pool::open_pool(&root.path().join("db/app.db"), false, false)
        .await
        .unwrap();
    let mut states = std::collections::HashMap::new();
    for i in 0..64 {
        states.insert(format!("u{i}"), pool::UserPlane::new(pool.clone()));
    }
    states.insert("admin".into(), pool::UserPlane::new(pool.clone()));
    let active = states["u7"].clone();
    pool::retain_active_users(&mut states);
    assert_eq!(states.len(), 2);
    assert!(states.contains_key("admin") && states.contains_key("u7"));
    drop(active);
    let mut few = std::collections::HashMap::from([("u1".to_string(), pool::UserPlane::new(pool))]);
    pool::retain_active_users(&mut few);
    assert_eq!(few.len(), 1, "below the bound nothing is evicted");
}

#[test]
fn namespaces_route_to_their_authority_and_views_are_agent_bound() {
    for (key, plane) in [
        ("agent:alpha:model", Plane::Agent),
        ("agent_abilities:alpha", Plane::Agent),
        ("session:s1", Plane::User),
        ("run:r1", Plane::User),
        ("attachment_quota", Plane::User),
        ("core_vault:x", Plane::User),
        ("followup_queue:s1", Plane::User),
        ("model_selection:catalog:v1", Plane::Application),
        ("storage:split_migrated", Plane::Application),
        ("agent_directory", Plane::Application),
    ] {
        assert_eq!(routing::plane_for(key), plane, "{key}");
    }
    assert!(routing::validate_agent_doc_key("alpha", "agent:alpha").is_ok());
    assert!(routing::validate_agent_doc_key("alpha", "agent:alpha:model").is_ok());
    assert!(routing::validate_agent_doc_key("alpha", "agent:alphabet:model").is_err());
    assert!(routing::validate_agent_doc_key("alpha", "agent:beta:model").is_err());
    assert!(routing::validate_agent_doc_key("alpha", "agent_abilities:beta").is_err());
    assert!(routing::validate_agent_doc_key("alpha", "session:s1").is_ok());
    assert!(
        routing::always_sealed("module_state:x")
            && routing::always_sealed("session_context_assets:s")
            && !routing::always_sealed("session:s")
    );
}

#[tokio::test]
async fn documents_and_events_use_raw_plane_sql() {
    let root = tempfile::tempdir().unwrap();
    let pool = pool::open_pool(&root.path().join("user_data/admin/admin.db"), true, true)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    docs::upsert(&mut tx, "memory:a", b"one").await.unwrap();
    docs::upsert(&mut tx, "memory:a", b"two").await.unwrap();
    docs::upsert(&mut tx, "memory:b", b"three").await.unwrap();
    docs::upsert(&mut tx, "other:c", b"four").await.unwrap();
    assert_eq!(
        docs::read(&mut tx, "memory:a").await.unwrap(),
        Some(b"two".to_vec())
    );
    assert_eq!(
        docs::scan(&mut tx, "memory:").await.unwrap(),
        vec![b"two".to_vec(), b"three".to_vec()]
    );
    sqlx::query("INSERT INTO recovery_input VALUES('r1',0,x'00')")
        .execute(&mut *tx)
        .await
        .unwrap();
    docs::upsert(&mut tx, "run:r1", b"run").await.unwrap();
    docs::remove(&mut tx, "run:r1").await.unwrap();
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM recovery_input")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    assert!(docs::read(&mut tx, "run:r1").await.unwrap().is_none());
    tx.rollback().await.unwrap();
    let mut connection = pool.acquire().await.unwrap();
    assert!(
        docs::read(&mut connection, "memory:a")
            .await
            .unwrap()
            .is_none(),
        "uncommitted writes roll back"
    );
    drop(connection);
    for i in 0..205 {
        docs::append_event(&pool, "s1", "k", format!("{i}").as_bytes())
            .await
            .unwrap();
    }
    docs::append_event(&pool, "s2", "k", b"x").await.unwrap();
    let page = docs::session_events(&pool, "s1", 0).await.unwrap();
    assert_eq!(page.len(), 200);
    let rest = docs::session_events(&pool, "s1", page.last().unwrap().0)
        .await
        .unwrap();
    assert_eq!(rest.len(), 5);
    assert!(rest.iter().all(|(_, kind, _)| kind == "k"));
}

#[tokio::test]
async fn recovery_input_round_trips_reuses_ciphertext_and_detects_tampering() {
    let root = tempfile::tempdir().unwrap();
    let handle = handle(root.path(), "admin");
    let pool = pool::open_pool(&root.path().join("user_data/admin/admin.db"), true, true)
        .await
        .unwrap();
    let save = |run: Value| {
        let pool = pool.clone();
        let handle = &handle;
        async move {
            let mut tx = pool.begin().await.unwrap();
            let metadata = recovery::save_run_input(&mut tx, handle, "run:r1", &run)
                .await
                .unwrap();
            assert!(metadata.get("input").is_none());
            docs::upsert(&mut tx, "run:r1", &handle.seal(&metadata).unwrap())
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }
    };
    let read = || {
        let pool = pool.clone();
        let handle = &handle;
        async move {
            let mut tx = pool.begin().await.unwrap();
            let raw = docs::read(&mut tx, "run:r1").await.unwrap().unwrap();
            recovery::read_document(&mut tx, handle, &raw).await
        }
    };
    let items = |n: usize| {
        (0..n)
            .map(|i| json!({"role":"user","content":format!("item {i}")}))
            .collect::<Vec<_>>()
    };
    let run = json!({"id":"r1","status":"running","input":items(3)});
    save(run.clone()).await;
    assert_eq!(read().await.unwrap(), run);
    let before: Vec<Vec<u8>> =
        sqlx::query_scalar("SELECT item FROM recovery_input ORDER BY ordinal")
            .fetch_all(&pool)
            .await
            .unwrap();
    let mut appended = run.clone();
    appended["input"] = json!(items(4));
    save(appended.clone()).await;
    let after: Vec<Vec<u8>> =
        sqlx::query_scalar("SELECT item FROM recovery_input ORDER BY ordinal")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        &after[..3],
        &before[..],
        "unchanged items keep their ciphertext"
    );
    assert_eq!(read().await.unwrap(), appended);
    let mut truncated = run.clone();
    truncated["input"] = json!(items(1));
    save(truncated.clone()).await;
    assert_eq!(read().await.unwrap(), truncated);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM recovery_input")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    // A substituted (validly sealed) row no longer matches the sealed manifest.
    sqlx::query("UPDATE recovery_input SET item=? WHERE ordinal=0")
        .bind(handle.seal(&json!("swapped")).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    assert!(read().await.is_err());
    sqlx::query("DELETE FROM recovery_input")
        .execute(&pool)
        .await
        .unwrap();
    assert!(read().await.is_err(), "missing rows are corruption");
    let mut tx = pool.begin().await.unwrap();
    assert!(
        recovery::save_run_input(&mut tx, &handle, "run:other", &run)
            .await
            .is_err()
    );
    assert_eq!(
        recovery::recovery_item_digest(&run).unwrap(),
        recovery::recovery_digest(&serde_json::to_vec(&run).unwrap())
    );
}

#[tokio::test]
async fn legacy_runtime_database_splits_into_planes_once() {
    let root = tempfile::tempdir().unwrap();
    let handle = handle(root.path(), "admin");
    let legacy = pool::open_pool(&root.path().join("runtime.sqlite"), true, true)
        .await
        .unwrap();
    let session = json!({"id":"s1","context":[{"role":"user","content":"hi"}],"messages":[{"role":"user","content":"hi","run_id":"r1","created_at":5}]});
    for (key, value) in [
        ("session:s1", session.clone()),
        ("agent:shared_default:model", json!({"model":"m"})),
        ("auth:x", json!({"a":1})),
    ] {
        sqlx::query("INSERT INTO docs VALUES(?,?)")
            .bind(key)
            .bind(handle.seal(&value).unwrap())
            .execute(&legacy)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO events(seq,session_id,kind,payload) VALUES(7,'s1','message',?)")
        .bind(handle.seal(&json!({"e":1})).unwrap())
        .execute(&legacy)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO run_index(id,session_id,status,updated_at) VALUES('r1','s1','completed',9)",
    )
    .execute(&legacy)
    .await
    .unwrap();
    legacy.close().await;
    let app = pool::open_pool(&root.path().join("db/app.db"), true, false)
        .await
        .unwrap();
    let user = pool::open_pool(&root.path().join("user_data/admin/admin.db"), true, true)
        .await
        .unwrap();
    let agent = pool::open_pool(
        &root
            .path()
            .join("agent_data/shared_default/shared_default.db"),
        false,
        false,
    )
    .await
    .unwrap();
    let planes = Planes {
        application: &app,
        user: &user,
        agent: &agent,
    };
    migration::migrate_legacy_split(root.path(), planes, &handle)
        .await
        .unwrap();
    let get = |pool: &sqlx::SqlitePool, key: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
                .bind(key)
                .fetch_optional(&pool)
                .await
                .unwrap()
        }
    };
    let migrated = handle
        .unseal(&get(&user, "session:s1").await.unwrap())
        .unwrap();
    assert_eq!(migrated["checkpoint_input"], session["context"]);
    assert!(
        get(&agent, "agent:shared_default:model").await.is_some()
            && get(&app, "auth:x").await.is_some()
    );
    assert!(get(&app, "session:s1").await.is_none());
    let chat: Vec<u8> = sqlx::query_scalar("SELECT content FROM chat WHERE id='legacy:s1:0'")
        .fetch_one(&user)
        .await
        .unwrap();
    assert_eq!(handle.unseal(&chat).unwrap(), json!("hi"));
    let seq: i64 = sqlx::query_scalar("SELECT seq FROM events WHERE session_id='s1'")
        .fetch_one(&user)
        .await
        .unwrap();
    assert_eq!(seq, 7);
    let status: String = sqlx::query_scalar("SELECT status FROM run_index WHERE id='r1'")
        .fetch_one(&user)
        .await
        .unwrap();
    assert_eq!(status, "completed");
    assert_eq!(
        handle
            .unseal(&get(&app, "storage:split_migrated").await.unwrap())
            .unwrap(),
        json!({"version":2})
    );
    // The completed marker makes a rerun a no-op, even after a conflicting edit.
    sqlx::query("UPDATE docs SET value=? WHERE key='session:s1'")
        .bind(handle.seal(&json!({"changed":true})).unwrap())
        .execute(&user)
        .await
        .unwrap();
    migration::migrate_legacy_split(root.path(), planes, &handle)
        .await
        .unwrap();
    // Without the marker the conflicting destination is rejected rather than overwritten.
    sqlx::query("DELETE FROM docs WHERE key='storage:split_migrated'")
        .execute(&app)
        .await
        .unwrap();
    assert!(
        migration::migrate_legacy_split(root.path(), planes, &handle)
            .await
            .is_err()
    );
    assert_eq!(
        handle
            .unseal(&get(&user, "session:s1").await.unwrap())
            .unwrap(),
        json!({"changed":true})
    );
}
