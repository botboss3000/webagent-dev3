//! Engine-owned execution handles. Root roles supply existing resources and distinct owner adapters.
use anyhow::{anyhow,Result};
use serde_json::{json,Value};
use std::{path::PathBuf,sync::{Arc,atomic::AtomicBool},collections::HashMap};
use tokio::sync::{Mutex,RwLock,Semaphore};
use tokio_util::sync::CancellationToken;
use crate::ports::*;
use self::ports::*;
pub mod config;
pub mod policy;
pub mod actions;
pub mod models;
pub mod native;
pub mod failure;
pub mod background;
pub mod background_followup;
pub mod run;
pub mod batch;
pub mod recovery;
pub mod scheduler;
pub mod watchdog;
pub mod followup;
pub mod components;
pub mod ports;
pub mod scope;
pub use scope::Scope;
/// These are the existing shared identities, created once by root composition.
#[derive(Clone)]
pub struct Handles<C:config::SchemaCatalog> {
 pub control_dir:PathBuf,
 pub runtime:Arc<RwLock<config::Config<C>>>,pub runtime_path:PathBuf,pub compaction_config_path:PathBuf,
 pub gate:Arc<Mutex<()>>,pub active:Arc<Mutex<HashMap<String,CancellationToken>>>,
 pub slots:Arc<Semaphore>,pub ready:Arc<AtomicBool>,pub stop:CancellationToken,
}
#[derive(Clone)]
pub struct Runtime<S,C:config::SchemaCatalog,B,A,H:ChatAuthority<S>,M,X> {
 pub(crate) handles:Handles<C>,pub(crate) store:Scope<S>,pub(crate) abilities:Arc<B>,
 pub(crate) provider:crate::model_client::Provider,pub(crate) refresh:Arc<Mutex<()>>,
 pub(crate) login:Arc<RwLock<Option<Value>>>,pub(crate) chat_actor:Option<String>,pub(crate) chat_access:Option<H::Binding>,
 pub(crate) actions:A,pub(crate) chat:H,pub(crate) modules:M,pub(crate) native:X,
 pub(crate) tool_socket:PathBuf,pub(crate) build_socket:PathBuf,
}
impl<S,C:config::SchemaCatalog,B,A,H:ChatAuthority<S>,M,X> std::ops::Deref for Runtime<S,C,B,A,H,M,X> {type Target=Handles<C>;fn deref(&self)->&Self::Target{&self.handles}}
impl<S:ScopedStorage+ChatEffects+AdmissionEffects+NotificationEffects+UsageEffects+RunObservation+RunPresentation,C:config::SchemaCatalog,B,A,H:ChatAuthority<S>,M,X> Runtime<S,C,B,A,H,M,X> {

 pub async fn run(&self,id:&str)->Result<Value>{self.store.get(&format!("run:{id}")).await?.ok_or_else(||anyhow!("Run not found"))}
 pub async fn shutoff(&self)->Result<bool>{Ok(self.store.get("control").await?.unwrap_or(json!({"shutoff":true}))["shutoff"].as_bool().unwrap_or(true))}
 pub async fn save(&self,run:&mut Value)->Result<()> {
  if !matches!(run["status"].as_str(),Some("completed"|"cancelled"|"failed"))||run["updated_at"].is_null(){run["updated_at"]=json!(crate::now());}
  let public=self.store.records().present_run(run);self.store.save_run_observed(run,"run_status",&public).await
 }
 pub async fn save_override(&self,run:&mut Value)->Result<()> {
  run["updated_at"]=json!(crate::now());let public=self.store.records().present_run(run);
  self.store.save_run_override_observed(run,"run_status",&public).await
 }
 pub fn session_operation<'a,T:Send+'a,F:std::future::Future<Output=Result<T>>+Send+'a>(&'a self,sid:&'a str,cancel:CancellationToken,operation:F)->impl std::future::Future<Output=Result<T>>+Send+'a {
  self.chat.operation(self.store.records(),sid,cancel,operation)
 }

}

