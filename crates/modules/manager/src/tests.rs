//! Real SQLite exercises Manager transactions; root integration retains actual Storage/Chat codecs and effects.
use super::*;
use crate::storage::{RecordEffects,Repository};
use anyhow::{anyhow,Result};
use serde_json::{json,Value};
use sqlx::{Sqlite,SqlitePool,Transaction};
use std::{path::{Path,PathBuf},sync::atomic::{AtomicBool,Ordering}};
struct Fixture {root:tempfile::TempDir,pool:SqlitePool,agent:SqlitePool,reconcile_error:AtomicBool}
impl Fixture {
 async fn new()->Self {
  let root=tempfile::tempdir().unwrap();
  async fn open_pool(path:PathBuf)->SqlitePool {sqlx::sqlite::SqlitePoolOptions::new().max_connections(4).connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(path).create_if_missing(true)).await.unwrap()}
  let pool=open_pool(root.path().join("user.db")).await;let agent=open_pool(root.path().join("agent.db")).await;
  sqlx::raw_sql("CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB); CREATE TABLE events(seq INTEGER PRIMARY KEY,session_id TEXT,kind TEXT,payload BLOB); CREATE TABLE owner_admissions(id TEXT PRIMARY KEY); CREATE TABLE loop_commands(command_id TEXT PRIMARY KEY);").execute(&pool).await.unwrap();
  sqlx::query("CREATE TABLE agents(id TEXT PRIMARY KEY,metadata BLOB)").execute(&agent).await.unwrap();
  let f=Self{root,pool,agent,reconcile_error:AtomicBool::new(false)};
  f.put("session:s",&json!({"id":"s","status":"active"})).await;
  let record=json!({"id":"r","session_id":"s","agent_id":"agent","status":"pending_review","generation":"","run":{"id":"r","session_id":"s","agent_id":"agent"},"message":{"author_id":"actor","content":"synthetic"},"usage":{}});
  f.put("manager_review:r",&record).await;f.put("manager_pending:s",&json!({"id":"r"})).await;f
 }
 fn repo(&self)->Repository<'_,Self> {Repository::new(self.root.path(),&self.pool,"owner",self)}
 async fn put(&self,key:&str,value:&Value) {sqlx::query("INSERT INTO docs VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").bind(key).bind(serde_json::to_vec(value).unwrap()).execute(&self.pool).await.unwrap();}
 async fn all_rows(&self)->Vec<(String,Vec<u8>)> {sqlx::query_as("SELECT key,value FROM docs ORDER BY key").fetch_all(&self.pool).await.unwrap()}
}
impl RecordEffects for Fixture {
 fn unseal(&self,b:&[u8])->Result<Value>{Ok(serde_json::from_slice(b)?)}
 fn encode_user_record(&self,v:&Value)->Result<Vec<u8>>{Ok(serde_json::to_vec(v)?)}
 fn resolved_prompts(&self,root:&Path)->Result<Value>{Ok(serde_json::from_slice(&std::fs::read(root.join("config/prompts.json"))?)?)}
 fn prompt_defaults(&self)->Result<Vec<u8>>{Ok(b"{}".to_vec())}
 fn public_usage(&self,v:&Value)->Value{v.clone()}
 fn record_usage(&self,_:&mut Value,_:&Value){}
 fn invalidate_documents(&self,_:&[String]){}
 async fn agent_pool<'a>(&'a self,id:&'a str)->Result<SqlitePool>{if id!="agent"{return Err(anyhow!("Unknown fixture agent"));}Ok(self.agent.clone())}
 async fn list_documents<'a>(&'a self,prefix:&'a str)->Result<Vec<Value>>{let rows:Vec<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE substr(key,1,?)=? ORDER BY key").bind(prefix.len()as i64).bind(prefix).fetch_all(&self.pool).await?;rows.iter().map(|v|self.unseal(v)).collect()}
 async fn read_document<'a>(&'a self,key:&'a str)->Result<Option<Value>>{let v:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(key).fetch_optional(&self.pool).await?;v.as_deref().map(|v|self.unseal(v)).transpose()}
 async fn requester_claim_ready<'a>(&'a self,_:&'a Value)->Result<bool>{Ok(true)}
 async fn participation_allows_run<'a>(&'a self,_:&'a Value)->Result<bool>{Ok(true)}
 async fn reconcile_requester_slots(&self)->Result<()>{if self.reconcile_error.load(Ordering::Acquire){Err(anyhow!("Fixture reconciliation failed"))}else{Ok(())}}
 async fn require_active_session_in<'a,'c>(&'a self,tx:&'a mut Transaction<'c,Sqlite>,sid:&'a str)->Result<()>where 'c:'a{let b:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(format!("session:{sid}")).fetch_one(&mut **tx).await?;if self.unseal(&b)?["status"]!="active"{return Err(anyhow!("Session unavailable"));}Ok(())}
 async fn write_document_in<'a,'c>(&'a self,tx:&'a mut Transaction<'c,Sqlite>,key:&'a str,value:&'a Value)->Result<()>where 'c:'a{sqlx::query("INSERT INTO docs VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").bind(key).bind(self.encode_user_record(value)?).execute(&mut **tx).await?;Ok(())}
 async fn admit_message_in<'a,'c>(&'a self,tx:&'a mut Transaction<'c,Sqlite>,id:&'a str,sid:&'a str,_:&'a Value,_:&'a Value,command_id:&'a str)->Result<()>where 'c:'a{self.require_active_session_in(tx,sid).await?;sqlx::query("INSERT INTO owner_admissions VALUES(?)").bind(id).execute(&mut **tx).await?;sqlx::query("INSERT INTO loop_commands VALUES(?)").bind(command_id).execute(&mut **tx).await?;Ok(())}
}
#[tokio::test]async fn late_event_abort_rolls_back_review_and_owner_effects_then_admits_once(){
 let f=Fixture::new().await;let observed=f.repo().claim_manager_review("r").await.unwrap().unwrap();let before=f.all_rows().await;
 sqlx::raw_sql("CREATE TRIGGER reject_verdict BEFORE INSERT ON events WHEN NEW.kind='manager_verdict' BEGIN SELECT RAISE(ABORT,'fixture late event abort'); END;").execute(&f.pool).await.unwrap();
 assert!(f.repo().finish_manager_review(&observed,"acceptable",Some(Verdict::Acceptable)).await.is_err());assert_eq!(f.all_rows().await,before);
 for table in ["owner_admissions","loop_commands","events"]{let n:i64=sqlx::query_scalar(&format!("SELECT count(*) FROM {table}")).fetch_one(&f.pool).await.unwrap();assert_eq!(n,0);}
 sqlx::query("DROP TRIGGER reject_verdict").execute(&f.pool).await.unwrap();
 let mut stale=observed.clone();stale["generation"]=json!("stale");assert!(!f.repo().finish_manager_review(&stale,"acceptable",Some(Verdict::Acceptable)).await.unwrap());
 assert!(f.repo().finish_manager_review(&observed,"acceptable",Some(Verdict::Acceptable)).await.unwrap());assert!(!f.repo().finish_manager_review(&observed,"acceptable",Some(Verdict::Acceptable)).await.unwrap());
 for table in ["owner_admissions","loop_commands","events"]{let n:i64=sqlx::query_scalar(&format!("SELECT count(*) FROM {table}")).fetch_one(&f.pool).await.unwrap();assert_eq!(n,1);}
}
#[tokio::test]async fn quota_reconciliation_error_follows_durable_nonacceptance_commit(){
 let f=Fixture::new().await;let observed=f.repo().claim_manager_review("r").await.unwrap().unwrap();f.reconcile_error.store(true,Ordering::Release);
 assert!(f.repo().finish_manager_review(&observed,"review_error",None).await.is_err());
 assert_eq!(f.read_document("manager_review:r").await.unwrap().unwrap()["status"],"review_error");assert!(f.read_document("manager_pending:s").await.unwrap().is_none());
 let events:i64=sqlx::query_scalar("SELECT count(*) FROM events").fetch_one(&f.pool).await.unwrap();assert_eq!(events,1);
 assert!(!f.repo().finish_manager_review(&observed,"review_error",None).await.unwrap());
}
#[test]fn cooperating_settings_writes_preserve_siblings_modes_and_lazy_defaults(){
 use std::os::unix::fs::PermissionsExt;
 let root=tempfile::tempdir().unwrap();std::fs::create_dir(root.path().join("config")).unwrap();let path=root.path().join("config/prompts.json");std::fs::write(&path,b"{\"extension\":true}").unwrap();std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o640)).unwrap();
 std::thread::scope(|scope|for id in ["one","two"]{let root=&root;scope.spawn(move||crate::settings::save_prompt(root.path(),id,id,||anyhow::bail!("Defaults must stay lazy")).unwrap());});
 let v:Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();assert_eq!(v["extension"],true);for id in ["one","two"]{assert_eq!(v["manager_incoming_message"][id],id);}assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode()&0o777,0o640);
}

