//! Caller-Tx composition for Context-owned selection state.
use crate::{repository::Repository,ports::*};
use anyhow::Result;
use serde_json::Value;
pub use webagent_context::ledger::scope_key;
impl<'a,R:Records,H:ChatEffects,Q:AdmissionEffects,N:NotificationEffects,U:UsageEffects> Repository<'a,R,H,Q,N,U> {
 fn context_repository(&self)->webagent_context::ledger::Repository<'_,crate::context::bridge::RecordAdapter<'_,R>> {
  webagent_context::ledger::Repository::new(self.root,self.pool,self.agent_id,crate::context::bridge::RecordAdapter(self.records))
 }
 pub async fn prepare_context_assets_operation(&self,run:&mut Value)->Result<()> {self.context_repository().prepare_context_assets_operation(run,&crate::context::bridge::Planner).await}
 pub async fn context_assets(&self,sid:&str,agent:&str,requester:&str)->Result<webagent_assets_contracts::State> {self.context_repository().context_assets(sid,agent,requester).await}
 pub fn apply_context_assets_write<'s,'tx>(&'s self,tx:&'s mut sqlx::Transaction<'tx,sqlx::Sqlite>,run:&'s Value)->impl std::future::Future<Output=Result<()>>+Send+'s+use<'s,'tx,R,H,Q,N,U> where 'tx:'s {async move {self.context_repository().apply_context_assets_write(tx,run).await}}
 pub fn reset_context_asset_layouts<'s,'tx>(&'s self,tx:&'s mut sqlx::Transaction<'tx,sqlx::Sqlite>,sid:&'s str)->impl std::future::Future<Output=Result<()>>+Send+'s+use<'s,'tx,R,H,Q,N,U> where 'tx:'s {async move {self.context_repository().reset_context_asset_layouts(tx,sid).await}}
}
