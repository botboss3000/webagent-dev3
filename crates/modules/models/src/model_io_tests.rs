//! Real encrypted SQLite tests for Models-owned transaction and publication behavior.
use crate::{storage::{fixtures::{Fixture,byo_catalog},RuntimeStorage},account_catalog::{Adoption,Source,Origin},model_selection::Transport,platform_catalog::{Entry,Mutation}};
use serde_json::{json,Value};
use std::collections::{BTreeMap,BTreeSet};
use webagent_access::storage::RecordCodec;

#[tokio::test]
async fn first_catalog_write_is_revision_fenced_and_credentials_remain_separate() {
    let f=Fixture::new().await;let repo=f.repo();let catalog=byo_catalog();
    let secret=json!({"api_key":"synthetic-private-key"});
    f.storage.credential_put("provider_credential:synthetic",&secret).await.unwrap();
    let (one,two)=tokio::join!(repo.save_model_catalog(&catalog,0),repo.save_model_catalog(&catalog,0));
    assert_eq!(one.is_ok() as u8+two.is_ok() as u8,1);
    let saved=repo.model_catalog().await.unwrap();assert_eq!(saved.revision,1);
    assert!(!serde_json::to_string(&saved).unwrap().contains("synthetic-private-key"));
    let encrypted:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key='model_selection:catalog:v1'").fetch_one(&f.storage.app).await.unwrap();
    assert!(serde_json::from_slice::<Value>(&encrypted).is_err());
    assert_eq!(f.storage.credential_get("provider_credential:synthetic").await.unwrap().unwrap(),secret);
    let mut forged=saved.clone();forged.connections.get_mut("connection").unwrap().owner_id="admin".into();
    assert!(repo.save_model_catalog(&forged,saved.revision).await.is_err());
    assert_eq!(repo.model_catalog().await.unwrap(),saved);
}

#[tokio::test]
async fn adoption_rolls_back_every_record_and_replay_cannot_change_the_request() {
    let f=Fixture::new().await;let repo=f.repo();let saved=repo.save_model_catalog(&byo_catalog(),0).await.unwrap();
    let input=Adoption {client_request_id:"adopt-once".into(),expected_roster_revision:0,
        source:Source {origin:Origin::Byo,entry_id:"byo:model".into()},alias:"Chosen".into()};
    let original_rows:Vec<(String,Vec<u8>)>=sqlx::query_as("SELECT key,value FROM docs ORDER BY key").fetch_all(&f.storage.app).await.unwrap();
    sqlx::raw_sql("CREATE TRIGGER reject_adoption_receipt BEFORE INSERT ON events WHEN NEW.kind='model_adopted' BEGIN SELECT RAISE(ABORT,'test late event failure'); END;").execute(&f.storage.app).await.unwrap();
    assert!(repo.adopt_byo_source("alice","shared_default",&input).await.is_err());
    assert_eq!(repo.model_catalog().await.unwrap(),saved);
    let restored_rows:Vec<(String,Vec<u8>)>=sqlx::query_as("SELECT key,value FROM docs ORDER BY key").fetch_all(&f.storage.app).await.unwrap();
    assert_eq!(restored_rows,original_rows);
    let extras:i64=sqlx::query_scalar("SELECT COUNT(*) FROM docs WHERE key LIKE 'receipt:model-adoption:%' OR key LIKE 'model_adoption:v1:%'").fetch_one(&f.storage.app).await.unwrap();
    assert_eq!(extras,0);
    sqlx::query("DROP TRIGGER reject_adoption_receipt").execute(&f.storage.app).await.unwrap();
    let accepted=repo.adopt_byo_source("alice","shared_default",&input).await.unwrap();
    let stable=repo.model_catalog().await.unwrap();
    assert_eq!(accepted["roster_revision"],1);
    assert!(stable.agent_models["shared_default"].roles.is_empty());
    assert_eq!(repo.adopt_byo_source("alice","shared_default",&input).await.unwrap(),accepted);
    assert_eq!(repo.model_catalog().await.unwrap(),stable);
    let mut reused=input.clone();reused.alias="Different".into();
    assert!(repo.adopt_byo_source("alice","shared_default",&reused).await.is_err());
    assert_eq!(repo.model_catalog().await.unwrap(),stable);
}

