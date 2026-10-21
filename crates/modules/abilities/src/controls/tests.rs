use super::*;
use aes_gcm::{Aes256Gcm,KeyInit,Nonce,aead::Aead};
use std::sync::atomic::{AtomicU64,Ordering};

struct Codec {cipher:Aes256Gcm,sequence:AtomicU64}
impl Codec {fn new()->Self {Self {cipher:Aes256Gcm::new_from_slice(&[23;32]).unwrap(),sequence:AtomicU64::new(1)}}}
impl RecordCodec for Codec {
    fn seal(&self,value:&Value)->Result<Vec<u8>> {
        let mut nonce=[0;12];nonce[4..].copy_from_slice(&self.sequence.fetch_add(1,Ordering::Relaxed).to_be_bytes());
        let mut bytes=nonce.to_vec();bytes.extend(self.cipher.encrypt(Nonce::from_slice(&nonce),serde_json::to_vec(value)?.as_slice()).map_err(|_|anyhow!("Fixture encryption failed"))?);Ok(bytes)
    }
    fn unseal(&self,raw:&[u8])->Result<Value> {
        if raw.len()<28 {bail!("Invalid encrypted fixture");}
        let bytes=self.cipher.decrypt(Nonce::from_slice(&raw[..12]),&raw[12..]).map_err(|_|anyhow!("Fixture authentication failed"))?;
        Ok(serde_json::from_slice(&bytes)?)
    }
    fn encode_application(&self,value:&Value)->Result<Vec<u8>> {self.seal(value)}
}
struct Fixture {dir:tempfile::TempDir,pool:SqlitePool,codec:Codec}
impl Fixture {
    async fn new()->Result<Self> {
        let dir=tempfile::tempdir()?;
        let pool=sqlx::sqlite::SqlitePoolOptions::new().max_connections(1).connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(dir.path().join("agent.sqlite")).create_if_missing(true)).await?;
        sqlx::raw_sql("CREATE TABLE agents(id TEXT PRIMARY KEY,metadata BLOB NOT NULL,revision INTEGER NOT NULL,updated_at INTEGER);
            CREATE TABLE agent_connections(id TEXT PRIMARY KEY,agent_id TEXT,connection_type TEXT,section TEXT,enabled INTEGER,config BLOB,revision INTEGER,created_at INTEGER,updated_at INTEGER);
            CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB NOT NULL);").execute(&pool).await?;
        let f=Self {dir,pool,codec:Codec::new()};
        sqlx::query("INSERT INTO agents VALUES(?,?,1,0)").bind("agent_a").bind(f.codec.seal(&json!({"ability_revision":0,"unrelated":{"saved":true},"ability_modes":{}}))?).execute(&f.pool).await?;
        Ok(f)
    }
    fn repo(&self)->Repository<'_,Codec> {Repository::new(&self.pool,"agent_a","alice",&self.codec)}
    async fn metadata(&self)->Result<(Value,i64)> {
        let row=sqlx::query("SELECT metadata,revision FROM agents WHERE id='agent_a'").fetch_one(&self.pool).await?;
        Ok((self.codec.unseal(&row.get::<Vec<u8>,_>("metadata"))?,row.get("revision")))
    }
}
fn controls()->Value {json!({"visibility":{"terminal_control":"off","web_access":"visible"},"skill_visibility":{"web_access":"discoverable"},"tool_modes":{},"settings":{"web_access":{"enabled":true}},"tool_execution":{},"tool_result_deduplication":{"web_search":false}})}

#[tokio::test]
async fn encrypted_controls_cas_scope_and_audit_survive_reopen()->Result<()> {
    let f=Fixture::new().await?;
    f.repo().save_agent_abilities(&controls(),0).await?;
    f.repo().ensure_agent_connections(&["terminal_control".into(),"web_access".into()]).await?;
    let (metadata,revision)=f.metadata().await?;
    assert_eq!(revision,2);assert_eq!(metadata["unrelated"]["saved"],true);
    assert_eq!(metadata["ability_revision"],1);
    let projection=f.repo().agent_abilities_for("agent_a").await?;
    assert_eq!(projection["visibility"]["terminal_control"],"off");
    assert_eq!(projection["visibility"]["web_access"],"visible");
    assert_eq!(projection["tool_result_deduplication"]["web_search"],false);
    assert!(f.repo().agent_abilities_for("other_agent").await.is_err());
    assert!(f.repo().save_agent_abilities(&controls(),0).await.is_err());
    assert!(f.repo().save_agent_abilities(&controls(),u64::MAX).await.is_err());
    let bytes:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key='agent:agent_a:ability_visibility_audit:1'").fetch_one(&f.pool).await?;
    assert!(!String::from_utf8_lossy(&bytes).contains("alice"));
    assert_eq!(f.codec.unseal(&bytes)?,json!({"agent_id":"agent_a","actor_id":"alice","revision":1}));
    f.pool.close().await;
    let pool=sqlx::sqlite::SqlitePoolOptions::new().max_connections(1).connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(f.dir.path().join("agent.sqlite"))).await?;
    let reopened=Repository::new(&pool,"agent_a","alice",&f.codec);
    assert_eq!(reopened.agent_abilities_for("agent_a").await?,projection);
    pool.close().await;Ok(())
}

#[tokio::test]
async fn late_invalid_settings_roll_back_connections_metadata_and_audit()->Result<()> {
    let f=Fixture::new().await?;let before=f.metadata().await?;
    let mut invalid=controls();invalid["tool_result_deduplication"]=json!({"web_search":"invalid"});
    assert!(f.repo().save_agent_abilities(&invalid,0).await.is_err());
    assert_eq!(f.metadata().await?,before);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM agent_connections").fetch_one(&f.pool).await?,0);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM docs").fetch_one(&f.pool).await?,0);
    f.repo().save_agent_abilities(&controls(),0).await?;
    assert_eq!(f.repo().agent_abilities_for("agent_a").await?["revision"],1);
    f.pool.close().await;Ok(())
}

#[tokio::test]
async fn late_audit_failure_rolls_back_every_control_write()->Result<()> {
    let f=Fixture::new().await?;let before=f.metadata().await?;
    sqlx::query("INSERT INTO docs VALUES(?,?)").bind("agent:agent_a:ability_visibility_audit:1").bind(f.codec.seal(&json!({"existing":true}))?).execute(&f.pool).await?;
    assert!(f.repo().save_agent_abilities(&controls(),0).await.is_err());
    assert_eq!(f.metadata().await?,before);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM agent_connections").fetch_one(&f.pool).await?,0);
    f.pool.close().await;Ok(())
}
