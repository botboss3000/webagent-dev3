//! Engine current-owner authority and source composition, over Context-owned materialization.
use crate::{context_input,context_assembly,provider};
use crate::runtime::{Runtime,config,ports::*,models::ModelRecords};
use crate::ports::*;
use crate::context::abilities::Catalog;
use anyhow::{bail,Result};
use serde_json::{json,Value};
use tokio_util::sync::CancellationToken;
use webagent_assets_contracts::State;
use webagent_capability_contracts::{Guidance,PreparedCapability};
use webagent_provider_contracts::ProfileV1;

pub use webagent_context::assets::{Working,record_tool_operation};
impl<S:ScopedStorage+ModelRecords+ChatEffects+AdmissionEffects+NotificationEffects+UsageEffects+RunObservation+RunPresentation,C:config::SchemaCatalog,B:Catalog,A:crate::runtime::actions::Actions<S>+Clone+PartialEq,H:ChatAuthority<S>,M:ModuleBindings<S>,X:AgentPolicy<S>+NativePolicy+CredentialBindings<S>+SkillSources<S>+PromptSlots<S>+MainGuidance<S>+crate::runtime::native::NativeDispatch<S,C,B,H::Binding>> Runtime<S,C,B,A,H,M,X> {
pub fn asset_catalog(&self)->Result<B> {self.abilities.current()}
pub fn source_fence(&self)->Result<String> {let app=self;
 let fresh=app.asset_catalog()?;
 if fresh.version()!=app.abilities.version()||fresh.live_version()!=app.abilities.live_version() {
  bail!("Context assets source changed");
 }
 let base=provider::Provider::instructions_for_snapshot(provider::DEFAULT_INSTRUCTIONS,crate::ports::AgentMetadata::root(app.store.records()),fresh.source_root())?;
 let engines=crate::components::catalog()?;
 let controls=crate::components::controls(crate::ports::AgentMetadata::root(app.store.records()))?;
 Ok(crate::digest(&json!([app.abilities.version(),app.abilities.live_version(),
  fresh.version(),fresh.live_version(),base,engines.revision,controls])))
}
fn asset_identity(&self,run:&Value)->Result<(String,String,String)> {webagent_context::assets::identity(run,self.store.current_user_id())}
async fn asset_legacy_seed(&self,run:&mut Value,state:&State)->Result<bool> {
 if !webagent_context::assets::legacy_seed_needed(run,state){return Ok(false);}
 let (_sid,_agent,_requester)=self.asset_identity(run)?;
 let rid=run["id"].as_str().ok_or_else(||anyhow::anyhow!("Context assets run missing"))?.to_owned();
 let canonical=self.run(&rid).await?;
 let operation=webagent_context::assets::legacy_operation(run,&canonical,self.store.current_user_id())?;
 run["_context_assets_operation"]=json!(operation);self.save(run).await?;Ok(true)
}
pub async fn hydrate_context(&self,run:&mut Value)->Result<State> {let app=self;
 let (sid,agent,requester)=app.asset_identity(run)?;
 let scoped=app.store.for_agent(&agent).await?;
 let mut state=scoped.context_assets(&sid,&agent,&requester).await?;
 if app.asset_legacy_seed(run,&state).await? {state=scoped.context_assets(&sid,&agent,&requester).await?;}
 let fresh=app.asset_catalog()?;
 let (_,_,visibility,session_state)=app.ability_controls_for(&sid,&requester).await?;
 let changed=webagent_context::assets::hydrate_selectors(run,&state,X::tool_marker,|name,loaded|fresh.tool_abilities(name,&visibility,&session_state,loaded))?;
 if changed {app.save(run).await?;}
 Ok(state)
}
pub async fn context_fingerprint(&self,sid:&str,agent_id:&str,loaded:&Value,selected:&Value,
 skills:&Value,optimization:&Value,requester:&str)->Result<String> {let app=self;
 let fresh=app.asset_catalog()?;
 let access=app.chat.live_access(app.store.records(),requester,sid,agent_id).await?;
 let (policy,mode_revision,visibility,state)=app.ability_controls_for(sid,requester).await?;
 let mode=app.session_mode(sid).await?;
 let scoped=app.store.for_agent(agent_id).await?;
 let ledger=scoped.context_assets(sid,agent_id,requester).await?;
 let own=app.native.own_skills(scoped.for_user(requester).await?.records()).await?;
 let base=provider::Provider::instructions_for_snapshot(provider::DEFAULT_INSTRUCTIONS,crate::ports::AgentMetadata::root(app.store.records()),fresh.source_root())?;
 let slots=app.native.prompt_slots(scoped.records(),agent_id).await?;
 let main_chat=Box::pin(app.native.main_guidance(app.store.records(),agent_id,sid)).await?;
 let engines=crate::components::catalog()?;
 let controls=crate::components::controls(crate::ports::AgentMetadata::root(app.store.records()))?;
 Ok(crate::digest(&json!([fresh.version(),fresh.live_version(),policy,mode_revision,visibility,state,mode,
  loaded,selected,skills,optimization,access.context_fingerprint(),ledger.selectors,own["revision"],own,
  base,slots,main_chat,engines.revision,controls])))
}
pub async fn prepare_capability(&self,run:&Value,schemas:&[Value],guidance:Vec<Guidance>,
 menu:String,sourcefingerprint:&str,cancel:&CancellationToken)->Result<PreparedCapability> {let app=self;
 let (sid,agent,requester)=app.asset_identity(run)?;let scoped=app.store.for_agent(&agent).await?;
 let fresh=app.asset_catalog()?;
 let mut base_prompt=provider::Provider::instructions_for_snapshot(provider::DEFAULT_INSTRUCTIONS,crate::ports::AgentMetadata::root(app.store.records()),fresh.source_root())?;
 // Automatic agent skills belong with current base instructions, ahead of history.
 // They never become selected/transcript assets or accumulate across calls.
 if let Some(skill)=Box::pin(app.native.main_guidance(app.store.records(),&agent,&sid)).await? {
  base_prompt.push_str("\n\n");base_prompt.push_str(&skill);
 }
 let slots=app.native.prompt_slots(scoped.records(),&agent).await?;
 let live_slots=context_assembly::pinned_instructions("",&slots)?;
 let ledger=scoped.context_assets(&sid,&agent,&requester).await?;
 let request=webagent_context::assets::capability_request(base_prompt,live_slots,ledger,schemas,guidance,menu,sourcefingerprint)?;
 crate::components::capability_prepare(crate::ports::AgentMetadata::root(app.store.records()),&request,cancel).await
}
pub async fn project_context(&self,run:&mut Value,cap:&PreparedCapability,profile:&ProfileV1,
 schemas:&[Value],history:&[Value],contributions:&[context_input::Contribution],call:context_input::CallIdentity,cancel:&CancellationToken)->Result<Working> {
 let (sid,agent,requester)=self.asset_identity(run)?;let scoped=self.store.for_agent(&agent).await?;
 let ledger=scoped.context_assets(&sid,&agent,&requester).await?;
 let session=scoped.get(&format!("session:{sid}")).await?.ok_or_else(||anyhow::anyhow!("Context session missing"))?;
 webagent_context::assets::project_context(run,cap,profile,schemas,history,contributions,call,ledger,&session,&agent,&requester,crate::ports::AgentMetadata::root(self.store.records()),&crate::context::bridge::Planner,cancel).await
}
}