impl<S:ScopedStorage,C:config::SchemaCatalog,B:Sync,A,H:ChatAuthority<S>,M,X:native::NativeDispatch<S,C,B,H::Binding>> Runtime<S,C,B,A,H,M,X> {
 pub async fn call_raw(self,sid:String,run_id:String,generation:String,name:String,args:Value,pending_id:String,policy_version:u64,cancel:CancellationToken)->Result<Value> {
  let view=self.native_view();
  Box::pin(self.native.call(view,native::Call{sid,run_id,generation,name,args,pending_id,policy_version,cancel})).await
 }
}

impl<S:ScopedStorage+models::ModelRecords,C:config::SchemaCatalog,B,A,H:ChatAuthority<S>,M:ModuleBindings<S>,X:AgentPolicy<S>> Runtime<S,C,B,A,H,M,X> {
 fn models(&self)->models::ModelServices<'_,S,models::ModuleView<'_,M,S>,models::SessionView<'_,H,S>> {
  models::ModelServices::new(self.store.records(),self.provider.transport(),&self.gate,&self.refresh,&self.login,&self.stop,models::ModuleView(&self.modules,self.store.records()),models::SessionView(&self.chat,self.store.records()))
 }
 pub async fn pin_model_plan(&self,plan:&mut webagent_models::model_selection::SelectionPlan)->Result<()> {
  self.models().pin_model_plan(plan,self.chat_actor.as_deref().unwrap_or(self.store.records().current_user_id()),self.chat_access.as_ref().map(|a|a.session_id())).await
 }
 pub async fn run_candidate(&self,run:&Value)->Result<Option<webagent_models::model_selection::Candidate>> {self.models().run_candidate(run).await}
 pub async fn tokens_for(&self,selection:&webagent_models::provider::ProviderSelection,run:&Value)->Result<Value>{self.models().tokens_for(selection,run).await}
 pub async fn tokens_at(&self,reference:&str)->Result<Value>{self.models().tokens_at(reference).await}
 pub async fn selection(&self)->Result<webagent_models::provider::ProviderSelection>{self.native.selection(self.store.records()).await}
 pub async fn run_model_authorized(&self,run:&Value)->bool {
  match self.run_candidate(run).await {Ok(_)=>true,Err(error)=>{tracing::warn!(run_id=run["id"].as_str().unwrap_or("unknown"),stage=bounded_model_authorization_failure_stage(&error),"Pinned model authorization failed");false}}
 }
}
fn bounded_model_authorization_failure_stage(error: &anyhow::Error) -> &'static str {
    if error.downcast_ref::<serde_json::Error>().is_some()
        || error.chain().any(|cause| cause.is::<serde_json::Error>())
    {
        return "plan_decode";
    }
    if error.downcast_ref::<sqlx::Error>().is_some()
        || error.chain().any(|cause| cause.is::<sqlx::Error>())
        || error.downcast_ref::<std::io::Error>().is_some()
        || error.chain().any(|cause| cause.is::<std::io::Error>())
    {
        return "storage";
    }
    const KNOWN: &[(&str, &str)] = &[
        ("Legacy run has no pinned role; start a new message after configuring Standard", "plan_missing"),
        ("Invalid pinned model plan identity", "plan_identity"),
        ("Candidate missing", "candidate_missing"),
        ("Selection candidates exhausted", "candidate_missing"),
        ("Model entry removed", "candidate_missing"),
        ("Model connection removed", "candidate_missing"),
        ("Pinned model is no longer available to this caller", "model_grant"),
        ("Model grant revoked", "model_grant"),
        ("Candidate grant revoked", "candidate_grant"),
        ("Pinned model candidate is no longer authorized", "candidate_grant"),
        ("Model candidate is not authorized", "candidate_grant"),
        ("Platform provenance removed", "platform"),
        ("Platform provenance changed", "platform"),
        ("Platform provenance missing from plan", "platform"),
        ("Published model lacks role capability", "platform"),
        ("Selected model connection is unavailable", "candidate_grant"),
        ("Agent model roster required", "candidate_grant"),
        ("Invalid agent model roster", "candidate_grant"),
        ("Invalid or duplicate role backup candidates", "candidate_grant"),
        ("Standard model required", "model_grant"),
        ("Premium model required", "model_grant"),
        ("Image In model required", "model_grant"),
        ("Image Out model required", "model_grant"),
        ("System model required", "model_grant"),
        ("Voice model required", "model_grant"),
        ("Decision model required", "model_grant"),
        ("BYO model outside tier", "model_grant"),
        ("Adopted platform transport changed", "platform"),
        ("Selected model is outside agent roster", "candidate_grant"),
        ("Role entry missing", "candidate_grant"),
        ("Model connection missing", "candidate_grant"),
        ("Role model is incompatible or not authorized", "candidate_grant"),
        ("Account not found", "account"),
        ("Account identity conflict", "account"),
        ("Invalid access identity", "account"),
        ("Guest access is not enabled", "account"),
        ("Entitlement tier not found", "tier"),
        ("Tier identity conflict", "tier"),
        ("Invalid entitlement tier", "tier"),
        ("Invalid entitlement grant", "tier"),
        ("Membership identity or revision conflict", "membership"),
        ("Membership capability level is missing", "membership"),
        ("Agent directory entry missing", "agent_owner"),
        ("Agent owner conflict", "agent_owner"),
        ("Invalid agent metadata", "agent_owner"),
    ];
    for cause in error.chain() {
        // Compare only exact internal messages; never return or log message text.
        let message = cause.to_string();
        if let Some((_, stage)) = KNOWN.iter().find(|(known, _)| *known == message.as_str()) {
            return stage;
        }
    }
    "unknown"
}