// Append to Manager's existing cfg(test) tests.rs; existing repository tests remain unchanged.
mod independent_runtime_io {
    use anyhow::{anyhow, bail, Result};
    use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
    use axum::{routing::post, Json, Router};
    use serde_json::{json, Value};
    use sqlx::{Sqlite, SqlitePool, Transaction};
    use std::{collections::{BTreeMap, BTreeSet}, future::Future, path::{Path, PathBuf},
        sync::{Arc, Mutex as SyncMutex, atomic::{AtomicBool, AtomicU64, Ordering}}, time::Duration};
    use tokio::sync::{Mutex, Notify, RwLock};
    use tokio_util::sync::CancellationToken;
    use webagent_access::storage::RecordCodec;
    use webagent_models::{model_selection::*, runtime::SessionAuthority,
        storage::{Repository as ModelRepository, RuntimeStorage}, transport::{Projection, Provider}};
    use crate::{runtime::{Engine, ReviewAuthority, Runtime, SessionScope}, storage::RecordEffects};

    const OWNER: &str = "admin";
    const REQUESTER: &str = "alice";
    const AGENT: &str = "manager-agent";
    const DEFAULT_AGENT: &str = "shared_default";
    const SESSION: &str = "fixture-session";
    const REVIEW: &str = "fixture-review";
    const CRITERIA: &str = "Only accept clothing and size questions; private fixture criteria.";
    const MESSAGE: &str = "Do you have medium shirts?";

