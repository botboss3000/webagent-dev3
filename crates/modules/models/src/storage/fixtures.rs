use super::*;
use aes_gcm::{Aes256Gcm,KeyInit,Nonce,aead::Aead};
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions,SqliteJournalMode,SqlitePoolOptions};
use std::sync::{Arc,atomic::{AtomicU64,Ordering}};
#[derive(Clone)]
pub(crate) struct Storage {
    pub app:SqlitePool,pub agent:SqlitePool,
    cipher:Aes256Gcm,nonce:Arc<AtomicU64>,pub reads:Arc<AtomicU64>,
}
impl RecordCodec for Storage {
    fn seal(&self,value:&Value)->Result<Vec<u8>> {
        let mut nonce=[0;12];nonce[4..].copy_from_slice(&self.nonce.fetch_add(1,Ordering::Relaxed).to_be_bytes());
        let mut data=nonce.to_vec();
        data.extend(self.cipher.encrypt(Nonce::from_slice(&nonce),serde_json::to_vec(value)?.as_ref()).map_err(|_|anyhow!("Encryption failed"))?);Ok(data)
    }
    fn unseal(&self,data:&[u8])->Result<Value> {
        if data.len()<28 {return Err(anyhow!("Invalid encrypted record"));}
        let bytes=self.cipher.decrypt(Nonce::from_slice(&data[..12]),&data[12..]).map_err(|_|anyhow!("Storage authentication failed"))?;
        Ok(serde_json::from_slice(&bytes)?)
    }
    fn encode_application(&self,value:&Value)->Result<Vec<u8>> {self.seal(value)}
}
impl RuntimeStorage for Storage {
    fn credential_get<'a>(&'a self,key:&'a str)->impl Future<Output=Result<Option<Value>>>+Send+'a {
        async move {
            self.reads.fetch_add(1,Ordering::Relaxed);
            let row:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(key).fetch_optional(&self.app).await?;
            row.map(|bytes|self.unseal(&bytes)).transpose()
        }
    }
    fn credential_put<'a>(&'a self,key:&'a str,value:&'a Value)->impl Future<Output=Result<()>>+Send+'a {
        async move {
            sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
                .bind(key).bind(self.seal(value)?).execute(&self.app).await?;Ok(())
        }
    }
    fn agent_pool<'a>(&'a self,agent:&'a str)->impl Future<Output=Result<SqlitePool>>+Send+'a {
        async move {
            if !webagent_access::valid_agent_id(agent) {return Err(anyhow!("Invalid agent identity"));}
            let found:Option<String>=sqlx::query_scalar("SELECT id FROM agent_directory WHERE id=?").bind(agent).fetch_optional(&self.app).await?;
            if found.is_none() {return Err(anyhow!("Agent not found"));}Ok(self.agent.clone())
        }
    }
}
pub(crate) struct Fixture {pub root:tempfile::TempDir,pub storage:Storage}
impl Fixture {
    pub fn repo(&self)->Repository<'_,Storage> {Repository::new(self.root.path(),&self.storage.app,&self.storage.agent,"shared_default","admin",&self.storage)}
    pub async fn new()->Self {
        let root=tempfile::tempdir().unwrap();
        async fn pool(path:std::path::PathBuf)->SqlitePool {
            SqlitePoolOptions::new().max_connections(4).connect_with(SqliteConnectOptions::new().filename(path)
                .create_if_missing(true).journal_mode(SqliteJournalMode::Wal).busy_timeout(std::time::Duration::from_secs(5))).await.unwrap()
        }
        let app=pool(root.path().join("app.db")).await;let agent=pool(root.path().join("agent.db")).await;
        sqlx::raw_sql("CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB);
            CREATE TABLE events(seq INTEGER PRIMARY KEY AUTOINCREMENT,session_id TEXT,kind TEXT,payload BLOB);
            CREATE TABLE user_accounts(user_id TEXT PRIMARY KEY,username TEXT,value BLOB);
            CREATE TABLE user_profiles(user_id TEXT PRIMARY KEY,value BLOB);
            CREATE TABLE agent_directory(id TEXT PRIMARY KEY,owner_id TEXT);
            CREATE TABLE agent_memberships(user_id TEXT,agent_id TEXT,value BLOB,PRIMARY KEY(user_id,agent_id));
            CREATE TABLE entitlement_tiers(tier_id TEXT PRIMARY KEY,revision INTEGER,value BLOB);").execute(&app).await.unwrap();
        sqlx::raw_sql("CREATE TABLE agents(id TEXT PRIMARY KEY,status TEXT,metadata BLOB,revision INTEGER,updated_at INTEGER);
            CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB);").execute(&agent).await.unwrap();
        let storage=Storage {app,agent,cipher:Aes256Gcm::new_from_slice(&[27;32]).unwrap(),nonce:Arc::new(AtomicU64::new(1)),reads:Arc::new(AtomicU64::new(0))};
        let f=Self {root,storage};let repo=f.repo();let access=repo.access();access.init_access_policy(None).unwrap();
        let basic=json!({"id":"basic","revision":1,"allowed_models":["*"],"tools":{"*":"auto"},"max_sessions":10,"max_active_runs":10});
        let mut operator=basic.clone();operator["id"]=json!("operator");
        access.write_access_policy(&json!({"schema_version":1,"registration":"open","revision":1,"tiers":{"basic":basic,"operator":operator}})).unwrap();
        for (id,role,tier) in [("admin","administrator","operator"),("alice","user","basic")] {
            let value=json!({"user_id":id,"username":id,"status":"active","role":role,"tier_id":tier,"access_revision":1,"generation":"initial","is_approved":true});
            sqlx::query("INSERT INTO user_accounts VALUES(?,?,?)").bind(id).bind(id).bind(f.storage.seal(&value).unwrap()).execute(&f.storage.app).await.unwrap();
            sqlx::query("INSERT INTO user_profiles VALUES(?,?)").bind(id).bind(f.storage.seal(&json!({"is_admin":id=="admin"})).unwrap()).execute(&f.storage.app).await.unwrap();
        }
        sqlx::query("INSERT INTO agent_directory VALUES('shared_default','admin')").execute(&f.storage.app).await.unwrap();
        sqlx::query("INSERT INTO agents VALUES('shared_default','active',?,1,0)").bind(f.storage.seal(&json!({"owner_id":"admin"})).unwrap()).execute(&f.storage.agent).await.unwrap();
        for (id,relationship,administrator) in [("admin","owner",false),("alice","member",true)] {
            let value=json!({"user_id":id,"agent_id":"shared_default","relationship":relationship,"agent_administrator":administrator,"revision":1});
            sqlx::query("INSERT INTO agent_memberships VALUES(?,'shared_default',?)").bind(id).bind(f.storage.seal(&value).unwrap()).execute(&f.storage.app).await.unwrap();
        }
        f
    }
}
pub(crate) fn byo_catalog()->crate::model_selection::Catalog {
    use crate::model_selection::*;
    use std::collections::{BTreeMap,BTreeSet};
    let mut catalog=Catalog::default();
    catalog.connections.insert("connection".into(),Connection {id:"connection".into(),owner_id:"alice".into(),transport:Transport::Deepseek,
        credential_ref:"provider_credential:synthetic".into(),allowed_agents:BTreeSet::new(),revision:1,revoked:false});
    catalog.entries.insert("model".into(),ModelEntry {id:"model".into(),owner_id:"alice".into(),connection_id:"connection".into(),
        upstream_model:"model-a".into(),display_name:"Model".into(),capabilities:BTreeSet::from(["text".into()]),allowed_agents:BTreeSet::new(),revision:1});
    catalog
}
