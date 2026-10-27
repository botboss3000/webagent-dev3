//! Models call admission, credential refresh and subscription authorization using existing shared handles.
use anyhow::{anyhow,Result};
use axum::Json;
use serde_json::{json,Value};
use std::{sync::Arc,time::{Duration,SystemTime,UNIX_EPOCH},path::PathBuf,future::Future};
use tokio::sync::{Mutex,RwLock};
use tokio_util::sync::CancellationToken;
use crate::{model_selection,account_catalog,provider,storage::{Repository,RuntimeStorage}};
pub trait SessionAuthority:Sync {
    fn live_access<'a>(&'a self,user:&'a str,session:&'a str,agent:&'a str)
        ->impl Future<Output=Result<webagent_access::authority::LiveAccess>>+Send+'a;
}
pub struct Runtime<'a,C:RuntimeStorage> {
    pub(crate) store:Repository<'a,C>,
    pub(crate) provider:&'a provider::Provider,
    pub(crate) gate:&'a Arc<Mutex<()>>,
    pub(crate) refresh:&'a Arc<Mutex<()>>,
    pub(crate) login:&'a Arc<RwLock<Option<Value>>>,
    pub(crate) stop:&'a CancellationToken,
}
fn now()->u64 {SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()}
impl<'a,C:RuntimeStorage> Runtime<'a,C> {
    pub fn new(store:Repository<'a,C>,provider:&'a provider::Provider,gate:&'a Arc<Mutex<()>>,
        refresh:&'a Arc<Mutex<()>>,login:&'a Arc<RwLock<Option<Value>>>,stop:&'a CancellationToken)->Self {
        Self {store,provider,gate,refresh,login,stop}
    }
    pub async fn tokens_at(&self, reference: &str) -> Result<Value> {
        let _lock = self.refresh.lock().await;
        let mut tokens = self
            .store
            .get(reference)
            .await?
            .ok_or_else(|| anyhow!("Connect your ChatGPT subscription first"))?;
        if tokens["expires_at"].as_f64().unwrap_or(0.) < now() as f64 + 60. {
            tokens = self.provider.refresh(&tokens).await?;
            self.store.put(reference, &tokens).await?;
        }
        Ok(tokens)
    }
    pub async fn tokens_for<A:SessionAuthority>(&self,selection:&provider::ProviderSelection,run:&Value,authority:&A)->Result<Value> {
        let candidate = self.run_candidate(run,authority).await?;
        if let Some(candidate) = &candidate {
            if selection.model != candidate.upstream_model || selection.provider != candidate.transport.provider_id() {
                return Err(anyhow!("Pinned model selection mismatch"));
            }
        }
        if let Some(pin)=candidate.as_ref().and_then(|candidate|candidate.platform.as_ref()) {
            return self.store.platform_dispatch_tokens(run["requester_user_id"].as_str().unwrap_or(self.store.current_user_id()),run["agent_id"].as_str().unwrap_or(""),pin).await;
        }
        let reference = candidate.as_ref().map(|c| c.credential_ref.as_str()).unwrap_or("provider_tokens");
        if selection.provider == provider::ProviderId::OpenaiSubscription {
            self.tokens_at(reference).await.map_err(|_| provider::typed_failure(model_selection::FailureKind::InvalidCredentials,
                false, "Assigned provider authorization unavailable; reconnect this connection"))
        } else {
            if candidate.is_none() { return Err(anyhow!("API providers require an explicit connection")); }
            let tokens = self.store.get(reference).await?.ok_or_else(|| provider::typed_failure(model_selection::FailureKind::InvalidCredentials, false, "Save an API key for this connection"))?;
            if tokens["api_key"].as_str().is_none_or(|s| s.is_empty()) { return Err(provider::typed_failure(model_selection::FailureKind::InvalidCredentials, false, "Provider API key unavailable")); }
            Ok(tokens)
        }
    }
    pub async fn run_candidate<A:SessionAuthority>(&self,run:&Value,authority:&A)->Result<Option<model_selection::Candidate>> {
        // Recovery cannot invent a role for an old run that never pinned one.
        let Some(value) = run.get("selection_plan").filter(|v| !v.is_null()) else {
            return Err(anyhow!("Legacy run has no pinned role; start a new message after configuring Standard"));
        };
        let plan: model_selection::SelectionPlan = serde_json::from_value(value.clone())?;
        let state: model_selection::FailoverState = serde_json::from_value(run["failover_state"].clone())?;
        if !matches!(plan.version,1|2) || plan.agent_id != run["agent_id"].as_str().unwrap_or("") {
            return Err(anyhow!("Invalid pinned model plan identity"));
        }
        let catalog = self.store.model_catalog().await?;
        // Membership, account ceiling and roster revocation remain live even for recovered plans.
        let candidate = plan.candidates.get(state.candidate_index).ok_or_else(||anyhow!("Candidate missing"))?;
        let requester=run["requester_user_id"].as_str().unwrap_or(self.store.current_user_id());
        let access=authority.live_access(requester,run["session_id"].as_str().unwrap_or(""),&plan.agent_id).await?;
        if !access.admitted() || (candidate.platform.is_none() && !access.model_allowed(&candidate.upstream_model)) {
            return Err(anyhow!("Model grant revoked"));
        }
        if catalog.agent_models.get(&plan.agent_id).is_none_or(|m|!m.entry_ids.contains(&candidate.entry_id)) {
            return Err(anyhow!("Candidate grant revoked"));
        }
        let candidate=plan.authorize_candidate(state.candidate_index, &catalog.entries, &catalog.connections)?.clone();
        // A bridge link runs on its owner's own Claude account, so it answers only the owner's chats.
        if candidate.transport==model_selection::Transport::ClaudeBridge
            && catalog.connections.get(&candidate.connection_id).is_none_or(|c|c.owner_id!=requester) {
            return Err(provider::typed_failure(model_selection::FailureKind::InvalidCredentials,false,
                "No Claude Bridge link of yours on this agent; add your own link"));
        }
        if let Some(pin)=&candidate.platform {
            let live=catalog.platform_sources.get(&candidate.entry_id).ok_or_else(||anyhow!("Platform provenance removed"))?;
            if live.roster_id!=pin.roster_id || live.entry.id!=pin.entry.id {return Err(anyhow!("Platform provenance changed"));}
            self.store.platform_dispatch_tokens(requester,&plan.agent_id,pin).await?;
        } else if catalog.platform_sources.contains_key(&candidate.entry_id) {return Err(anyhow!("Platform provenance missing from plan"));}
        Ok(Some(candidate))
    }
    pub async fn pin_model_plan<A:SessionAuthority>(&self,plan:&mut model_selection::SelectionPlan,actor:&str,session:Option<&str>,authority:&A)->Result<()> {
        for candidate in &mut plan.candidates {
            if let Some(previous)=&candidate.platform {
                let source=account_catalog::Source{origin:account_catalog::Origin::Platform,
                    entry_id:account_catalog::platform_id(&previous.roster_id,&previous.entry.id)};
                let pin=self.store.pin_platform_source(actor,&source).await?;
                if pin.entry.transport!=candidate.transport {return Err(anyhow!("Adopted platform transport changed"));}
                let capability=match plan.role {model_selection::ModelRole::ImageIn=>"image_in",model_selection::ModelRole::ImageOut=>"image_out",model_selection::ModelRole::Voice=>"voice",model_selection::ModelRole::Decision=>"decision",_=>"text"};
                if !pin.entry.capabilities.contains(capability) {return Err(anyhow!("Published model lacks role capability"));}
                candidate.upstream_model=pin.entry.upstream_model.clone();candidate.platform=Some(pin);
            } else if !match session {
                Some(session)=>authority.live_access(actor,session,&plan.agent_id).await?,
                None=>self.store.live_access(actor,&plan.agent_id).await?,
            }.model_allowed(&candidate.upstream_model) {
                return Err(anyhow!("BYO model outside tier"));
            }
        }
        Ok(())
    }
    pub async fn provider_start_for(&self,credential_ref:String,connection_id:Option<String>)->crate::api::Api {
        let app=self;

    let mut login = app.login.write().await;
    if login.as_ref().is_some_and(|v| v["status"] == "pending") {
        return Err(crate::api::conflict("Authorization already pending"));
    }
    let mut device = app.provider.device_start().await?;
    device["status"] = json!("pending");
    device["connection_id"] = json!(connection_id);
    let expires_at = device["expires_at"].as_u64().unwrap_or_else(|| now() + 900);
    let answer =
        json!({"verification_url":device["verification_url"],"user_code":device["user_code"],"expires_at":expires_at});
    *login = Some(device.clone());
    drop(login);
    let app=OwnedDeviceFlow::from_runtime(app);
    tokio::spawn(async move {
        let expires = expires_at;
        let interval = device["interval"].as_u64().unwrap_or(5).max(5);
        loop {
            tokio::select! {_=app.stop.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(interval))=>{}}
            let current = app.login.read().await.clone();
            if current.as_ref().is_none_or(|v| {
                v["device_auth_id"] != device["device_auth_id"] || v["status"] != "pending"
            }) {
                break;
            }
            if now() > expires {
                if let Some(v) = app.login.write().await.as_mut() {
                    v["status"] = json!("expired");
                }
                break;
            }
            match app.provider.device_poll(&device).await {
                Ok(Some(tokens)) => {
                    app.publish_device_tokens(&device,&credential_ref,connection_id.as_deref(),&tokens).await;
                    break;
                }
                Ok(None) => {}
                Err(_) => {
                    let mut login = app.login.write().await;
                    if let Some(v) = login.as_mut() {
                        if v["device_auth_id"] == device["device_auth_id"] {
                            v["status"] = json!("failed");
                        }
                    }
                    break;
                }
            }
        }
    });
    Ok(Json(answer))
    }
}