impl<S,C:config::SchemaCatalog,B,A,H:ChatAuthority<S>,M,X> Runtime<S,C,B,A,H,M,X> {
 pub fn new(handles:Handles<C>,store:S,abilities:Arc<B>,provider:crate::model_client::Provider,refresh:Arc<Mutex<()>>,login:Arc<RwLock<Option<Value>>>,chat_actor:Option<String>,chat_access:Option<H::Binding>,actions:A,chat:H,modules:M,native:X,tool_socket:PathBuf,build_socket:PathBuf)->Self {
  Self{handles,store:Scope::new(store),abilities,provider,refresh,login,chat_actor,chat_access,actions,chat,modules,native,tool_socket,build_socket}
 }
}

impl<S,C:config::SchemaCatalog,B,A,H:ChatAuthority<S>,M,X> Runtime<S,C,B,A,H,M,X> {
 pub(crate) fn native_view(&self)->native::View<'_,S,C,B,H::Binding> {
  native::View {store:self.store.records(),catalog:&self.abilities,actor:self.chat_actor.as_deref(),binding:self.chat_access.as_ref(),handles:&self.handles,provider:&self.provider,refresh:&self.refresh,login:&self.login,tool_socket:&self.tool_socket,build_socket:&self.build_socket}
 }
}

impl<S:ScopedStorage,C:config::SchemaCatalog,B,A,H:ChatAuthority<S>,M,X:PromptSlots<S>> Runtime<S,C,B,A,H,M,X> {
 pub async fn assembled_agent_instructions(&self,store:&S,agent:&str)->Result<String> {
  crate::context_assembly::pinned_instructions(crate::provider::DEFAULT_INSTRUCTIONS,&self.native.prompt_slots(store,agent).await?)
 }
 pub fn actor(&self)->&str {self.chat_actor.as_deref().unwrap_or(self.store.records().current_user_id())}
}

impl<S,C:config::SchemaCatalog,B,A,H:ChatAuthority<S>,M,X> Runtime<S,C,B,A,H,M,X> {
 pub async fn current_workflow_policy(&self)->Result<crate::workflow::Policy> {config::Config::<C>::read(&self.runtime_path)?.policy()}
}
