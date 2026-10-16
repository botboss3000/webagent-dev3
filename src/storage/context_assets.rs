//! Host-owned encrypted conversation selections and transactional placement metadata.
//! Components compute plans before writer admission; only fenced Store commits apply them.
use super::*;
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
pub(crate) fn scope_key(agent:&str,requester:&str)->String {crate::digest(&json!([agent,requester]))}

/// Reject corrupt namespace or schema data instead of silently discarding permanent selections.
fn validate_ledger(ledger:&Ledger,sid:&str)->Result<()> {
 if serde_json::to_vec(ledger)?.len()>webagent_assets_contracts::MAX_BYTES||ledger.version!=1||ledger.session_id!=sid||ledger.scopes.len()>256 {return Err(anyhow!("Invalid context assets ledger"));}
 for (scope,state) in &ledger.scopes {
  if scope.len()!=64||!scope.bytes().all(|byte|byte.is_ascii_hexdigit()) {return Err(anyhow!("Invalid context assets scope"));}
  validate_state(state)?;
 }
 Ok(())
}

impl Store {
 /// Resolve a staged native operation before opening the checkpoint writer transaction.
 pub(crate) async fn prepare_context_assets_operation(&self,run:&mut Value)->Result<()> {
  Box::pin(self.prepare_context_assets_operation_inner(run)).await
 }

 /// Preserve original expected revisions and stable IDs; pure service failures never write state.
 async fn prepare_context_assets_operation_inner(&self,run:&mut Value)->Result<()> {
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
  let plan=crate::engine_components::assets_plan(&self.root,&request,&tokio_util::sync::CancellationToken::new()).await?;
  webagent_assets_contracts::validate_plan(&request,&plan)?;
  let webagent_assets_contracts::Plan::Transition{state,..}=plan else {return Err(anyhow!("Context assets plan kind"));};
  run["_context_assets_write"]=json!({"scope":scope_key(&agent,&requester),"expected_revision":revision,"state":state});
  Ok(())
 }

 /// Read exact scope state without creating records or inferring grants from selections.
 pub(crate) async fn context_assets(&self,sid:&str,agent:&str,requester:&str)->Result<State> {
  Box::pin(self.context_assets_inner(sid,agent,requester)).await
 }

