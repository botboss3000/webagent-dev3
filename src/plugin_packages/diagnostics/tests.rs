//! Independent production IO boundaries; no root App/Store or copied owner algorithms.
use crate::{config::Config,recorder::*,SnapshotStore};
use webagent_context::input::Request;
use anyhow::{anyhow,Result};
use serde_json::{json,Value};
use aes_gcm::{aead::Aead,Aes256Gcm,KeyInit,Nonce};
use std::sync::{Arc,atomic::{AtomicU64,Ordering}};
#[derive(Clone)]
struct Codec {cipher:Aes256Gcm,nonce:Arc<AtomicU64>}
impl Codec {fn new()->Self {Self{cipher:Aes256Gcm::new_from_slice(&[29;32]).unwrap(),nonce:Arc::new(AtomicU64::new(1))}}}
impl SnapshotStore for Codec {
 fn current_user_id(&self)->&str {"admin"}
 fn seal(&self,value:&Value)->Result<Vec<u8>> {
  let mut nonce=[0;12];nonce[4..].copy_from_slice(&self.nonce.fetch_add(1,Ordering::Relaxed).to_be_bytes());
  let mut sealed=nonce.to_vec();sealed.extend(self.cipher.encrypt(Nonce::from_slice(&nonce),serde_json::to_vec(value)?.as_slice()).map_err(|_|anyhow!("Fixture encryption failed"))?);Ok(sealed)
 }
 fn unseal(&self,sealed:&[u8])->Result<Value> {
  if sealed.len()<28 {return Err(anyhow!("Fixture record rejected"));}
  let raw=self.cipher.decrypt(Nonce::from_slice(&sealed[..12]),&sealed[12..]).map_err(|_|anyhow!("Fixture authentication failed"))?;Ok(serde_json::from_slice(&raw)?)
 }
 async fn session_exists(&self,_:&str)->Result<bool,crate::Error> {Ok(false)}
}
#[test]
fn shared_config_digest_reload_and_fail_closed_sentinel() {
 let root=tempfile::tempdir().unwrap();let path=root.path().join("diagnostics.json");
 let disabled=Config::disabled();disabled.validate().unwrap();assert!(!disabled.active());
 let mut enabled=disabled.clone();enabled.enabled=true;
 let raw=serde_json::to_vec(&enabled).unwrap();std::fs::write(&path,&raw).unwrap();
 let (parsed,digest)=Config::read(&path).unwrap();assert!(parsed.active());assert_eq!(digest,crate::digest(&raw));
 let changed=path.with_extension("next");std::fs::write(&changed,serde_json::to_vec(&disabled).unwrap()).unwrap();std::fs::rename(changed,&path).unwrap();assert!(!Settings::read(&path).config.active());
 std::fs::write(&path,b"{bad}").unwrap();let invalid=Settings::read(&path);assert!(!invalid.config.active());assert!(invalid.error.is_some());
 std::fs::remove_file(&path).unwrap();let missing=Settings::read(&path);assert!(!missing.config.active());assert!(missing.error.is_some());assert!(!path.exists());
 let mut unknown=serde_json::to_value(&enabled).unwrap();unknown["unknown"]=json!(true);std::fs::write(&path,serde_json::to_vec(&unknown).unwrap()).unwrap();assert!(Config::read(&path).is_err());
}
#[tokio::test]
async fn encrypted_archive_checks_identity_expiry_and_retention() {
 let root=tempfile::tempdir().unwrap();let pool=open(root.path()).await.unwrap();let store=Codec::new();let config=Config::disabled();
 let payload=json!({"owner":"admin","attempt_id":"fixture-attempt","session_id":"fixture-session","run_id":"fixture-run","history":"fixture private context"});
 let sealed=store.seal(&payload).unwrap();assert!(!String::from_utf8_lossy(&sealed).contains("fixture private context"));
 insert_snapshot(&pool,&payload,&sealed,123,&config).await.unwrap();
 assert_eq!(read_snapshot(&pool,&store,"fixture-session","fixture-run","fixture-attempt").await.unwrap(),payload);
 assert!(read_snapshot(&pool,&store,"wrong-session","fixture-run","fixture-attempt").await.is_err());
 let wrong=store.seal(&json!({"owner":"other","attempt_id":"fixture-attempt","session_id":"fixture-session","run_id":"fixture-run"})).unwrap();
 sqlx::query("UPDATE context_snapshots SET sealed=?").bind(wrong).execute(&pool).await.unwrap();assert!(read_snapshot(&pool,&store,"fixture-session","fixture-run","fixture-attempt").await.is_err());
 sqlx::query("UPDATE context_snapshots SET sealed=?,expires_at=0").bind(sealed).execute(&pool).await.unwrap();assert!(read_snapshot(&pool,&store,"fixture-session","fixture-run","fixture-attempt").await.is_err());prune(&pool,&config).await.unwrap();let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM context_snapshots").fetch_one(&pool).await.unwrap();assert_eq!(count,0);
}

#[test]
fn assembly_evidence_is_content_free_and_snapshot_is_exact() {
    let history = vec![
        json!({"role":"user","content":"PRIVATE"}),
        json!({"type":"function_call","call_id":"c","name":"file_read"}),
        json!({"type":"function_call_output","call_id":"c","output":"PRIVATE"}),
    ];
    let selection = webagent_models::provider::ProviderSelection::parse(
        &json!({"provider":"deepseek","model":"deepseek-chat"}),
        "unused",
    )
    .unwrap();
    let compaction = webagent_context::compaction::Config::configured().unwrap();
    let contributions = [];
    let schemas = [json!({"name":"file_read","description":"PRIVATE"})];
    let request = Request {
        history: &history,
        base: "PRIVATE",
        guidance: " PRIVATE",
        schemas: &schemas,
        contributions: &contributions,
        identity: webagent_context::input::CallIdentity { turn: 1, call: 2 },
        selection: &selection,
        max_bytes: 119808,
        compaction: &compaction,
    };
    let measured = measure(&request);
    assert!(!measured.to_string().contains("PRIVATE"));
    assert_eq!(measured["compaction"]["pending_calls"], 0);
    let digest = Some("fixture".into());
    let report = json!({"outcome":"ready"});
    let sid = json!("s");
    let rid = json!("r");
    let capture = Capture {
        owner: "admin",
        category: "context_assembly",
        config_digest: &digest,
        attempt_id: "a",
        session_id: &sid,
        run_id: &rid,
        provider_selection: selection.value(),
        report: &report,
        assembled_instructions: assembled_instructions(&request),
        history: request.history,
        base: request.base,
        guidance: request.guidance,
        schemas: request.schemas,
        contributions: &contributions,
        compaction: &compaction,
        max_bytes: request.max_bytes,
        turn: 1,
        call: 2,
    };
    let bytes = webagent_context::accounting::json_size(&capture).unwrap();
    let value = serde_json::to_value(&capture).unwrap();
    assert_eq!(bytes, serde_json::to_vec(&value).unwrap().len());
    assert_eq!(value["history"], json!(history));
    assert_eq!(value["base"], "PRIVATE");
    assert_eq!(value["schemas"], json!(schemas));
    let invalid = anyhow::anyhow!("PRIVATE secret token");
    assert_eq!(
        webagent_context::assembly::failure_code(&invalid),
        ("assembly_internal", "assembly")
    );
}
