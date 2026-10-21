//! Root composition delegates Models domain and persistence to its independently compiled package.
pub use webagent_models::account_catalog::*;
use crate::store::Store;
use anyhow::Result;
use serde_json::{json,Value};
use std::collections::{BTreeMap,BTreeSet};
impl Store {
    pub async fn save_account_model_preferences(&self,user:&str,input:&Preferences)->Result<Preferences> { self.models_repository().save_account_model_preferences(user, input).await }
    pub async fn account_model_choices(&self,user:&str)->Result<Value> { self.models_repository().account_model_choices(user).await }
    pub async fn account_model_preferences(&self,user:&str)->Result<Preferences> { self.models_repository().account_model_preferences(user).await }
    pub async fn resolve_account_model_source(&self,user:&str,source:&Source)->Result<ResolvedSource> { self.models_repository().resolve_account_model_source(user, source).await }
    pub async fn adopt_byo_source(&self,user:&str,agent:&str,input:&Adoption)->Result<Value> { self.models_repository().adopt_byo_source(user, agent, input).await }
    pub async fn seed_new_agent_default(&self,user:&str,agent:&str)->Result<bool> { self.models_repository().seed_new_agent_default(user, agent).await }
    pub async fn agent_model_adoptions(&self,user:&str,agent:&str)->Result<Value> { self.models_repository().agent_model_adoptions(user, agent).await }
    pub(crate) async fn platform_dispatch_tokens(&self,user:&str,agent:&str,pin:&PlatformPin)->Result<Value> { self.models_repository().platform_dispatch_tokens(user, agent, pin).await }
    pub(crate) async fn pin_platform_source(&self,user:&str,source:&Source)->Result<PlatformPin> { self.models_repository().pin_platform_source(user, source).await }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn no_policy_never_exposes_platform_and_forged_preferences_fail() {
        let (_root,app)=crate::runtime_tests::fixture().await;
        let choices=app.store.account_model_choices("admin").await.unwrap();
        assert_eq!(choices["available_platform_models"],json!([]));
        let mut preferences=Preferences::default();preferences.selected_platform_ids.insert("platform:base:forged".into());
        assert!(app.store.save_account_model_preferences("admin",&preferences).await.is_err());
        assert!(app.store.account_model_choices("unknown").await.is_err());
        let saved=app.store.save_account_model_preferences("admin",&Preferences::default()).await.unwrap();
        assert_eq!(saved.revision,1);
        assert!(app.store.save_account_model_preferences("admin",&Preferences::default()).await.is_err());
        let text=choices.to_string();assert!(!text.contains("credential_ref"));assert!(!text.contains("fixture-only"));
    }
}
#[cfg(test)]
mod source_tests {
    use super::*;
    #[tokio::test]
    async fn sources_rehydrate_live_identity_and_never_trust_client_transport() {
        let (_root,app)=crate::runtime_tests::fixture().await;
        let catalog=app.store.model_catalog().await.unwrap();
        let owned=catalog.entries.values().find(|e|e.owner_id=="admin").unwrap();
        let source=Source{origin:Origin::Byo,entry_id:byo_id(&owned.id)};
        match app.store.resolve_account_model_source("admin",&source).await.unwrap() {
            ResolvedSource::Byo{entry,connection}=>{
                assert_eq!(entry.id,owned.id);assert_eq!(connection.id,owned.connection_id);
            },_=>panic!("BYO source changed origin"),
        }
        assert!(app.store.resolve_account_model_source("unknown",&source).await.is_err());
        assert!(app.store.resolve_account_model_source("admin",&Source{origin:Origin::Platform,entry_id:source.entry_id}).await.is_err());
        assert!(serde_json::from_value::<Source>(json!({"origin":"byo","entry_id":"byo:owned","transport":"deepseek"})).is_err());
        assert!(serde_json::from_value::<Source>(json!({"origin":"byo","entry_id":"byo:owned","credential_ref":"forged"})).is_err());
    }
    #[tokio::test]
    async fn platform_resolution_requires_bound_publication_and_rechecks_retirement() {
        use crate::platform_catalog::{Entry,Mutation};
        let (_root,app)=crate::runtime_tests::fixture().await;
        let user=app.store.create_user_account("resolver_user","fixture-only-password-hash").await.unwrap();
        let user=user["user_id"].as_str().unwrap();
        app.store.update_account_access(user,1,crate::access::AccountStatus::Active,crate::access::Role::User,"basic").await.unwrap();
        let entry=Entry{id:"model".into(),display_name:"Published".into(),upstream_model:"deepseek-chat".into(),
            transport:crate::model_selection::Transport::Deepseek,capabilities:BTreeSet::from(["text".into()])};
        app.store.mutate_platform_roster("team",0,Mutation::Draft{name:"Team".into(),entries:BTreeMap::from([("model".into(),entry)])},"admin").await.unwrap();
        let source=Source{origin:Origin::Platform,entry_id:platform_id("team","model")};
        assert!(app.store.resolve_account_model_source(user,&source).await.is_err());
        let mut tier=app.store.entitlement_tier("basic").await.unwrap().unwrap();let revision=tier.revision;
        tier.model_policy=Some(crate::access::ModelPolicy{roster_id:Some("team".into()),allowed_entry_ids:BTreeSet::from(["model".into()]),allow_byo:false,max_byo_entries:0});
        app.store.save_entitlement_tier(tier,revision).await.unwrap();
        assert!(app.store.resolve_account_model_source(user,&source).await.is_err());
        app.store.mutate_platform_roster("team",1,Mutation::Publish{rollback:None},"admin").await.unwrap();
        match app.store.resolve_account_model_source(user,&source).await.unwrap() {
            ResolvedSource::Platform{roster_id,publication_revision,entry}=>{
                assert_eq!(roster_id,"team");assert_eq!(publication_revision,2);assert_eq!(entry.display_name,"Published");
            },_=>panic!("Platform source changed origin"),
        }
        app.store.mutate_platform_roster("team",2,Mutation::Retire,"admin").await.unwrap();
        assert!(app.store.resolve_account_model_source(user,&source).await.is_err());
    }
}
#[cfg(test)]
mod adoption_tests {
    use super::*;
    #[tokio::test]
    async fn byo_adoption_is_atomic_fenced_and_role_free() {
        let (_root,app)=crate::runtime_tests::fixture().await;
        let pool=crate::agent_storage::open_agent_pool(&app.store.root,"adoption_agent").await.unwrap();
        sqlx::query("INSERT INTO agents(id,name,description,provider,model,status,metadata,revision,created_at,updated_at) VALUES('adoption_agent','Adoption','','openai_subscription','','active',?,1,0,0)")
            .bind(app.store.seal(&json!({"owner_id":"admin"})).unwrap()).execute(&pool).await.unwrap();
        pool.close().await;
        app.store.register_agent("adoption_agent","admin").await.unwrap();
        let catalog=app.store.model_catalog().await.unwrap();
        let entry=catalog.entries.values().find(|entry|entry.owner_id=="admin").unwrap();
        let input=Adoption{client_request_id:"adopt_once".into(),expected_roster_revision:0,
            source:Source{origin:Origin::Byo,entry_id:byo_id(&entry.id)},alias:"Local alias".into()};
        let saved=app.store.adopt_byo_source("admin","adoption_agent",&input).await.unwrap();
        let catalog=app.store.model_catalog().await.unwrap();
        assert!(catalog.agent_models["adoption_agent"].roles.is_empty());
        assert_eq!(catalog.agent_models["adoption_agent"].entry_ids,vec![entry.id.clone()]);
        assert_eq!(catalog.entries[&entry.id].display_name,entry.display_name);
        let readback=app.store.agent_model_adoptions("admin","adoption_agent").await.unwrap();
        assert_eq!(readback[&entry.id]["alias"],"Local alias");
        assert!(!readback.to_string().contains("credential_ref"));
        assert!(!readback.to_string().contains("adopted_by"));
        assert!(app.store.agent_model_adoptions("unknown","adoption_agent").await.is_err());
        assert_eq!(app.store.adopt_byo_source("admin","adoption_agent",&input).await.unwrap(),saved);
        assert_eq!(app.store.model_catalog().await.unwrap(),catalog);
        let mut changed=input.clone();changed.alias="Different".into();
        assert!(app.store.adopt_byo_source("admin","adoption_agent",&changed).await.is_err());
        changed.client_request_id="new_request".into();
        assert!(app.store.adopt_byo_source("admin","adoption_agent",&changed).await.is_err());
        assert!(app.store.adopt_byo_source("unknown","adoption_agent",&input).await.is_err());
        assert!(serde_json::from_value::<Adoption>(json!({"client_request_id":"x","expected_roster_revision":0,
            "source":{"origin":"byo","entry_id":"byo:x"},"alias":"x","credential_ref":"forged"})).is_err());
    }
}
#[cfg(test)]
mod platform_pin_tests {
    use super::*;
    #[tokio::test]
    async fn platform_pin_rechecks_entitlement_and_key_generation() {
        use crate::platform_catalog::{Entry,Mutation};
        let (_root,app)=crate::runtime_tests::fixture().await;
        let user=app.store.create_user_account("pin_user","fixture-only-password-hash").await.unwrap();
        let user=user["user_id"].as_str().unwrap();
        app.store.update_account_access(user,1,crate::access::AccountStatus::Active,crate::access::Role::User,"basic").await.unwrap();
        app.store.set_agent_membership(user,"shared_default",crate::access::Relationship::Member,0).await.unwrap();
        let entry=Entry{id:"model".into(),display_name:"Pinned".into(),upstream_model:"deepseek-chat".into(),
            transport:crate::model_selection::Transport::Deepseek,capabilities:BTreeSet::from(["text".into()])};
        app.store.mutate_platform_roster("team",0,Mutation::Draft{name:"Team".into(),entries:BTreeMap::from([("model".into(),entry)])},"admin").await.unwrap();
        let source=Source{origin:Origin::Platform,entry_id:platform_id("team","model")};
        let mut tier=app.store.entitlement_tier("basic").await.unwrap().unwrap();let revision=tier.revision;
        tier.model_policy=Some(crate::access::ModelPolicy{roster_id:Some("team".into()),allowed_entry_ids:BTreeSet::from(["model".into()]),allow_byo:false,max_byo_entries:0});
        app.store.save_entitlement_tier(tier,revision).await.unwrap();
        app.store.platform_credential_set("team","model",1,Some("fixture-first-key"),"admin").await.unwrap();
        app.store.mutate_platform_roster("team",2,Mutation::Publish{rollback:None},"admin").await.unwrap();
        let pin=app.store.pin_platform_source(user,&source).await.unwrap();
        assert!(!serde_json::to_string(&pin).unwrap().contains("fixture-first-key"));
        assert_eq!(app.store.platform_dispatch_tokens(user,"shared_default",&pin).await.unwrap()["api_key"],"fixture-first-key");
        app.store.platform_credential_set("team","model",3,Some("fixture-second-key"),"admin").await.unwrap();
        assert!(app.store.platform_dispatch_tokens(user,"shared_default",&pin).await.is_err());
        let pin=app.store.pin_platform_source(user,&source).await.unwrap();
        app.store.set_agent_membership(user,"shared_default",crate::access::Relationship::Revoked,1).await.unwrap();
        assert!(app.store.platform_dispatch_tokens(user,"shared_default",&pin).await.is_err());
        app.store.set_agent_membership(user,"shared_default",crate::access::Relationship::Member,2).await.unwrap();
        app.store.mutate_platform_roster("team",4,Mutation::Retire,"admin").await.unwrap();
        assert!(app.store.platform_dispatch_tokens(user,"shared_default",&pin).await.is_err());
    }
}