 /// Keep the public future small and read session/ledger from one consistent snapshot.
 async fn context_assets_inner(&self,sid:&str,agent:&str,requester:&str)->Result<State> {
  if !valid_id(sid)||!valid_id(agent)||!valid_id(requester)||agent!=self.agent_id {return Err(anyhow!("Invalid context assets identity"));}
  let mut tx=self.pool.begin().await?;
  let raw=sqlx::query_scalar::<_,Vec<u8>>("SELECT value FROM docs WHERE key=?")
   .bind(format!("session:{sid}")).fetch_optional(&mut *tx).await?.ok_or_else(||anyhow!("Context assets session missing"))?;
  let session=self.unseal(&raw)?;
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
   Some(raw)=>serde_json::from_value(self.unseal(&raw)?)?,
   None=>Ledger{version:1,session_id:sid.into(),scopes:BTreeMap::new()},
  };
  validate_ledger(&ledger,sid)?;Ok(ledger)
 }

 /// Apply only a run-derived scope after run/lease/participation fences have passed.
 pub(super) async fn apply_context_assets_write(&self,tx:&mut sqlx::Transaction<'_,sqlx::Sqlite>,run:&Value)->Result<()> {
  Box::pin(self.apply_context_assets_write_inner(tx,run)).await
 }

 /// CAS the candidate snapshot; stale plans cannot resurrect an unloaded selector.
 async fn apply_context_assets_write_inner(&self,tx:&mut sqlx::Transaction<'_,sqlx::Sqlite>,run:&Value)->Result<()> {
  let Some(value)=run.get("_context_assets_write").filter(|value|!value.is_null()) else {return Ok(());};
  let stage:Staged=serde_json::from_value(value.clone())?;
  let sid=run["session_id"].as_str().ok_or_else(||anyhow!("Context assets session missing"))?;
  let agent=run["agent_id"].as_str().unwrap_or("shared_default");
  let requester=run["requester_user_id"].as_str().unwrap_or(self.current_user_id());
  if !valid_id(sid)||!valid_id(agent)||!valid_id(requester)||agent!=self.agent_id||stage.scope!=scope_key(agent,requester) {
   return Err(anyhow!("Context assets scope changed"));
  }
  validate_state(&stage.state)?;
  let mut ledger=self.context_assets_ledger(tx,sid).await?;
  let current=ledger.scopes.get(&stage.scope).cloned().unwrap_or_default();
  if current.revision!=stage.expected_revision {return Err(anyhow!("Context assets revision changed"));}
  if stage.state==current {return Ok(());}
  if Some(stage.state.revision)!=current.revision.checked_add(1) {return Err(anyhow!("Invalid context assets next revision"));}
  if !ledger.scopes.contains_key(&stage.scope)&&ledger.scopes.len()>=256 {return Err(anyhow!("Context assets scope capacity reached"));}
  ledger.scopes.insert(stage.scope,stage.state);
  validate_ledger(&ledger,sid)?;
  self.write_document(tx,&format!("session_context_assets:{sid}"),&serde_json::to_value(ledger)?).await
 }

 /// Compaction and Undo retain selections/receipts while invalidating every scope's layout.
 pub(super) async fn reset_context_asset_layouts(&self,tx:&mut sqlx::Transaction<'_,sqlx::Sqlite>,sid:&str)->Result<()> {
  Box::pin(self.reset_context_asset_layouts_inner(tx,sid)).await
 }

 /// Reset only changed scopes in the existing lifecycle writer; no service runs under this lock.
 async fn reset_context_asset_layouts_inner(&self,tx:&mut sqlx::Transaction<'_,sqlx::Sqlite>,sid:&str)->Result<()> {
  let mut ledger=self.context_assets_ledger(tx,sid).await?;let mut changed=false;
  for state in ledger.scopes.values_mut() {
   if !state.placements.is_empty()||!state.schema_order.is_empty()||!state.history_basis.is_empty() {
    state.placements.clear();state.schema_order.clear();state.history_basis.clear();
    state.revision=state.revision.checked_add(1).ok_or_else(||anyhow!("Context assets revision exhausted"))?;
    changed=true;
   }
  }
  if changed {validate_ledger(&ledger,sid)?;self.write_document(tx,&format!("session_context_assets:{sid}"),&serde_json::to_value(ledger)?).await?;}
  Ok(())
 }
}

