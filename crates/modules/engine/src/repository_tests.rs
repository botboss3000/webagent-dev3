//! Production Engine SQL with encrypted SQLite and deliberately narrow same-transaction effect fixtures.
//! Owner fixtures prove transaction routing; existing root tests remain authority/codec/history integration evidence.
use crate::{ports::*, repository::Repository};
use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    Sqlite, SqliteConnection, SqlitePool, Transaction,
};
use std::{
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use webagent_access::storage::RecordCodec;
struct Fixture {
    root: tempfile::TempDir,
    pool: SqlitePool,
    app: SqlitePool,
    cipher: Aes256Gcm,
    nonce: AtomicU64,
    gate: tokio::sync::RwLock<()>,
    invalidations: AtomicU64,
    reconciliations: AtomicU64,
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        async fn open_pool(path: impl AsRef<Path>) -> SqlitePool {
            SqlitePoolOptions::new()
                .max_connections(4)
                .connect_with(
                    SqliteConnectOptions::new()
                        .filename(path)
                        .create_if_missing(true)
                        .journal_mode(SqliteJournalMode::Wal)
                        .busy_timeout(Duration::from_secs(2)),
                )
                .await
                .unwrap()
        }
        let pool = open_pool(root.path().join("user.db")).await;
        let app = open_pool(root.path().join("app.db")).await;
        sqlx::raw_sql("CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB NOT NULL);
   CREATE TABLE run_index(id TEXT PRIMARY KEY,session_id TEXT,status TEXT,updated_at INTEGER,enqueue_seq INTEGER,lease_epoch INTEGER NOT NULL DEFAULT 0,lease_owner TEXT,lease_expires_at INTEGER NOT NULL DEFAULT 0);
   CREATE TABLE events(seq INTEGER PRIMARY KEY AUTOINCREMENT,session_id TEXT,kind TEXT,payload BLOB);
   CREATE TABLE interactions(session_id TEXT,run_id TEXT,step TEXT,value BLOB,PRIMARY KEY(session_id,run_id,step));
   CREATE TABLE loop_events(seq INTEGER PRIMARY KEY AUTOINCREMENT,run_id TEXT,schema_version INTEGER,kind TEXT,payload BLOB,created_at INTEGER);
   CREATE TABLE publication_outbox(run_id TEXT PRIMARY KEY,schema_version INTEGER,payload BLOB,created_at INTEGER);
   CREATE TABLE notifications(event_key TEXT PRIMARY KEY,payload BLOB);
   CREATE TABLE fixture_effects(key TEXT PRIMARY KEY,value BLOB);").execute(&pool).await.unwrap();
        Self {
            root,
            pool,
            app,
            cipher: Aes256Gcm::new_from_slice(&[19; 32]).unwrap(),
            nonce: AtomicU64::new(1),
            gate: Default::default(),
            invalidations: AtomicU64::new(0),
            reconciliations: AtomicU64::new(0),
        }
    }
    fn repo(&self) -> Repository<'_, Self, Self, Self, Self, Self> {
        Repository::new(
            self.root.path(),
            &self.pool,
            &self.app,
            "shared_default",
            self,
            self,
            self,
            self,
            self,
        )
    }
    async fn document(&self, key: &str, value: &Value) {
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(key)
            .bind(self.seal(value).unwrap())
            .execute(&self.pool)
            .await
            .unwrap();
    }
    async fn initial(&self) -> Value {
        self.document(
            "session:s",
            &json!({"id":"s","agent_id":"shared_default","status":"active"}),
        )
        .await;
        let mut run = json!({"id":"r","session_id":"s","agent_id":"shared_default","requester_user_id":"admin","status":"queued","generation":"seed","lease_epoch":0,"run_revision":0,"input":[],"updated_at":1,"turns":0});
        self.repo()
            .save_run(&mut run, "run_status", &json!({}))
            .await
            .unwrap();
        run
    }
    async fn docs(&self) -> Vec<(String, Vec<u8>)> {
        sqlx::query_as("SELECT key,value FROM docs ORDER BY key")
            .fetch_all(&self.pool)
            .await
            .unwrap()
    }
    async fn count(&self, table: &str) -> i64 {
        sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
    async fn effect(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        key: &str,
        value: &Value,
    ) -> Result<()> {
        sqlx::query("INSERT INTO fixture_effects(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").bind(key).bind(self.seal(value)?).execute(&mut **tx).await?;
        Ok(())
    }
}
impl RecordCodec for Fixture {
    fn seal(&self, value: &Value) -> Result<Vec<u8>> {
        let mut nonce = [0; 12];
        nonce[4..].copy_from_slice(&self.nonce.fetch_add(1, Ordering::Relaxed).to_be_bytes());
        let mut bytes = nonce.to_vec();
        bytes.extend(
            self.cipher
                .encrypt(
                    Nonce::from_slice(&nonce),
                    serde_json::to_vec(value)?.as_ref(),
                )
                .map_err(|_| anyhow!("Encryption failed"))?,
        );
        Ok(bytes)
    }
    fn unseal(&self, bytes: &[u8]) -> Result<Value> {
        if bytes.len() < 28 {
            return Err(anyhow!("Invalid encrypted record"));
        }
        let raw = self
            .cipher
            .decrypt(Nonce::from_slice(&bytes[..12]), &bytes[12..])
            .map_err(|_| anyhow!("Storage authentication failed"))?;
        Ok(serde_json::from_slice(&raw)?)
    }
    fn encode_application(&self, value: &Value) -> Result<Vec<u8>> {
        self.seal(value)
    }
}
impl Records for Fixture {
    fn current_user_id(&self) -> &str {
        "admin"
    }
    fn encode_user_record(&self, v: &Value) -> Result<Vec<u8>> {
        self.seal(v)
    }
    fn context_gate(&self) -> &tokio::sync::RwLock<()> {
        &self.gate
    }
    fn invalidate(&self, _: &[String]) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
    }
    fn invalidate_context(&self, _: &str) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
    }
    fn reasoning_hashes(&self, _: &[Value]) -> Result<Vec<String>> {
        Ok(vec![])
    }
    async fn decode_document<'a>(
        &'a self,
        _: &'a mut SqliteConnection,
        raw: &'a [u8],
    ) -> Result<Value> {
        self.unseal(raw)
    }
    async fn write_document<'a, 'c>(
        &'a self,
        tx: &'a mut Transaction<'c, Sqlite>,
        key: &'a str,
        v: &'a Value,
    ) -> Result<()>
    where
        'c: 'a,
    {
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").bind(key).bind(self.seal(v)?).execute(&mut **tx).await?;
        Ok(())
    }
}
impl ChatEffects for Fixture {
    async fn require_active_session<'a, 'c>(
        &'a self,
        tx: &'a mut Transaction<'c, Sqlite>,
        sid: &'a str,
    ) -> Result<()>
    where
        'c: 'a,
    {
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(format!("session:{sid}"))
            .fetch_one(&mut **tx)
            .await?;
        if self.unseal(&raw)?["status"] != "active" {
            return Err(anyhow!("Fixture session inactive"));
        }
        Ok(())
    }
    async fn require_participation<'a, 'c>(
        &'a self,
        _: &'a mut Transaction<'c, Sqlite>,
        _: &'a Value,
    ) -> Result<()>
    where
        'c: 'a,
    {
        Ok(())
    }
    async fn insert_interaction<'a, 'c>(
        &'a self,
        tx: &'a mut Transaction<'c, Sqlite>,
        sid: &'a str,
        rid: &'a str,
        step: &'a str,
        v: &'a Value,
    ) -> Result<()>
    where
        'c: 'a,
    {
        sqlx::query("INSERT INTO interactions(session_id,run_id,step,value) VALUES(?,?,?,?)")
            .bind(sid)
            .bind(rid)
            .bind(step)
            .bind(self.seal(v)?)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }
    async fn attach_execution_metadata<'a, 'c>(
        &'a self,
        tx: &'a mut Transaction<'c, Sqlite>,
        _: &'a str,
        _: &'a str,
        _: &'a str,
        v: &'a Value,
    ) -> Result<()>
    where
        'c: 'a,
    {
        self.effect(tx, "execution_metadata", v).await
    }
    async fn insert_working_message<'a, 'c>(
        &'a self,
        tx: &'a mut Transaction<'c, Sqlite>,
        _: &'a str,
        _: &'a str,
        _: &'a str,
        text: &'a str,
    ) -> Result<()>
    where
        'c: 'a,
    {
        self.effect(tx, "working", &json!(text)).await
    }
    async fn insert_chat<'a, 'c>(
        &'a self,
        tx: &'a mut Transaction<'c, Sqlite>,
        _: &'a str,
        _: &'a str,
        _: &'a str,
        v: &'a Value,
    ) -> Result<()>
    where
        'c: 'a,
    {
        self.effect(tx, "final_chat", v).await
    }
    async fn queue_session_naming<'a, 'c>(
        &'a self,
        tx: &'a mut Transaction<'c, Sqlite>,
        _: &'a str,
        v: &'a Value,
    ) -> Result<()>
    where
        'c: 'a,
    {
        self.effect(tx, "naming", v).await
    }
    async fn followup_publication_guard<'a, 'c>(
        &'a self,
        _: &'a mut Transaction<'c, Sqlite>,
        _: &'a str,
    ) -> Result<()>
    where
        'c: 'a,
    {
        Ok(())
    }
}

