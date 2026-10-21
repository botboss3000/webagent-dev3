//! Host-owned encrypted conversation selections and transactional placement metadata.
//! Components compute plans before writer admission; only fenced Store commits apply them.
use crate::ports::*;
use anyhow::{anyhow,Result};
use serde_json::{json,Value};
use serde::{Deserialize,Serialize};
use std::collections::BTreeMap;
use webagent_assets_contracts::{State,validate_state,valid_id};

#[derive(Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {version:u32,session_id:String,scopes:BTreeMap<String,State>}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Staged {scope:String,expected_revision:u64,state:State}

/// Bind one agent/requester pair inside the canonical conversation owner's user plane.
pub fn scope_key(agent:&str,requester:&str)->String {crate::digest(&json!([agent,requester]))}

/// Reject corrupt namespace or schema data instead of silently discarding permanent selections.
fn validate_ledger(ledger:&Ledger,sid:&str)->Result<()> {
 if serde_json::to_vec(ledger)?.len()>webagent_assets_contracts::MAX_BYTES||ledger.version!=1||ledger.session_id!=sid||ledger.scopes.len()>256 {return Err(anyhow!("Invalid context assets ledger"));}
 for (scope,state) in &ledger.scopes {
  if scope.len()!=64||!scope.bytes().all(|byte|byte.is_ascii_hexdigit()) {return Err(anyhow!("Invalid context assets scope"));}
  validate_state(state)?;
 }
 Ok(())
}

pub struct Repository<'a,R:Records> {pub root:&'a std::path::Path,pub pool:&'a sqlx::SqlitePool,pub agent_id:&'a str,pub records:R}
impl<'a,R:Records> Repository<'a,R> {
 pub fn new(root:&'a std::path::Path,pool:&'a sqlx::SqlitePool,agent_id:&'a str,records:R)->Self {Self{root,pool,agent_id,records}}
 fn current_user_id(&self)->&str {self.records.current_user_id()}
 /// Resolve a staged native operation before opening the checkpoint writer transaction.
 pub async fn prepare_context_assets_operation(&self,run:&mut Value,planner:&impl Planner)->Result<()> {
  Box::pin(self.prepare_context_assets_operation_inner(run,planner)).await
 }

 /// Preserve original expected revisions and stable IDs; pure service failures never write state.
 async fn prepare_context_assets_operation_inner(&self,run:&mut Value,planner:&impl Planner)->Result<()> {
  let Some(value)=run.get("_context_assets_operation").filter(|value|!value.is_null()) else {return Ok(());};
  let operation:webagent_assets_contracts::Operation=serde_json::from_value(value.clone())?;
  let sid=run["session_id"].as_str().ok_or_else(||anyhow!("Context assets session missing"))?.to_owned();
  let agent=run["agent_id"].as_str().unwrap_or("shared_default").to_owned();
  let requester=run["requester_user_id"].as_str().unwrap_or(self.current_user_id()).to_owned();
  let current=self.context_assets(&sid,&agent,&requester).await?;
  let (id,expected)=webagent_assets_contracts::operation_identity(&operation);
  let fingerprint=webagent_assets_contracts::digest(&operation)?;
  let receipt=current.receipts.iter().find(|receipt|receipt.request_id==id);
  if receipt.is_some_and(|receipt|receipt.fingerprint!=fingerprint) {
   return Err(anyhow!("Context assets request ID reused"));
  }
  if expected!=current.revision&&receipt.is_none() {return Err(anyhow!("Context assets revision changed"));}
  let revision=current.revision;
  let request=webagent_assets_contracts::Request::Transition{version:1,state:current,operation};
  let plan=planner.assets_plan(&self.root,&request,&tokio_util::sync::CancellationToken::new()).await?;
  webagent_assets_contracts::validate_plan(&request,&plan)?;
  let webagent_assets_contracts::Plan::Transition{state,..}=plan else {return Err(anyhow!("Context assets plan kind"));};
  run["_context_assets_write"]=json!({"scope":scope_key(&agent,&requester),"expected_revision":revision,"state":state});
  Ok(())
 }

 /// Read exact scope state without creating records or inferring grants from selections.
 pub async fn context_assets(&self,sid:&str,agent:&str,requester:&str)->Result<State> {
  Box::pin(self.context_assets_inner(sid,agent,requester)).await
 }

 /// Keep the public future small and read session/ledger from one consistent snapshot.
 async fn context_assets_inner(&self,sid:&str,agent:&str,requester:&str)->Result<State> {
  if !valid_id(sid)||!valid_id(agent)||!valid_id(requester)||agent!=self.agent_id {return Err(anyhow!("Invalid context assets identity"));}
  let mut tx=self.pool.begin().await?;
  let raw=sqlx::query_scalar::<_,Vec<u8>>("SELECT value FROM docs WHERE key=?")
   .bind(format!("session:{sid}")).fetch_optional(&mut *tx).await?.ok_or_else(||anyhow!("Context assets session missing"))?;
  let session=self.records.unseal(&raw)?;
  if session["status"]!="active"||session["agent_id"].as_str().unwrap_or("shared_default")!=agent {
   return Err(anyhow!("Context assets session unavailable"));
  }
  let ledger=self.context_assets_ledger(&mut tx,sid).await?;
  let state=ledger.scopes.get(&scope_key(agent,requester)).cloned().unwrap_or_default();
  tx.commit().await?;
  Ok(state)
 }

 /// Read sealed metadata using the caller's existing user transaction, never component IPC.
 async fn context_assets_ledger(&self,tx:&mut sqlx::Transaction<'_,sqlx::Sqlite>,sid:&str)->Result<Ledger> {
  let raw=sqlx::query_scalar::<_,Vec<u8>>("SELECT value FROM docs WHERE key=?")
   .bind(format!("session_context_assets:{sid}")).fetch_optional(&mut **tx).await?;
  let ledger=match raw {
   Some(raw)=>serde_json::from_value(self.records.unseal(&raw)?)?,
   None=>Ledger{version:1,session_id:sid.into(),scopes:BTreeMap::new()},
  };
  validate_ledger(&ledger,sid)?;Ok(ledger)
 }

 /// Apply only a run-derived scope after run/lease/participation fences have passed.
 pub fn apply_context_assets_write<'s,'tx>(&'s self,tx:&'s mut sqlx::Transaction<'tx,sqlx::Sqlite>,run:&'s Value)->impl std::future::Future<Output=Result<()>>+Send+'s+use<'s,'tx,R> where 'tx:'s {
  Box::pin(self.apply_context_assets_write_inner(tx,run))
 }