#[cfg(test)]
mod tests {
 use super::*;
 use webagent_assets_contracts::Placement;
 /// Build an isolated real Store and an active canonical conversation.
 async fn fixture()->(tempfile::TempDir,Store,Value) {
  let root=tempfile::tempdir().unwrap();let store=Store::open(root.path()).await.unwrap();
  store.put("session:s",&json!({"id":"s","agent_id":"shared_default"})).await.unwrap();
  let mut run=json!({"id":"r","session_id":"s","agent_id":"shared_default","status":"queued","run_revision":0,
   "lease_epoch":0,"generation":"fixture","input":[]});
  store.save_run(&mut run,"run_status",&json!({})).await.unwrap();(root,store,run)
 }
 /// Produce a valid candidate independently of model/tool text.
 fn candidate()->State {
  State{revision:1,selectors:vec![json!({"type":"tool","tool_name":"file_read"})],
   placements:vec![Placement{id:"main".into(),after_input:0,anchor:None}],
   schema_order:vec!["file_read".into()],history_basis:"fixture".into(),..Default::default()}
 }
 /// Stage a host-selected scope exactly as the working-loop bridge does.
 fn stage(run:&mut Value,requester:&str,expected:u64,state:&State) {
  run["requester_user_id"]=json!(requester);
  run["_context_assets_write"]=json!({"scope":scope_key("shared_default",requester),"expected_revision":expected,"state":state});
 }
 /// A successful fence seals state and removes private staging from both run copies.
 #[tokio::test]
 async fn fenced_write_is_sealed_scoped_and_strips_staging() {
  let (_root,store,mut run)=fixture().await;stage(&mut run,"admin",0,&candidate());
  store.save_run(&mut run,"run_status",&json!({})).await.unwrap();
  assert!(run.get("_context_assets_write").is_none());
  assert!(store.get("run:r").await.unwrap().unwrap().get("_context_assets_write").is_none());
  assert_eq!(store.context_assets("s","shared_default","admin").await.unwrap(),candidate());
  assert_eq!(store.context_assets("s","shared_default","participant").await.unwrap(),State::default());
  let raw:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key='session_context_assets:s'").fetch_one(&store.pool).await.unwrap();
  assert!(!String::from_utf8_lossy(&raw).contains("file_read"));
 }
 /// Rejected CAS leaves canonical state and caller staging intact for bounded reprepare.
 #[tokio::test]
 async fn stale_stage_never_restores_old_selection() {
  let (_root,store,mut run)=fixture().await;stage(&mut run,"admin",0,&candidate());
  store.save_run(&mut run,"run_status",&json!({})).await.unwrap();
  stage(&mut run,"admin",0,&candidate());
  let error=store.save_run(&mut run,"run_status",&json!({})).await.unwrap_err();
  assert_eq!(error.to_string(),"Context assets revision changed");
  assert!(run.get("_context_assets_write").is_some());
  assert_eq!(store.context_assets("s","shared_default","admin").await.unwrap().revision,1);
 }
 /// A later tool-result failure rolls back the ledger and preserves the uncommitted stage.
 #[tokio::test]
 async fn failed_tool_result_rolls_back_asset_effect() {
  let (_root,store,mut run)=fixture().await;stage(&mut run,"admin",0,&candidate());
  run["input"]=json!([{"type":"function_call_output","output":"missing call id"}]);
  assert!(store.save_run(&mut run,"tool_result",&json!({})).await.is_err());
  assert!(run.get("_context_assets_write").is_some());
  assert_eq!(store.context_assets("s","shared_default","admin").await.unwrap(),State::default());
  assert!(store.get("run:r").await.unwrap().unwrap().get("_context_assets_write").is_none());
 }
 /// Compaction rebases all requester layouts atomically without dropping their selections.
 #[tokio::test]
 async fn compaction_resets_every_scope_and_is_idempotent() {
  let (_root,store,mut run)=fixture().await;
  for actor in ["admin","participant"] {
   stage(&mut run,actor,0,&candidate());store.save_run(&mut run,"run_status",&json!({})).await.unwrap();
  }
  let source=json!([{"role":"user","content":"history"}]);run["input"]=json!([{"role":"assistant","content":"summary"}]);
  run["compactions"]=json!(1);
  store.save_run(&mut run,"context_compacted",&json!({"source":source,"source_digest":crate::digest(&source),"dropped_items":1,"segment_id":"r-1"})).await.unwrap();
  for actor in ["admin","participant"] {
   let state=store.context_assets("s","shared_default",actor).await.unwrap();
   assert_eq!(state.revision,2);assert_eq!(state.selectors,candidate().selectors);
   assert!(state.placements.is_empty()&&state.schema_order.is_empty()&&state.history_basis.is_empty());
  }
  let mut tx=store.pool.begin().await.unwrap();store.reset_context_asset_layouts(&mut tx,"s").await.unwrap();tx.commit().await.unwrap();
  assert_eq!(store.context_assets("s","shared_default","admin").await.unwrap().revision,2);
 }
 /// The real linked planner prepares effects before writer admission and retains exact retry fences.
 #[cfg(feature="linked-context")]
 #[tokio::test]
 async fn staged_native_operations_commit_once_and_cannot_resurrect_unload() {
  use webagent_assets_contracts::Operation;
  let (_root,store,mut run)=fixture().await;
  let selected=json!({"type":"tool","tool_name":"file_read"});
  run["requester_user_id"]=json!("admin");
  let activate=Operation::Activate{selectors:vec![selected.clone()],expected_revision:0,request_id:"load-1".into()};
  run["_context_assets_operation"]=json!(activate);
  run["input"]=json!([{"type":"function_call_output","output":"missing call id"}]);
  assert!(store.save_run(&mut run,"tool_result",&json!({})).await.is_err());
  assert!(run.get("_context_assets_operation").is_some());
  assert_eq!(store.context_assets("s","shared_default","admin").await.unwrap(),State::default());
  run["input"]=json!([]);
  store.save_run(&mut run,"run_status",&json!({})).await.unwrap();
  assert!(run.get("_context_assets_operation").is_none());
  assert_eq!(run["context_assets_revision"],1);
  let first=store.context_assets("s","shared_default","admin").await.unwrap();
  assert_eq!(first.selectors,vec![selected.clone()]);assert_eq!(first.receipts.len(),1);
  run["_context_assets_operation"]=json!(Operation::Unload{selector:selected.clone(),expected_revision:1,request_id:"unload-1".into()});
  store.save_run(&mut run,"run_status",&json!({})).await.unwrap();
  assert_eq!(run["context_assets_revision"],2);
  assert!(store.context_assets("s","shared_default","admin").await.unwrap().selectors.is_empty());
  // Replaying the original load receipt reports its effect without restoring it.
  run["_context_assets_operation"]=json!(activate);
  store.save_run(&mut run,"run_status",&json!({})).await.unwrap();
  assert!(store.context_assets("s","shared_default","admin").await.unwrap().selectors.is_empty());
  run["_context_assets_operation"]=json!(Operation::Activate{selectors:vec![selected],expected_revision:0,request_id:"stale-load".into()});
  assert_eq!(store.save_run(&mut run,"run_status",&json!({})).await.unwrap_err().to_string(),"Context assets revision changed");
  assert!(run.get("_context_assets_operation").is_some());
 }

