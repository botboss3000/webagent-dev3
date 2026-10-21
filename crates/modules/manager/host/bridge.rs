//! Root composition delegates existing owner operations without Manager decisions.
use super::*;
use webagent_manager::runtime::{Runtime,ReviewAuthority,SessionScope,Engine};
use webagent_models::{runtime::SessionAuthority,transport::Projection};
#[derive(Clone)] pub(super) struct Authority(App);
impl SessionAuthority for Authority {
 async fn live_access<'a>(&'a self,user:&'a str,session:&'a str,agent:&'a str)->Result<webagent_access::authority::LiveAccess> {
  let authority=crate::ModelSessionAuthority(&self.0.store);
  SessionAuthority::live_access(&authority,user,session,agent).await
 }
}
impl ReviewAuthority<Store> for Authority {
 async fn session_scope<'a>(&'a self,sid:&'a str,requester:&'a str)->Result<SessionScope<Store,Self>> {
  let mut actor=self.0.clone();actor.chat_actor=Some(requester.to_owned());
  let scoped=crate::shared_chat::scope(actor,sid).await?;
  let access=scoped.chat_access.as_ref().ok_or_else(||anyhow!("Review access missing"))?;
  let owner_id=access.owner_id.clone();let agent_id=access.agent_id.clone();let session=access.session_id.clone();let can_write=access.can_write();
  let actor=crate::shared_chat::actor(&scoped).to_owned();
  Ok(SessionScope{storage:scoped.store.clone(),user_pool:scoped.store.pool.clone(),agent_pool:scoped.store.agent_pool.clone(),
   owner_id,agent_id,actor,session,can_write,authority:Authority(scoped)})
 }
 async fn modules_binding<'a>(&'a self,run:&'a Value)->Result<()> {
  if !crate::modules::binding_valid(&self.0,run).await {return Err(anyhow!("Module admission revoked"));} Ok(())
 }
 fn session_operation<'a,T:Send+'a>(&'a self,sid:&'a str,cancel:CancellationToken,operation:impl std::future::Future<Output=Result<T>>+Send+'a)->impl std::future::Future<Output=Result<T>>+Send+'a {
  crate::shared_chat::guard_operation(&self.0.store,sid,cancel,operation)
 }
}
#[derive(Clone)] pub(super) struct EnginePort(PathBuf);
impl Projection for EnginePort {
 async fn project<'a>(&'a self,input:&'a webagent_provider_contracts::RequestV1,cancel:&'a CancellationToken)->Result<webagent_provider_contracts::ResultV1> {
  crate::engine_components::provider_project(&self.0,input,cancel).await
 }
}
impl Engine for EnginePort {
 fn snapshot(&self)->Result<(String,u64)> {Ok((crate::engine_components::catalog()?.revision.clone(),crate::engine_components::controls(&self.0)?.revision))}
 fn current(&self,snapshot:&(String,u64))->Result<bool> {
  Ok(crate::engine_components::catalog()?.revision==snapshot.0 && crate::engine_components::controls(&self.0)?.revision==snapshot.1
   && crate::engine_components::component_available(&self.0,crate::engine_components::PROJECTION))
 }
}
pub(super) type ManagerRuntime=Runtime<Store,Authority,EnginePort>;
pub(super) fn runtime(app:&App)->ManagerRuntime {
 Runtime {root:app.store.root.clone(),user_pool:app.store.pool.clone(),app_pool:app.store.app_pool.clone(),agent_pool:app.store.agent_pool.clone(),
  owner:app.store.current_user_id().to_owned(),agent:app.store.current_agent_id().to_owned(),actor:crate::shared_chat::actor(app).to_owned(),session:app.chat_access.as_ref().map(|a|a.session_id.clone()),
  storage:app.store.clone(),authority:Authority(app.clone()),engine:EnginePort(app.store.root.clone()),provider:app.provider.transport().clone(),
  gate:app.gate.clone(),refresh:app.refresh.clone(),login:app.login.clone(),stop:app.stop.clone(),ready:app.ready.clone()}
}
#[derive(Clone)]pub(super) struct Owners(pub App);
impl webagent_manager::scheduler::OwnerSources<Store,Authority,EnginePort> for Owners {
 async fn users(&self)->Result<Vec<ManagerRuntime>> {Ok(crate::user_apps(&self.0).await?.iter().map(runtime).collect())}
}
