//! Independent real IO: no Engine, App, copied permission algorithm or service supervisor.
use anyhow::Result;
use serde_json::{json,Value};
use sqlx::{Sqlite,Transaction};
use aes_gcm::{Aes256Gcm,KeyInit,Nonce,aead::Aead};
use crate::{ledger,ports::Records};
struct Codec {cipher:Aes256Gcm}
impl Codec {
 fn new()->Self {Self{cipher:Aes256Gcm::new_from_slice(&[7;32]).unwrap()}}
 fn seal(&self,value:&Value)->Vec<u8> {
  let id=uuid::Uuid::new_v4();let nonce=&id.as_bytes()[..12];
  let encrypted=self.cipher.encrypt(Nonce::from_slice(nonce),serde_json::to_vec(value).unwrap().as_slice()).unwrap();
  [nonce,encrypted.as_slice()].concat()
 }
}
impl Records for Codec {
 fn current_user_id(&self)->&str {"owner"}
 fn unseal(&self,raw:&[u8])->Result<Value> {
  anyhow::ensure!(raw.len()>=12,"Invalid encrypted fixture");
  let plain=self.cipher.decrypt(Nonce::from_slice(&raw[..12]),&raw[12..]).map_err(|_|anyhow::anyhow!("Invalid encrypted fixture"))?;
  Ok(serde_json::from_slice(&plain)?)
 }
 fn write_document<'a,'c>(&'a self,tx:&'a mut Transaction<'c,Sqlite>,key:&'a str,value:&'a Value)->impl std::future::Future<Output=Result<()>>+Send+'a where 'c:'a {async move {
  sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
   .bind(key).bind(self.seal(value)).execute(&mut **tx).await?;Ok(())
 }}
}
fn staged(requester:&str,expected:u64,revision:u64)->Value {
 let mut state=webagent_assets_contracts::State::default();state.revision=revision;
 state.selectors=vec![json!({"type":"tool","tool_name":"file_read"})];
 json!({"session_id":"conversation","agent_id":"nondefault","requester_user_id":requester,
 "_context_assets_write":{"scope":ledger::scope_key("nondefault",requester),"expected_revision":expected,"state":state}})
}
#[tokio::test]
async fn real_scoped_ledger_cas_and_caller_transaction_rollback()->Result<()> {
 let dir=tempfile::tempdir()?;
 let options=sqlx::sqlite::SqliteConnectOptions::new().filename(dir.path().join("user.sqlite")).create_if_missing(true);
 let pool=sqlx::sqlite::SqlitePoolOptions::new().max_connections(2).connect_with(options).await?;
 sqlx::query("CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB NOT NULL)").execute(&pool).await?;
 let codec=Codec::new();
 sqlx::query("INSERT INTO docs VALUES(?,?)").bind("session:conversation").bind(codec.seal(&json!({"status":"active","agent_id":"nondefault"}))).execute(&pool).await?;
 let repo=ledger::Repository::new(dir.path(),&pool,"nondefault",codec);
 let mut tx=pool.begin().await?;
 repo.apply_context_assets_write(&mut tx,&staged("alice",0,1)).await?;
 // A late foreign-owner effect fails in the SAME writer; no ledger commit may escape it.
 assert!(sqlx::query("INSERT INTO nonexistent_history VALUES(1)").execute(&mut *tx).await.is_err());
 tx.rollback().await?;
 assert_eq!(repo.context_assets("conversation","nondefault","alice").await?.revision,0);
 let mut tx=pool.begin().await?;repo.apply_context_assets_write(&mut tx,&staged("alice",0,1)).await?;tx.commit().await?;
 assert_eq!(repo.context_assets("conversation","nondefault","alice").await?.revision,1);
 assert_eq!(repo.context_assets("conversation","nondefault","bob").await?.revision,0);
 let mut tx=pool.begin().await?;assert!(repo.apply_context_assets_write(&mut tx,&staged("alice",0,1)).await.is_err());tx.rollback().await?;
 assert!(repo.context_assets("conversation","default","alice").await.is_err());
 let raw:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key='session_context_assets:conversation'").fetch_one(&pool).await?;
 assert!(!String::from_utf8_lossy(&raw).contains("file_read"));
 pool.close().await;Ok(())
}
#[test]
fn live_policy_parser_preserves_invalid_bytes_and_exact_cap()->Result<()> {
 let dir=tempfile::tempdir()?;std::fs::create_dir(dir.path().join("config"))?;
 let path=dir.path().join("config/tool-output-retention.json");
 let invalid=br#"{"enabled":false,"enabled":true}"#;
 std::fs::write(&path,invalid)?;assert!(crate::retention::Policy::read(dir.path()).is_err());assert_eq!(std::fs::read(&path)?,invalid);
 let mut exact=br#"{"enabled":false}"#.to_vec();exact.resize(64*1024,b' ');
 assert!(crate::retention::Policy::parse(&exact).is_ok());exact.push(b' ');assert!(crate::retention::Policy::parse(&exact).is_err());Ok(())
}