#[tokio::test]
async fn platform_rotation_and_rollback_never_restore_a_cleared_secret() {
    let f=Fixture::new().await;let repo=f.repo();
    let entry=Entry {id:"model".into(),display_name:"Published".into(),upstream_model:"model-a".into(),
        transport:Transport::Deepseek,capabilities:BTreeSet::from(["text".into()])};
    repo.mutate_platform_roster("team",0,Mutation::Draft {name:"Team".into(),entries:BTreeMap::from([("model".into(),entry.clone())])},"admin").await.unwrap();
    let first=repo.platform_credential_set("team","model",1,Some("synthetic-key-one"),"admin").await.unwrap();
    repo.mutate_platform_roster("team",2,Mutation::Publish {rollback:None},"admin").await.unwrap();
    assert_eq!(repo.platform_tokens_pinned("team",&entry,&first).await.unwrap()["api_key"],"synthetic-key-one");
    let second=repo.platform_credential_set("team","model",3,Some("synthetic-key-two"),"admin").await.unwrap();
    assert_ne!(first,second);assert!(repo.platform_tokens_pinned("team",&entry,&first).await.is_err());
    repo.platform_credential_set("team","model",4,None,"admin").await.unwrap();
    repo.mutate_platform_roster("team",5,Mutation::Publish {rollback:Some(3)},"admin").await.unwrap();
    assert!(!repo.platform_credential_configured("team",&entry).await.unwrap());
    assert!(repo.platform_tokens_pinned("team",&entry,&second).await.is_err());
    let roster=repo.platform_roster("team").await.unwrap().unwrap();
    assert!(!serde_json::to_string(&roster).unwrap().contains("synthetic-key"));
}

#[tokio::test]
async fn live_model_source_and_dispatch_recheck_access_and_credential_generation() {
    let f=Fixture::new().await;let repo=f.repo();
    let entry=Entry {id:"model".into(),display_name:"Published".into(),upstream_model:"model-a".into(),
        transport:Transport::Deepseek,capabilities:BTreeSet::from(["text".into()])};
    repo.mutate_platform_roster("team",0,Mutation::Draft {name:"Team".into(),entries:BTreeMap::from([("model".into(),entry.clone())])},"admin").await.unwrap();
    repo.platform_credential_set("team","model",1,Some("synthetic-key-one"),"admin").await.unwrap();
    repo.mutate_platform_roster("team",2,Mutation::Publish {rollback:None},"admin").await.unwrap();
    let access=repo.access();let mut policy=access.read_access_policy().unwrap();
    policy["tiers"]["basic"]["model_policy"]=json!({"roster_id":"team","allowed_entry_ids":["model"],"allow_byo":false,"max_byo_entries":0});
    access.write_access_policy(&policy).unwrap();
    let source=Source {origin:Origin::Platform,entry_id:"platform:team:model".into()};
    let pin=repo.pin_platform_source("alice",&source).await.unwrap();
    assert_eq!(repo.platform_dispatch_tokens("alice","shared_default",&pin).await.unwrap()["api_key"],"synthetic-key-one");
    repo.platform_credential_set("team","model",3,Some("synthetic-key-two"),"admin").await.unwrap();
    assert!(repo.platform_dispatch_tokens("alice","shared_default",&pin).await.is_err());
    let fresh=repo.pin_platform_source("alice",&source).await.unwrap();
    assert!(repo.platform_dispatch_tokens("alice","shared_default",&fresh).await.is_ok());
    access.assign_agent_membership("admin","alice","shared_default",webagent_access::Relationship::Revoked,false,1).await.unwrap();
    assert!(repo.platform_dispatch_tokens("alice","shared_default",&fresh).await.is_err());
}