 /// CAS the candidate snapshot; stale plans cannot resurrect an unloaded selector.
 fn apply_context_assets_write_inner<'s,'tx>(&'s self,tx:&'s mut sqlx::Transaction<'tx,sqlx::Sqlite>,run:&'s Value)->impl std::future::Future<Output=Result<()>>+Send+'s+use<'s,'tx,R> where 'tx:'s {async move {
  let Some(value)=run.get("_context_assets_write").filter(|value|!value.is_null()) else {return Ok(());};
  let stage:Staged=serde_json::from_value(value.clone())?;
  let sid=run["session_id"].as_str().ok_or_else(||anyhow!("Context assets session missing"))?;
  let agent=run["agent_id"].as_str().unwrap_or("shared_default");
  let requester=run["requester_user_id"].as_str().unwrap_or(self.current_user_id());
  if !valid_id(sid)||!valid_id(agent)||!valid_id(requester)||agent!=self.agent_id||stage.scope!=scope_key(agent,requester) {
   return Err(anyhow!("Context assets scope changed"));
  }
  validate_state(&stage.state)?;
  let mut ledger=crate::send_future(self.context_assets_ledger(tx,sid)).await?;
  let current=ledger.scopes.get(&stage.scope).cloned().unwrap_or_default();
  if current.revision!=stage.expected_revision {return Err(anyhow!("Context assets revision changed"));}
  if stage.state==current {return Ok(());}
  if Some(stage.state.revision)!=current.revision.checked_add(1) {return Err(anyhow!("Invalid context assets next revision"));}
  if !ledger.scopes.contains_key(&stage.scope)&&ledger.scopes.len()>=256 {return Err(anyhow!("Context assets scope capacity reached"));}
  ledger.scopes.insert(stage.scope,stage.state);
  validate_ledger(&ledger,sid)?;
  crate::send_future(self.records.write_document(tx,&format!("session_context_assets:{sid}"),&serde_json::to_value(ledger)?)).await
 }
 }

 /// Compaction and Undo retain selections/receipts while invalidating every scope's layout.
 pub fn reset_context_asset_layouts<'s,'tx>(&'s self,tx:&'s mut sqlx::Transaction<'tx,sqlx::Sqlite>,sid:&'s str)->impl std::future::Future<Output=Result<()>>+Send+'s+use<'s,'tx,R> where 'tx:'s {
  Box::pin(self.reset_context_asset_layouts_inner(tx,sid))
 }

 /// Reset only changed scopes in the existing lifecycle writer; no service runs under this lock.
 fn reset_context_asset_layouts_inner<'s,'tx>(&'s self,tx:&'s mut sqlx::Transaction<'tx,sqlx::Sqlite>,sid:&'s str)->impl std::future::Future<Output=Result<()>>+Send+'s+use<'s,'tx,R> where 'tx:'s {async move {
  let mut ledger=crate::send_future(self.context_assets_ledger(tx,sid)).await?;let mut changed=false;
  for state in ledger.scopes.values_mut() {
   if !state.placements.is_empty()||!state.schema_order.is_empty()||!state.history_basis.is_empty() {
    state.placements.clear();state.schema_order.clear();state.history_basis.clear();
    state.revision=state.revision.checked_add(1).ok_or_else(||anyhow!("Context assets revision exhausted"))?;
    changed=true;
   }
  }
  if changed {validate_ledger(&ledger,sid)?;crate::send_future(self.records.write_document(tx,&format!("session_context_assets:{sid}"),&serde_json::to_value(ledger)?)).await?;}
  Ok(())
 }
 }
}