impl AdmissionEffects for Fixture {
    async fn requester_claim_ready<'a>(&'a self, _: &'a Value) -> Result<bool> {
        Ok(true)
    }
    async fn reconcile_requester_slots<'a>(&'a self) -> Result<()> {
        self.reconciliations.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn reserve_requester_run<'a>(&'a self, _: &'a Value) -> Result<()> {
        Ok(())
    }
}
impl NotificationEffects for Fixture {
    async fn stage_run_notification<'a, 'c>(
        &'a self,
        tx: &'a mut Transaction<'c, Sqlite>,
        v: &'a Value,
    ) -> Result<()>
    where
        'c: 'a,
    {
        let key = format!(
            "checkpoint:{}:{}",
            v["id"].as_str().unwrap(),
            v["run_revision"]
        );
        self.stage_notification(tx, &key, "checkpoint", "s", Some("r"), None)
            .await
    }
    async fn stage_notification<'a, 'c>(
        &'a self,
        tx: &'a mut Transaction<'c, Sqlite>,
        key: &'a str,
        kind: &'a str,
        sid: &'a str,
        rid: Option<&'a str>,
        status: Option<&'a str>,
    ) -> Result<()>
    where
        'c: 'a,
    {
        sqlx::query("INSERT INTO notifications(event_key,payload) VALUES(?,?)")
            .bind(key)
            .bind(self.seal(&json!({"kind":kind,"sid":sid,"rid":rid,"status":status}))?)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }
}
impl UsageEffects for Fixture {
    async fn stage_run_usage_in<'a, 'c>(
        &'a self,
        tx: &'a mut Transaction<'c, Sqlite>,
        v: &'a Value,
    ) -> Result<Value>
    where
        'c: 'a,
    {
        let value = json!({"input_tokens":v["turns"].as_u64().unwrap_or(0)});
        self.write_document(tx, "session_usage:s", &value).await?;
        Ok(value)
    }
}