struct OwnedDeviceFlow<C:RuntimeStorage> {
    root:PathBuf,app_pool:sqlx::SqlitePool,agent_pool:sqlx::SqlitePool,agent_id:String,actor:String,storage:C,
    provider:provider::Provider,gate:Arc<Mutex<()>>,refresh:Arc<Mutex<()>>,
    login:Arc<RwLock<Option<Value>>>,stop:CancellationToken,
}
impl<C:RuntimeStorage> OwnedDeviceFlow<C> {
    fn from_runtime(app:&Runtime<'_,C>)->Self {
        Self {root:app.store.root.to_owned(),app_pool:app.store.app_pool.clone(),agent_pool:app.store.agent_pool.clone(),
            agent_id:app.store.agent_id.to_owned(),actor:app.store.actor.to_owned(),storage:app.store.storage.clone(),
            provider:app.provider.clone(),gate:app.gate.clone(),refresh:app.refresh.clone(),login:app.login.clone(),stop:app.stop.clone()}
    }
    fn repo(&self)->Repository<'_,C> {Repository::new(&self.root,&self.app_pool,&self.agent_pool,&self.agent_id,&self.actor,&self.storage)}
    async fn publish_device_tokens(&self,device:&Value,credential_ref:&str,connection_id:Option<&str>,tokens:&Value) {

                    // Serialize with catalog edits: revocation or replacing a connection
                    // during OAuth cannot authorize the changed record after the fact.
                    let _catalog_guard = self.gate.lock().await;
                    let allowed = match connection_id {
                        Some(id) => self.repo().model_catalog().await.ok().and_then(|c| c.connections.get(id).cloned())
                            .is_some_and(|c| !c.revoked && c.credential_ref == credential_ref),
                        None => true,
                    };
                    let _guard = self.refresh.lock().await;
                    let mut login = self.login.write().await;
                    if login.as_ref().is_some_and(|v| {
                        v["device_auth_id"] == device["device_auth_id"] && v["status"] == "pending"
                    }) {
                        if allowed && self.storage.credential_put(credential_ref,tokens).await.is_ok() {
                            if let Some(v) = login.as_mut() {
                                v["status"] = json!("connected");
                            }
                        } else if let Some(v) = login.as_mut() {
                            v["status"] = json!("failed");
                        }
                    }

    }

}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use crate::storage::fixtures::{Fixture,byo_catalog};
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn device_publication_rechecks_rotation_and_supersession_under_existing_gate() {
        let f=Fixture::new().await;let repo=f.repo();
        let mut catalog=byo_catalog();catalog.connections.get_mut("connection").unwrap().transport=model_selection::Transport::OpenaiSubscription;
        catalog.connections.get_mut("connection").unwrap().credential_ref="provider_credential:old".into();
        repo.save_model_catalog(&catalog,0).await.unwrap();
        let provider=provider::Provider::new();let gate=Arc::new(Mutex::new(()));let refresh=Arc::new(Mutex::new(()));
        let login=Arc::new(RwLock::new(Some(json!({"device_auth_id":"device-a","status":"pending","connection_id":"connection"}))));
        let stop=CancellationToken::new();let runtime=Runtime::new(f.repo(),&provider,&gate,&refresh,&login,&stop);
        let flow=OwnedDeviceFlow::from_runtime(&runtime);let device=json!({"device_auth_id":"device-a"});
        let tokens=json!({"access_token":concat!("synthetic-p","rivate-token")});
        let mut rotated=repo.model_catalog().await.unwrap();rotated.connections.get_mut("connection").unwrap().credential_ref="provider_credential:new".into();
        repo.save_model_catalog(&rotated,rotated.revision).await.unwrap();
        flow.publish_device_tokens(&device,"provider_credential:old",Some("connection"),&tokens).await;
        assert!(f.storage.credential_get("provider_credential:old").await.unwrap().is_none());
        assert_eq!(login.read().await.as_ref().unwrap()["status"],"failed");
        *login.write().await=Some(json!({"device_auth_id":"device-b","status":"pending","connection_id":"connection"}));
        flow.publish_device_tokens(&device,"provider_credential:new",Some("connection"),&tokens).await;
        assert!(f.storage.credential_get("provider_credential:new").await.unwrap().is_none());
        assert_eq!(login.read().await.as_ref().unwrap()["device_auth_id"],"device-b");
        let new_device=json!({"device_auth_id":"device-b"});
        flow.publish_device_tokens(&new_device,"provider_credential:new",Some("connection"),&tokens).await;
        assert_eq!(f.storage.credential_get("provider_credential:new").await.unwrap().unwrap(),tokens);
        assert_eq!(login.read().await.as_ref().unwrap()["status"],"connected");
        let bytes:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key='provider_credential:new'").fetch_one(&f.storage.app).await.unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("synthetic-private-token"));
    }

    #[tokio::test]
    async fn token_read_waits_for_the_same_refresh_mutex_before_storage_access() {
        let f=Fixture::new().await;let provider=provider::Provider::new();
        let gate=Arc::new(Mutex::new(()));let refresh=Arc::new(Mutex::new(()));let login=Arc::new(RwLock::new(None));
        let stop=CancellationToken::new();let runtime=Runtime::new(f.repo(),&provider,&gate,&refresh,&login,&stop);
        let tokens=json!({"access_token":concat!("synthetic-","fresh-token"),"expires_at":now()+3600});
        f.storage.credential_put("provider_credential:fresh",&tokens).await.unwrap();
        f.storage.reads.store(0,Ordering::Relaxed);
        let guard=refresh.lock().await;let pending=runtime.tokens_at("provider_credential:fresh");tokio::pin!(pending);
        assert!(tokio::time::timeout(Duration::from_millis(20),&mut pending).await.is_err());
        assert_eq!(f.storage.reads.load(Ordering::Relaxed),0);
        drop(guard);assert_eq!(pending.await.unwrap(),tokens);
        assert_eq!(f.storage.reads.load(Ordering::Relaxed),1);
    }
}
