//! Root compatibility for the actual Engine-owned context selection ledger.
use super::*;
#[cfg(test)] use webagent_assets_contracts::State;
pub(crate) use webagent_engine::context_ledger::scope_key;
impl Store {
 pub(crate) async fn prepare_context_assets_operation(&self,run:&mut Value)->Result<()> {self.engine_repository().prepare_context_assets_operation(run).await}
 pub(crate) async fn context_assets(&self,sid:&str,agent:&str,requester:&str)->Result<webagent_assets_contracts::State> {self.engine_repository().context_assets(sid,agent,requester).await}
 pub(super) async fn apply_context_assets_write(&self,tx:&mut sqlx::Transaction<'_,sqlx::Sqlite>,run:&Value)->Result<()> {self.engine_repository().apply_context_assets_write(tx,run).await}
 pub(super) async fn reset_context_asset_layouts(&self,tx:&mut sqlx::Transaction<'_,sqlx::Sqlite>,sid:&str)->Result<()> {self.engine_repository().reset_context_asset_layouts(tx,sid).await}
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
