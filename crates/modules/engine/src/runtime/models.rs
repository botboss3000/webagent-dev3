//! Engine call orchestration over actual Models services and the original owner admission fences.
use anyhow::{anyhow,Result};
use serde_json::Value;
use std::{future::Future,sync::Arc};
use tokio::sync::{Mutex,RwLock};
use tokio_util::sync::CancellationToken;
use webagent_models::{model_selection::{SelectionPlan,Candidate},provider::ProviderSelection,runtime::{Runtime,SessionAuthority},storage::{Repository,RuntimeStorage},transport::Provider};
use crate::ports::{Records,AgentMetadata};
/// Borrow the existing application/agent resources; keys, routing and codec stay with Storage.
pub trait ModelRecords:Records+AgentMetadata+RuntimeStorage {
 fn application_pool(&self)->&sqlx::SqlitePool;
 fn agent_id(&self)->&str;
}
/// Original Modules admission binding, rechecked at the same call points as root orchestration.
pub trait ModuleAuthority:Sync {
 fn binding_valid<'a>(&'a self,run:&'a Value)->impl Future<Output=bool>+Send+'a;
}
pub struct ModelServices<'a,S:ModelRecords,M,Q> {
 store:&'a S,
 provider:&'a Provider,
 gate:&'a Arc<Mutex<()>>,
 refresh:&'a Arc<Mutex<()>>,
 login:&'a Arc<RwLock<Option<Value>>>,
 stop:&'a CancellationToken,
 modules:M,
 authority:Q,
}
impl<'a,S:ModelRecords,M:ModuleAuthority,Q:SessionAuthority> ModelServices<'a,S,M,Q> {
 pub fn new(store:&'a S,provider:&'a Provider,gate:&'a Arc<Mutex<()>>,refresh:&'a Arc<Mutex<()>>,login:&'a Arc<RwLock<Option<Value>>>,stop:&'a CancellationToken,modules:M,authority:Q)->Self {
  Self{store,provider,gate,refresh,login,stop,modules,authority}
 }
 fn repository(&self)->Repository<'_,S> {
  Repository::new(AgentMetadata::root(self.store),ModelRecords::application_pool(self.store),AgentMetadata::agent_pool(self.store),ModelRecords::agent_id(self.store),Records::current_user_id(self.store),self.store)
 }
 fn models_runtime(&self)->Runtime<'_,S> {
  Runtime::new(self.repository(),self.provider,self.gate,self.refresh,self.login,self.stop)
 }
 /// The current model catalog of this store's application plane.
 pub async fn model_catalog(&self)->Result<webagent_models::model_selection::Catalog> {self.repository().model_catalog().await}
 pub async fn pin_model_plan(&self,plan:&mut SelectionPlan,actor:&str,session:Option<&str>)->Result<()> {
  self.models_runtime().pin_model_plan(plan,actor,session,&self.authority).await
 }
 pub async fn run_candidate(&self,run:&Value)->Result<Option<Candidate>> {
  if !self.modules.binding_valid(run).await {return Err(anyhow!("Module admission revoked"));}
  self.models_runtime().run_candidate(run,&self.authority).await
 }
 pub async fn tokens_for(&self,selection:&ProviderSelection,run:&Value)->Result<Value> {
  if !self.modules.binding_valid(run).await {return Err(anyhow!("Module admission revoked"));}
  self.models_runtime().tokens_for(selection,run,&self.authority).await
 }
 pub async fn tokens_at(&self,reference:&str)->Result<Value> {self.models_runtime().tokens_at(reference).await}
}

/// Short-lived adapters bind each existing authority call to the Runtime's current Store scope.
pub struct ModuleView<'a,M,S>(pub &'a M,pub &'a S);
impl<S:Sync,M:super::ports::ModuleBindings<S>> ModuleAuthority for ModuleView<'_,M,S> {
 fn binding_valid<'a>(&'a self,run:&'a Value)->impl Future<Output=bool>+Send+'a {self.0.binding_valid(self.1,run)}
}
pub struct SessionView<'a,H,S>(pub &'a H,pub &'a S);
impl<S:Sync,H:super::ports::ChatAuthority<S>> SessionAuthority for SessionView<'_,H,S> {
 fn live_access<'a>(&'a self,user:&'a str,session:&'a str,agent:&'a str)->impl Future<Output=Result<webagent_access::authority::LiveAccess>>+Send+'a {self.0.live_access(self.1,user,session,agent)}
}