#[tokio::test]
async fn competing_claim_and_checkpoint_commit_only_one_fenced_transition() {
    let f = Fixture::new().await;
    let seed = f.initial().await;
    let mut one = seed.clone();
    let mut two = seed.clone();
    let r = f.repo();
    let (a, b) = tokio::join!(
        r.claim_run(&mut one, "worker-a", "generation-a"),
        r.claim_run(&mut two, "worker-b", "generation-b")
    );
    assert_eq!(
        usize::from(matches!(a, Ok(true))) + usize::from(matches!(b, Ok(true))),
        1
    );
    let winner = if matches!(a, Ok(true)) {
        assert_eq!(two, seed);
        one
    } else {
        assert_eq!(one, seed);
        two
    };
    let saved = f.repo().read_run("r").await.unwrap().unwrap();
    assert_eq!(saved, winner);
    assert_eq!(saved["lease_epoch"], 1);
    assert_eq!(saved["run_revision"], 2);
    assert_eq!(f.count("loop_events").await, 2);
    assert_eq!(f.count("notifications").await, 2);
    let mut left = winner.clone();
    let mut right = winner.clone();
    left["turns"] = json!(1);
    right["turns"] = json!(2);
    let r = f.repo();
    let payload = json!({});
    let (a, b) = tokio::join!(
        r.save_run(&mut left, "run_status", &payload),
        r.save_run(&mut right, "run_status", &payload)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let saved = f.repo().read_run("r").await.unwrap().unwrap();
    assert_eq!(saved["run_revision"], 3);
    assert!(saved["turns"] == 1 || saved["turns"] == 2);
    assert_eq!(f.count("loop_events").await, 3);
    assert_eq!(f.count("notifications").await, 3);
    let index: (i64, String, i64) = sqlx::query_as(
        "SELECT lease_epoch,lease_owner,lease_expires_at FROM run_index WHERE id='r'",
    )
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(index.0, 1);
    assert_eq!(Some(index.1.as_str()), saved["lease_owner"].as_str());
    assert!(index.2 > 0);
}

#[tokio::test]
async fn late_loop_event_abort_rolls_back_staged_ledger_history_notification_and_publication() {
    let f = Fixture::new().await;
    let mut run = f.initial().await;
    let task = json!({"id":"delivery","user_id":"admin","session_id":"s","status":"completed","revision":0,"result":"synthetic retained evidence"});
    assert!(f.repo().background_cas(&task, None).await.unwrap());
    let before = f.docs().await;
    let invalidations = f.invalidations.load(Ordering::SeqCst);
    let before_events = f.count("loop_events").await;
    let before_notifications = f.count("notifications").await;
    let before_effects = f.count("fixture_effects").await;
    run["background_delivered"] = json!(["delivery"]);
    run["status"] = json!("completed");
    run["turns"] = json!(1);
    run["input"] = json!([{"type":"function_call_output","call_id":"call","output":"synthetic tool evidence"}]);
    run["_tool_execution_metadata"] = json!({"call":{"source":"fixture"}});
    run["_context_assets_write"] = json!({"scope":crate::context_ledger::scope_key("shared_default","admin"),"expected_revision":0,"state":{"revision":1}});
    let staged = run.clone();
    let output =
        json!({"output":[{"type":"message","content":[{"text":"Done"}]}],"terminal_text":"Done"});
    sqlx::raw_sql("CREATE TRIGGER reject_late_loop_event BEFORE INSERT ON loop_events WHEN NEW.kind='model_output' BEGIN SELECT RAISE(ABORT,'fixture late loop event'); END;").execute(&f.pool).await.unwrap();
    let error = f
        .repo()
        .save_run(&mut run, "model_output", &output)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("fixture late loop event"));
    assert_eq!(f.count("background_task_delivery").await, 0);
    let task = f.repo().background_get("delivery").await.unwrap().unwrap();
    assert!(task["delivery_run_id"].is_null());
    assert_eq!(task["revision"], 0);
    assert_eq!(run, staged);
    assert_eq!(f.docs().await, before);
    assert_eq!(f.count("interactions").await, 0);
    assert_eq!(f.count("publication_outbox").await, 0);
    assert_eq!(f.count("loop_events").await, before_events);
    assert_eq!(f.count("notifications").await, before_notifications);
    assert_eq!(f.count("fixture_effects").await, before_effects);
    assert_eq!(f.invalidations.load(Ordering::SeqCst), invalidations);
    assert_eq!(f.reconciliations.load(Ordering::SeqCst), 0);
    sqlx::raw_sql("DROP TRIGGER reject_late_loop_event")
        .execute(&f.pool)
        .await
        .unwrap();
    f.repo()
        .save_run(&mut run, "model_output", &output)
        .await
        .unwrap();
    assert_eq!(run["run_revision"], 2);
    assert!(run.get("_context_assets_write").is_none());
    assert_eq!(run["context_assets_revision"], 1);
    assert_eq!(f.count("interactions").await, 2);
    assert_eq!(f.count("publication_outbox").await, 1);
    assert_eq!(f.count("loop_events").await, before_events + 1);
    assert_eq!(f.count("notifications").await, before_notifications + 1);
    assert_eq!(f.count("background_task_delivery").await, 1);
    let task = f.repo().background_get("delivery").await.unwrap().unwrap();
    assert_eq!(task["delivery_run_id"], "r");
    assert_eq!(task["revision"], 1);
    assert!(f
        .repo()
        .background_scan(Some("s"))
        .await
        .unwrap()
        .is_empty());
    let mut other = run.clone();
    other["id"] = json!("foreign-delivery");
    let mut tx = f.pool.begin().await.unwrap();
    assert!(f
        .repo()
        .background_claim_delivery(&mut tx, &other)
        .await
        .is_err());
    tx.rollback().await.unwrap();
    let saved = f.repo().read_run("r").await.unwrap().unwrap();
    assert_eq!(saved, run);
    let rows = f.docs().await;
    let outbox = f.count("publication_outbox").await;
    f.repo()
        .save_run(&mut run, "model_output", &output)
        .await
        .unwrap();
    assert_eq!(f.docs().await, rows);
    assert_eq!(f.count("publication_outbox").await, outbox);
    assert_eq!(f.reconciliations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn command_acknowledgement_waits_for_a_concurrent_writer_instead_of_failing_locked() {
    let f = Fixture::new().await;
    sqlx::raw_sql("CREATE TABLE loop_commands(seq INTEGER PRIMARY KEY AUTOINCREMENT,command_id TEXT NOT NULL UNIQUE,run_id TEXT NOT NULL,schema_version INTEGER NOT NULL,kind TEXT NOT NULL,payload BLOB NOT NULL,created_at INTEGER NOT NULL,acknowledged_at INTEGER)").execute(&f.pool).await.unwrap();
    assert_eq!(f.repo().acknowledge_loop_commands().await.unwrap(), 0);
    let insert="INSERT INTO loop_commands(command_id,run_id,schema_version,kind,payload,created_at) VALUES(?,'r',1,'run_status',?,0)";
    let payload = f
        .seal(&json!({"schema_version":1,"run_id":"r","kind":"run_status"}))
        .unwrap();
    sqlx::query(insert)
        .bind("first")
        .bind(&payload)
        .execute(&f.pool)
        .await
        .unwrap();
    // A writer (the API admitting a cancel) holds the write lock while the acknowledgement starts.
    let mut writer = f.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    sqlx::query(insert)
        .bind("second")
        .bind(&payload)
        .execute(&mut *writer)
        .await
        .unwrap();
    let commit = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        writer.commit().await.unwrap();
    };
    let r = f.repo();
    let (acknowledged, ()) = tokio::join!(r.acknowledge_loop_commands(), commit);
    assert_eq!(acknowledged.unwrap(), 2);
    assert_eq!(f.repo().acknowledge_loop_commands().await.unwrap(), 0);
}