 /// Stop bypasses an unavailable planner and discards uncommitted selection effects only on success.
 #[tokio::test]
 async fn terminal_override_never_waits_for_planner_or_commits_pending_assets() {
  let (_root,store,mut run)=fixture().await;
  stage(&mut run,"admin",0,&candidate());
  run["_context_assets_operation"]=json!(webagent_assets_contracts::Operation::Activate{
   selectors:vec![json!({"type":"tool","tool_name":"file_read"})],expected_revision:0,request_id:"pending-load".into()});
  std::fs::create_dir_all(store.root.join("config")).unwrap();
  std::fs::write(store.root.join("config/engine-components.json"),br#"{"revision":1,"disabled":["context-assets"]}"#).unwrap();
  run["status"]=json!("stopped");
  let mut stale=run.clone();stale["run_revision"]=json!(999);
  assert!(store.save_run_override(&mut stale,"run_status",&json!({})).await.is_err());
  assert!(stale.get("_context_assets_operation").is_some()&&stale.get("_context_assets_write").is_some());
  store.save_run_override(&mut run,"run_status",&json!({})).await.unwrap();
  assert!(run.get("_context_assets_operation").is_none()&&run.get("_context_assets_write").is_none());
  assert_eq!(store.context_assets("s","shared_default","admin").await.unwrap(),State::default());
  let saved=store.get("run:r").await.unwrap().unwrap();
  assert_eq!(saved["status"],"stopped");assert!(saved.get("_context_assets_operation").is_none());
 }

 /// Model-selected scope keys and skipped revisions are never accepted as staged host state.
 #[tokio::test]
 async fn rejects_wrong_scope_and_invalid_revision() {
  let (_root,store,mut run)=fixture().await;stage(&mut run,"admin",0,&candidate());
  run["_context_assets_write"]["scope"]=json!(scope_key("shared_default","participant"));
  assert!(store.save_run(&mut run,"run_status",&json!({})).await.is_err());
  let mut invalid=candidate();invalid.revision=9;stage(&mut run,"admin",0,&invalid);
  assert!(store.save_run(&mut run,"run_status",&json!({})).await.is_err());
  assert_eq!(store.context_assets("s","shared_default","admin").await.unwrap(),State::default());
 }
}