    #[derive(Clone)]
    struct Storage {
        root: PathBuf, app: SqlitePool, user: SqlitePool, agent: SqlitePool,
        target_agent: SqlitePool, default_agent: SqlitePool, agent_scope: String,
        cipher: Aes256Gcm, nonce: Arc<AtomicU64>, usage: Arc<AtomicU64>,
    }
    impl RecordCodec for Storage {
        fn seal(&self, value: &Value) -> Result<Vec<u8>> {
            let mut nonce = [0; 12];
            nonce[4..].copy_from_slice(&self.nonce.fetch_add(1, Ordering::Relaxed).to_be_bytes());
            let mut bytes = nonce.to_vec();
            bytes.extend(self.cipher.encrypt(Nonce::from_slice(&nonce), serde_json::to_vec(value)?.as_ref())
                .map_err(|_| anyhow!("Fixture encryption failed"))?);
            Ok(bytes)
        }
        fn unseal(&self, bytes: &[u8]) -> Result<Value> {
            if bytes.len() < 28 { bail!("Invalid fixture ciphertext"); }
            let plain = self.cipher.decrypt(Nonce::from_slice(&bytes[..12]), &bytes[12..])
                .map_err(|_| anyhow!("Fixture authentication failed"))?;
            Ok(serde_json::from_slice(&plain)?)
        }
        fn encode_application(&self, value: &Value) -> Result<Vec<u8>> { self.seal(value) }
    }
    impl Storage {
        async fn put_user(&self, key: &str, value: &Value) -> Result<()> {
            sqlx::query("INSERT INTO docs VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
                .bind(key).bind(self.seal(value)?).execute(&self.user).await?;
            Ok(())
        }
        async fn user_document(&self, key: &str) -> Result<Option<Value>> {
            let bytes: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
                .bind(key).fetch_optional(&self.user).await?;
            bytes.as_deref().map(|bytes| RecordCodec::unseal(self, bytes)).transpose()
        }
        fn scoped(&self, id: &str) -> Result<Self> {
            let mut storage = self.clone();
            storage.agent = match id {
                AGENT => self.target_agent.clone(),
                DEFAULT_AGENT => self.default_agent.clone(),
                _ => bail!("Fixture agent scope mismatch"),
            };
            storage.agent_scope = id.into();
            Ok(storage)
        }
        fn access<'a>(&'a self, actor: &'a str) -> webagent_access::storage::Repository<'a, Self> {
            webagent_access::storage::Repository::new(&self.root, &self.app, &self.agent, &self.agent_scope, actor, self)
        }
        fn models(&self) -> ModelRepository<'_, Self> {
            ModelRepository::new(&self.root, &self.app, &self.agent, AGENT, OWNER, self)
        }
    }
    impl RuntimeStorage for Storage {
        async fn credential_get<'a>(&'a self, key: &'a str) -> Result<Option<Value>> {
            let bytes: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
                .bind(key).fetch_optional(&self.app).await?;
            bytes.as_deref().map(|bytes| RecordCodec::unseal(self, bytes)).transpose()
        }
        async fn credential_put<'a>(&'a self, key: &'a str, value: &'a Value) -> Result<()> {
            sqlx::query("INSERT INTO docs VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
                .bind(key).bind(self.seal(value)?).execute(&self.app).await?;
            Ok(())
        }
        async fn agent_pool<'a>(&'a self, id: &'a str) -> Result<SqlitePool> {
            if !matches!(id, AGENT | DEFAULT_AGENT) { bail!("Fixture agent scope mismatch"); }
            let exists: Option<String> = sqlx::query_scalar("SELECT id FROM agent_directory WHERE id=?")
                .bind(id).fetch_optional(&self.app).await?;
            if exists.is_none() { bail!("Fixture agent missing"); }
            Ok(self.scoped(id)?.agent)
        }
    }
    impl RecordEffects for Storage {
        fn unseal(&self, bytes: &[u8]) -> Result<Value> { RecordCodec::unseal(self, bytes) }
        fn encode_user_record(&self, value: &Value) -> Result<Vec<u8>> { self.seal(value) }
        fn resolved_prompts(&self, root: &Path) -> Result<Value> {
            if root != self.root { bail!("Fixture prompt scope mismatch"); }
            Ok(serde_json::from_slice(&std::fs::read(root.join("config/prompts.json"))?)?)
        }
        fn prompt_defaults(&self) -> Result<Vec<u8>> { bail!("Existing fixture prompts must keep defaults lazy") }
        fn public_usage(&self, value: &Value) -> Value { value.clone() }
        fn record_usage(&self, _: &mut Value, _: &Value) { assert_eq!(self.agent_scope, AGENT); self.usage.fetch_add(1, Ordering::SeqCst); }
        fn invalidate_documents(&self, _: &[String]) {}
        async fn agent_pool<'a>(&'a self, id: &'a str) -> Result<SqlitePool> { RuntimeStorage::agent_pool(self, id).await }
        async fn list_documents<'a>(&'a self, prefix: &'a str) -> Result<Vec<Value>> {
            let rows: Vec<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE substr(key,1,?)=? ORDER BY key")
                .bind(prefix.len() as i64).bind(prefix).fetch_all(&self.user).await?;
            rows.iter().map(|bytes| RecordCodec::unseal(self, bytes)).collect()
        }
        async fn read_document<'a>(&'a self, key: &'a str) -> Result<Option<Value>> { self.user_document(key).await }
        async fn requester_claim_ready<'a>(&'a self, run: &'a Value) -> Result<bool> {
            Ok(run["requester_user_id"] == REQUESTER && run["agent_id"] == AGENT && run["session_id"] == SESSION)
        }
        async fn participation_allows_run<'a>(&'a self, run: &'a Value) -> Result<bool> {
            // This post-scope owner operation must receive the actual returned Store view.
            assert_eq!(self.agent_scope, AGENT);
            Ok(run["participation_generation"] == 1 && self.user_document(&format!("session:{SESSION}")).await?
                .is_some_and(|session| session["status"] == "active"))
        }
        async fn reconcile_requester_slots(&self) -> Result<()> { Ok(()) }
        async fn require_active_session_in<'a, 'c>(&'a self, tx: &'a mut Transaction<'c, Sqlite>, sid: &'a str) -> Result<()> where 'c: 'a {
            let bytes: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
                .bind(format!("session:{sid}")).fetch_one(&mut **tx).await?;
            if sid != SESSION || RecordCodec::unseal(self, &bytes)?["status"] != "active" { bail!("Fixture session unavailable"); }
            Ok(())
        }
        async fn write_document_in<'a, 'c>(&'a self, tx: &'a mut Transaction<'c, Sqlite>, key: &'a str, value: &'a Value) -> Result<()> where 'c: 'a {
            sqlx::query("INSERT INTO docs VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
                .bind(key).bind(self.seal(value)?).execute(&mut **tx).await?;
            Ok(())
        }
        async fn admit_message_in<'a, 'c>(&'a self, tx: &'a mut Transaction<'c, Sqlite>, id: &'a str, sid: &'a str, run: &'a Value, _: &'a Value, command_id: &'a str) -> Result<()> where 'c: 'a {
            // Concrete foreign-owner fixture effects only: the real owner bridge remains a root integration test.
            self.require_active_session_in(tx, sid).await?;
            sqlx::query("INSERT INTO owner_admissions VALUES(?)").bind(id).execute(&mut **tx).await?;
            self.write_document_in(tx, &format!("run:{id}"), run).await?;
            sqlx::query("INSERT INTO loop_commands VALUES(?)").bind(command_id).execute(&mut **tx).await?;
            Ok(())
        }
    }

    #[derive(Clone)]
    struct Authority { storage: Storage, actor: String, session: Option<String>, calls: Arc<SyncMutex<Vec<(String, String, String, String)>>> }
    impl Authority {
        fn trace(&self, kind: &str, user: &str, sid: &str, agent: &str) {
            self.calls.lock().unwrap().push((kind.into(), user.into(), sid.into(), agent.into()));
        }
        async fn session(&self, sid: &str, user: &str) -> Result<Value> {
            let value = self.storage.user_document(&format!("session:{sid}")).await?.ok_or_else(|| anyhow!("Fixture session missing"))?;
            if sid != SESSION || user != REQUESTER || value["status"] != "active" || value["agent_id"] != AGENT
                || value["owner_id"] != OWNER || value["writer"] != user { bail!("Fixture session binding changed"); }
            Ok(value)
        }
    }
    impl SessionAuthority for Authority {
        async fn live_access<'a>(&'a self, user: &'a str, sid: &'a str, agent: &'a str) -> Result<webagent_access::authority::LiveAccess> {
            self.session(sid, user).await?;
            assert_eq!(self.actor, user);
            assert_eq!(self.session.as_deref(), Some(sid));
            assert_eq!(self.storage.agent_scope, agent);
            if agent != AGENT { bail!("Fixture live agent mismatch"); }
            self.trace("live", user, sid, agent);
            self.storage.access(user).live_access(user, agent).await
        }
    }
    impl ReviewAuthority<Storage> for Authority {
        async fn session_scope<'a>(&'a self, sid: &'a str, requester: &'a str) -> Result<SessionScope<Storage, Self>> {
            let value = self.session(sid, requester).await?;
            self.trace("scope", requester, sid, AGENT);
            let storage = self.storage.scoped(AGENT)?;
            let authority = Authority { storage: storage.clone(), actor: requester.into(), session: Some(sid.into()), calls: self.calls.clone() };
            Ok(SessionScope { user_pool: storage.user.clone(), agent_pool: storage.agent.clone(), storage,
                owner_id: value["owner_id"].as_str().unwrap().into(), agent_id: AGENT.into(), actor: requester.into(),
                session: sid.into(), can_write: value["writer"] == requester, authority })
        }
        async fn modules_binding<'a>(&'a self, run: &'a Value) -> Result<()> {
            if run["requester_user_id"] != REQUESTER || run["agent_id"] != AGENT || run["session_id"] != SESSION { bail!("Fixture module binding changed"); }
            self.trace("modules", REQUESTER, SESSION, AGENT);
            Ok(())
        }
        fn session_operation<'a, T: Send + 'a>(&'a self, sid: &'a str, cancel: CancellationToken,
            operation: impl Future<Output = Result<T>> + Send + 'a) -> impl Future<Output = Result<T>> + Send + 'a {
            async move {
                self.session(sid, REQUESTER).await?;
                tokio::select! { biased; _ = cancel.cancelled() => Err(anyhow!("Fixture operation cancelled")), result = operation => result }
            }
        }
    }
    #[derive(Clone)]
    struct ProjectionOwner;
    impl Projection for ProjectionOwner {
        async fn project<'a>(&'a self, input: &'a webagent_provider_contracts::RequestV1,
            cancel: &'a CancellationToken) -> Result<webagent_provider_contracts::ResultV1> {
            if cancel.is_cancelled() { bail!("Fixture projection cancelled"); }
            webagent_provider_projection::project(input)
        }
    }
    impl Engine for ProjectionOwner {
        fn snapshot(&self) -> Result<(String, u64)> { Ok(("fixture-projection".into(), 1)) }
        fn current(&self, stamp: &(String, u64)) -> Result<bool> { Ok(stamp == &("fixture-projection".into(), 1)) }
    }
    type TestRuntime = Runtime<Storage, Authority, ProjectionOwner>;
    struct MockProvider { task: tokio::task::JoinHandle<()>, requests: Arc<Mutex<Vec<Value>>>, arrived: Arc<Notify>, release: Arc<Notify>, endpoint: String }
    impl Drop for MockProvider { fn drop(&mut self) { self.task.abort(); } }
    async fn mock_provider(answer: &str, held: bool) -> MockProvider {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let arrived = Arc::new(Notify::new()); let release = Arc::new(Notify::new());
        let captures = requests.clone(); let signal = arrived.clone(); let gate = release.clone(); let answer = answer.to_owned();
        let router = Router::new().route("/responses", post(move |Json(body): Json<Value>| {
            let requests = captures.clone(); let arrived = signal.clone(); let release = gate.clone(); let answer = answer.clone();
            async move {
                requests.lock().await.push(body); arrived.notify_one();
                if held { release.notified().await; }
                let event = json!({"type":"response.completed","response":{"status":"completed",
                    "usage":{"input_tokens":12,"output_tokens":4},"output":[{"type":"message","role":"assistant",
                    "content":[{"type":"output_text","text":answer}]}]}});
                ([("content-type", "text/event-stream")], format!("data: {event}\n\n"))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/responses", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); });
        MockProvider { task, requests, arrived, release, endpoint }
    }
    struct Fixture { _root: tempfile::TempDir, storage: Storage, runtime: TestRuntime, initial_runtime: TestRuntime }
    impl Fixture {
        async fn new(endpoint: String) -> Self {
            use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
            let root = tempfile::tempdir().unwrap();
            async fn pool(path: PathBuf) -> SqlitePool {
                SqlitePoolOptions::new().max_connections(4).connect_with(SqliteConnectOptions::new().filename(path)
                    .create_if_missing(true).journal_mode(SqliteJournalMode::Wal).busy_timeout(Duration::from_secs(5))).await.unwrap()
            }
            let app = pool(root.path().join("app.db")).await;
            let user = pool(root.path().join("user.db")).await;
            let agent = pool(root.path().join("agent.db")).await;
            let default_agent = pool(root.path().join("default-agent-trap.db")).await;
            sqlx::raw_sql("CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB); CREATE TABLE events(seq INTEGER PRIMARY KEY AUTOINCREMENT,session_id TEXT,kind TEXT,payload BLOB);
                CREATE TABLE user_accounts(user_id TEXT PRIMARY KEY,username TEXT,value BLOB); CREATE TABLE user_profiles(user_id TEXT PRIMARY KEY,value BLOB);
                CREATE TABLE agent_directory(id TEXT PRIMARY KEY,owner_id TEXT); CREATE TABLE agent_memberships(user_id TEXT,agent_id TEXT,value BLOB,PRIMARY KEY(user_id,agent_id));
                CREATE TABLE entitlement_tiers(tier_id TEXT PRIMARY KEY,revision INTEGER,value BLOB);").execute(&app).await.unwrap();
            sqlx::raw_sql("CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB); CREATE TABLE events(seq INTEGER PRIMARY KEY AUTOINCREMENT,session_id TEXT,kind TEXT,payload BLOB);
                CREATE TABLE owner_admissions(id TEXT PRIMARY KEY); CREATE TABLE loop_commands(command_id TEXT PRIMARY KEY);").execute(&user).await.unwrap();
            sqlx::raw_sql("CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB); CREATE TABLE agents(id TEXT PRIMARY KEY,status TEXT,metadata BLOB,revision INTEGER,updated_at INTEGER); CREATE TABLE agent_memberships(user_id TEXT,agent_id TEXT,value BLOB,PRIMARY KEY(user_id,agent_id));").execute(&agent).await.unwrap();
            sqlx::raw_sql("CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB); CREATE TABLE agents(id TEXT PRIMARY KEY,status TEXT,metadata BLOB,revision INTEGER,updated_at INTEGER); CREATE TABLE agent_memberships(user_id TEXT,agent_id TEXT,value BLOB,PRIMARY KEY(user_id,agent_id));").execute(&default_agent).await.unwrap();
            let storage = Storage { root: root.path().to_owned(), app, user, target_agent: agent.clone(), agent,
                default_agent, agent_scope: AGENT.into(),
                cipher: Aes256Gcm::new_from_slice(&[31; 32]).unwrap(), nonce: Arc::new(AtomicU64::new(1)), usage: Arc::new(AtomicU64::new(0)) };
            let access = storage.access(OWNER); access.init_access_policy(None).unwrap();
            let tier = json!({"id":"basic","revision":1,"allowed_models":["*"],"tools":{"*":"auto"},"max_sessions":10,"max_active_runs":10});
            let mut operator = tier.clone(); operator["id"] = json!("operator");
            access.write_access_policy(&json!({"schema_version":1,"registration":"open","revision":1,"tiers":{"basic":tier,"operator":operator}})).unwrap();
            for (id, role, tier) in [(OWNER, "administrator", "operator"), (REQUESTER, "user", "basic")] {
                let account = json!({"user_id":id,"username":id,"status":"active","role":role,"tier_id":tier,"access_revision":1,"generation":"fixture","is_approved":true});
                sqlx::query("INSERT INTO user_accounts VALUES(?,?,?)").bind(id).bind(id).bind(storage.seal(&account).unwrap()).execute(&storage.app).await.unwrap();
                sqlx::query("INSERT INTO user_profiles VALUES(?,?)").bind(id).bind(storage.seal(&json!({"is_admin":id==OWNER})).unwrap()).execute(&storage.app).await.unwrap();
                sqlx::query("INSERT INTO agent_memberships VALUES(?,?,?)").bind(id).bind(AGENT)
                    .bind(storage.seal(&json!({"user_id":id,"agent_id":AGENT,"relationship":if id==OWNER {"owner"}else{"member"},"agent_administrator":id==REQUESTER,"revision":1})).unwrap())
                    .execute(&storage.agent).await.unwrap();
            }
            for id in [AGENT, DEFAULT_AGENT] {
                sqlx::query("INSERT INTO agent_directory VALUES(?,?)").bind(id).bind(OWNER).execute(&storage.app).await.unwrap();
            }
            sqlx::query("INSERT INTO agents VALUES(?,'active',?,1,0)").bind(DEFAULT_AGENT)
                .bind(storage.seal(&json!({"owner_id":OWNER,"manager_incoming_message":{"enabled":false,"criteria":"Wrong default agent trap"},"manager_incoming_message_revision":99})).unwrap())
                .execute(&storage.default_agent).await.unwrap();
            sqlx::query("INSERT INTO agents VALUES(?,'active',?,1,0)").bind(AGENT)
                .bind(storage.seal(&json!({"owner_id":OWNER,"manager_incoming_message":{"enabled":true,"criteria":CRITERIA},"manager_incoming_message_revision":1})).unwrap())
                .execute(&storage.agent).await.unwrap();
            storage.put_user("control", &json!({"shutoff":false})).await.unwrap();
            storage.put_user(&format!("session:{SESSION}"), &json!({"id":SESSION,"status":"active","owner_id":OWNER,"writer":REQUESTER,"agent_id":AGENT})).await.unwrap();
            std::fs::write(root.path().join("config/prompts.json"), serde_json::to_vec(&json!({"manager_incoming_message":{(AGENT):CRITERIA},"fixture_extension":true})).unwrap()).unwrap();
            let shared = BTreeSet::from([AGENT.to_owned()]); let mut catalog = Catalog::default();
            catalog.connections.insert("fixture-connection".into(), Connection { id:"fixture-connection".into(), owner_id:REQUESTER.into(), transport:Transport::OpenaiSubscription,
                credential_ref:"provider_credential:fixture".into(), allowed_agents:shared.clone(), revision:1, revoked:false });
            catalog.entries.insert("fixture-model".into(), ModelEntry { id:"fixture-model".into(), owner_id:REQUESTER.into(), connection_id:"fixture-connection".into(),
                upstream_model:"fixture-model".into(), display_name:"Fixture".into(), capabilities:BTreeSet::from(["text".into()]), allowed_agents:shared, revision:1 });
            catalog.agent_models.insert(AGENT.into(), AgentModels { limits:BTreeMap::new(), efforts:BTreeMap::new(), agent_id:AGENT.into(), owner_id:OWNER.into(), entry_ids:vec!["fixture-model".into()], revision:1,
                roles:BTreeMap::from([(ModelRole::Standard, RoleAssignment { entry_id:"fixture-model".into(), backup_entry_ids:vec![] }), (ModelRole::System, RoleAssignment { entry_id:"fixture-model".into(), backup_entry_ids:vec![] })]) });
            storage.models().save_model_catalog(&catalog, 0).await.unwrap();
            storage.credential_put("provider_credential:fixture", &json!({"access_token":"synthetic-local-only-token","expires_at":crate::runtime::now()+3600})).await.unwrap();
            let authority = Authority { storage:storage.clone(), actor:REQUESTER.into(), session:Some(SESSION.into()), calls:Arc::new(SyncMutex::new(Vec::new())) };
            let runtime = Runtime { root:root.path().to_owned(), user_pool:storage.user.clone(), app_pool:storage.app.clone(), agent_pool:storage.agent.clone(),
                owner:OWNER.into(), agent:AGENT.into(), actor:REQUESTER.into(), session:Some(SESSION.into()), storage:storage.clone(), authority, engine:ProjectionOwner,
                provider:Provider::for_test(endpoint), gate:Arc::new(Mutex::new(())), refresh:Arc::new(Mutex::new(())), login:Arc::new(RwLock::new(None)),
                stop:CancellationToken::new(), ready:Arc::new(AtomicBool::new(true)) };
            // The scheduler begins with the ordinary per-user owner view, before record-agent/requester scope.
            // The user pool can find/claim the review. Only agent/actor/session/authority are wrong.
            let initial_storage = storage.scoped(DEFAULT_AGENT).unwrap();
            let mut initial_runtime = runtime.clone();
            initial_runtime.storage = initial_storage.clone();
            initial_runtime.agent_pool = initial_storage.agent.clone();
            initial_runtime.agent = DEFAULT_AGENT.into(); initial_runtime.actor = OWNER.into(); initial_runtime.session = None;
            initial_runtime.authority = Authority { storage:initial_storage, actor:OWNER.into(), session:None, calls:runtime.authority.calls.clone() };
            Self { _root:root, storage, runtime, initial_runtime }
        }
        async fn submit(&self) {
            let prepared = self.runtime.prepare(AGENT).await.unwrap().unwrap();
            let mut plan = self.storage.models().model_catalog().await.unwrap().resolve_role(AGENT, ModelRole::Standard).unwrap();
            let gate = self.runtime.gate.clone(); let refresh = self.runtime.refresh.clone(); let login = self.runtime.login.clone(); let stop = self.runtime.stop.clone();
            let models = webagent_models::runtime::Runtime::new(self.storage.models(), &self.runtime.provider, &gate, &refresh, &login, &stop);
            models.pin_model_plan(&mut plan, REQUESTER, Some(SESSION), &self.runtime.authority).await.unwrap();
            let run = json!({"id":REVIEW,"session_id":SESSION,"agent_id":AGENT,"requester_user_id":REQUESTER,"participation_generation":1,
                "selection_plan":plan,"failover_state":FailoverState::default(),"status":"queued"});
            let record = json!({"id":REVIEW,"session_id":SESSION,"agent_id":AGENT,"status":"pending_review","run":run,
                "message":{"role":"user","content":MESSAGE,"author_id":REQUESTER},"settings":prepared["settings"],"settings_revision":prepared["settings_revision"],
                "selection_plan":prepared["selection_plan"],"failover_state":prepared["failover_state"],"usage":{}});
            self.storage.put_user(&format!("manager_review:{REVIEW}"), &record).await.unwrap();
            self.storage.put_user(&format!("manager_pending:{SESSION}"), &json!({"id":REVIEW})).await.unwrap();
            // Exercise the actual handoff directly as well as through process; a field-only check cannot stand in for pool rebinding.
            assert_eq!(self.initial_runtime.owner, OWNER);
            assert_eq!(self.initial_runtime.agent, DEFAULT_AGENT);
            assert_eq!(self.initial_runtime.actor, OWNER);
            assert!(self.initial_runtime.session.is_none());
            let rebound = self.initial_runtime.review_scope(&record).await.unwrap();
            assert_eq!(rebound.owner, OWNER); assert_eq!(rebound.agent, AGENT);
            assert_eq!(rebound.actor, REQUESTER); assert_eq!(rebound.session.as_deref(), Some(SESSION));
            assert_eq!(rebound.storage.agent_scope, AGENT);
            assert_eq!(rebound.authority.actor, REQUESTER);
            assert_eq!(rebound.authority.session.as_deref(), Some(SESSION));
            let actual_agent: String = sqlx::query_scalar("SELECT id FROM agents").fetch_one(&rebound.agent_pool).await.unwrap();
            assert_eq!(actual_agent, AGENT);
            let bytes: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(format!("session:{SESSION}")).fetch_one(&rebound.user_pool).await.unwrap();
            assert_eq!(RecordCodec::unseal(&rebound.storage, &bytes).unwrap()["owner_id"], OWNER);
        }
        async fn record(&self) -> Value { self.storage.user_document(&format!("manager_review:{REVIEW}")).await.unwrap().unwrap() }
        async fn update_criteria(&self, criteria: &str, revision: u64) {
            let _guard = self.runtime.gate.lock().await;
            let raw: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?").bind(AGENT).fetch_one(&self.storage.agent).await.unwrap();
            let mut metadata = RecordCodec::unseal(&self.storage, &raw).unwrap();
            metadata["manager_incoming_message_revision"] = json!(revision);
            metadata["manager_incoming_message"]["criteria"] = json!(criteria);
            sqlx::query("UPDATE agents SET metadata=? WHERE id=?").bind(self.storage.seal(&metadata).unwrap()).bind(AGENT).execute(&self.storage.agent).await.unwrap();
            crate::settings::save_prompt(&self.storage.root, AGENT, criteria, || bail!("Defaults must stay lazy")).unwrap();
        }
        async fn admissions(&self) -> i64 { sqlx::query_scalar("SELECT count(*) FROM owner_admissions").fetch_one(&self.storage.user).await.unwrap() }
        fn check_authority_trace(&self) {
            let calls = self.runtime.authority.calls.lock().unwrap(); assert!(!calls.is_empty());
            for (_, user, session, agent) in calls.iter() { assert_eq!(user, REQUESTER); assert_eq!(session, SESSION); assert_eq!(agent, AGENT); }
            assert!(calls.iter().any(|(kind, _, _, _)| kind == "live"));
            assert!(calls.iter().any(|(kind, _, _, _)| kind == "modules"));
        }
    }
    async fn assert_request(server: &MockProvider) {
        let requests = server.requests.lock().await; assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["model"], "fixture-model");
        assert!(requests[0].get("tools").and_then(Value::as_array).is_none_or(Vec::is_empty));
        assert!(requests[0]["instructions"].as_str().unwrap().contains(CRITERIA));
        assert!(requests[0]["input"].to_string().contains(MESSAGE));
    }
    #[tokio::test]
    async fn process_uses_real_system_transport_and_rejects_malformed_verdict() {
        for (answer, status, admitted) in [(r#"{"verdict":"acceptable"}"#, "acceptable", 1), ("acceptable", "review_error", 0)] {
            let server = mock_provider(answer, false).await;
            let fixture = Fixture::new(server.endpoint.clone()).await; fixture.submit().await;
            crate::scheduler::process(fixture.initial_runtime.clone(), REVIEW.into()).await.unwrap();
            assert_request(&server).await;
            let record = fixture.record().await; assert_eq!(record["status"], status);
            assert_eq!(fixture.admissions().await, admitted);
            assert_eq!(fixture.storage.user_document(&format!("run:{REVIEW}")).await.unwrap().is_some(), admitted == 1);
            assert!(fixture.storage.user_document(&format!("manager_pending:{SESSION}")).await.unwrap().is_none());
            assert_eq!(fixture.storage.usage.load(Ordering::SeqCst), 1);
            assert!(!fixture.runtime.repository().public(&record).to_string().contains(CRITERIA));
            fixture.check_authority_trace();
            crate::scheduler::process(fixture.initial_runtime.clone(), REVIEW.into()).await.unwrap();
            assert_eq!(server.requests.lock().await.len(), 1); assert_eq!(fixture.admissions().await, admitted);
        }
    }
    #[tokio::test]
    async fn process_discards_late_response_after_same_criteria_restored_revision_change() {
        let server = mock_provider(r#"{"verdict":"acceptable"}"#, true).await;
        let fixture = Fixture::new(server.endpoint.clone()).await; fixture.submit().await;
        let pending = tokio::spawn(crate::scheduler::process(fixture.initial_runtime.clone(), REVIEW.into()));
        tokio::time::timeout(Duration::from_secs(5), server.arrived.notified()).await.unwrap();
        fixture.update_criteria("Changed private fixture criteria", 2).await;
        fixture.update_criteria(CRITERIA, 3).await;
        server.release.notify_one(); pending.await.unwrap().unwrap();
        assert_request(&server).await;
        let record = fixture.record().await; assert_eq!(record["status"], "review_error");
        assert_eq!(fixture.admissions().await, 0);
        assert!(fixture.storage.user_document(&format!("run:{REVIEW}")).await.unwrap().is_none());
        assert_eq!(fixture.storage.usage.load(Ordering::SeqCst), 1);
        assert!(!fixture.runtime.repository().public(&record).to_string().contains(CRITERIA));
        fixture.check_authority_trace();
        crate::scheduler::process(fixture.initial_runtime.clone(), REVIEW.into()).await.unwrap();
        assert_eq!(server.requests.lock().await.len(), 1);
    }
}
