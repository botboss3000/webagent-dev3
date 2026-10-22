//! Root compatibility for Engine-owned Context asset assembly and permanent selection staging.
use crate::*;
use webagent_assets_contracts::{State,Operation};
use webagent_capability_contracts::{Guidance,PreparedCapability};
use webagent_provider_contracts::ProfileV1;
pub use webagent_engine::context::assets::{Working,record_tool_operation};
pub(crate) fn catalog(app:&App)->Result<crate::abilities::Catalog> {app.engine().asset_catalog()}
pub(crate) fn source_fence(app:&App)->Result<String> {app.engine().source_fence()}
pub(crate) async fn hydrate(app:&App,run:&mut Value)->Result<State> {app.engine().hydrate_context(run).await}
pub(crate) async fn fingerprint(app:&App,sid:&str,agent_id:&str,loaded:&Value,selected:&Value,
 skills:&Value,optimization:&Value,requester:&str)->Result<String> {app.engine().context_fingerprint(sid,agent_id,loaded,selected,skills,optimization,requester).await}
pub(crate) async fn prepare_capability(app:&App,run:&Value,schemas:&[Value],guidance:Vec<Guidance>,
 menu:String,sourcefingerprint:&str,cancel:&CancellationToken)->Result<PreparedCapability> {app.engine().prepare_capability(run,schemas,guidance,menu,sourcefingerprint,cancel).await}
pub(crate) async fn project(app:&App,run:&mut Value,cap:&PreparedCapability,profile:&ProfileV1,
 schemas:&[Value],history:&[Value],contributions:&[context_input::Contribution],
 call:context_input::CallIdentity,cancel:&CancellationToken)->Result<Working> {app.engine().project_context(run,cap,profile,schemas,history,contributions,call,cancel).await}
#[cfg(test)]
mod tests {
 use super::*;
 /// Native output and changed current projections must agree before any activation is staged.

 /// Legacy identity conversion discards source pins and cannot import automatic ability IDs.


 /// Initialize a canonical queued run without a provider or worker; all state uses the real Store.
 #[cfg(feature="linked-context")]
 async fn bridge_fixture()->(tempfile::TempDir,App,Value) {
  let (root,app)=crate::runtime_tests::fixture().await;
  app.store.put("session:assets-bridge",&json!({"id":"assets-bridge","agent_id":"shared_default"})).await.unwrap();
  let mut run=json!({"id":"assets-run","session_id":"assets-bridge","agent_id":"shared_default",
   "status":"queued","run_revision":0,"lease_epoch":0,"generation":"fixture",
   "requester_user_id":app.store.current_user_id(),"input":[{"role":"user","content":"durable user input"}]});
  app.save(&mut run).await.unwrap();(root,app,run)
 }
 /// Resolve a real capability through the host bridge with a selected, untrusted file body.
 #[cfg(feature="linked-context")]
 async fn bridge_capability(app:&App,run:&Value)->PreparedCapability {
  prepare_capability(app,run,&[json!({"type":"function","name":"file_read","parameters":{"type":"object"}})],
   vec![Guidance{id:"fixture-skill".into(),kind:webagent_capability_contracts::GuidanceKind::Selectable,
    body:"file guidance sentinel".into(),selected:true,natural:false}],
   "temporary menu sentinel".into(),&"a".repeat(64),&CancellationToken::new()).await.unwrap()
 }
 /// Saved main prompt changes are resolved on each preparation, including the synchronous dispatch fence.
 #[cfg(feature="linked-context")]
 #[tokio::test]
 async fn preparation_rereads_current_main_prompt() {
  let (root,app,run)=bridge_fixture().await;
  std::fs::create_dir_all(root.path().join("config")).unwrap();
  let path=root.path().join("config/prompts.json");
  std::fs::write(&path,serde_json::to_vec(&json!({"base_instructions":"first prompt sentinel"})).unwrap()).unwrap();
  let first_fence=source_fence(&app).unwrap();
  let first=Box::pin(bridge_capability(&app,&run)).await;
  assert!(first.main.contains("first prompt sentinel"));
  std::fs::write(&path,serde_json::to_vec(&json!({"base_instructions":"second prompt sentinel"})).unwrap()).unwrap();
  let second=Box::pin(bridge_capability(&app,&run)).await;
  assert!(second.main.contains("second prompt sentinel"));
  assert!(!second.main.contains("first prompt sentinel"));
  assert_ne!(first_fence,source_fence(&app).unwrap());
 }
 /// Main Chat skills refresh with the base prompt, without modifying transcript documents.
 #[cfg(feature="linked-context")]
 #[tokio::test]
 async fn main_chat_skill_is_live_prefix_without_transcript_accumulation() {
  let (_root,app,run)=bridge_fixture().await;
  let sid=run["session_id"].as_str().unwrap();
  let before=app.store.get(&format!("session:{sid}")).await.unwrap();
  app.store.main_chat_change(0,"skill-prefix","skill-prefix",vec![],|state|{
   state["settings"]["enabled"]=json!(true);state["session_id"]=json!(sid);
   state["skill_overrides"]=json!({"contribute":"working sentinel","review":"review prefix sentinel"});Ok(Value::Null)
  }).await.unwrap();
  let first=Box::pin(bridge_capability(&app,&run)).await;
  assert!(first.main.contains("review prefix sentinel"));assert!(!first.main.contains("working sentinel"));
  app.store.main_chat_change(1,"skill-updated","skill-updated",vec![],|state|{
   state["skill_overrides"]["review"]=json!("updated prefix sentinel");Ok(Value::Null)
  }).await.unwrap();
  let second=Box::pin(bridge_capability(&app,&run)).await;
  assert!(second.main.contains("updated prefix sentinel"));assert!(!second.main.contains("review prefix sentinel"));
  assert_eq!(app.store.get(&format!("session:{sid}")).await.unwrap(),before);
 }
 /// Exercise activation, projection, compaction and unload through App/Store without persisting synthetic text.
 #[cfg(feature="linked-context")]
 #[tokio::test]
 async fn projected_guidance_stays_out_of_durable_history() {
  let (_root,app,mut run)=bridge_fixture().await;
  let selected=json!({"type":"tool","tool_name":"file_read"});
  run["context_assets_revision"]=json!(0);
  run["pending"]=json!({"name":"load_tool","call_id":"load-fixture","arguments":{"tool_name":"file_read"}});
  run["selected_tools"]=json!(["file_read"]);
  record_tool_operation(&mut run,&json!({"status":"success","output":{"activated":true,"selected_tools":["file_read"]}}),&json!([]),&json!([])).unwrap();
  run.as_object_mut().unwrap().remove("pending");
  app.save(&mut run).await.unwrap();
  let state=hydrate(&app,&mut run).await.unwrap();assert_eq!(state.selectors,vec![selected.clone()]);
  let cap=Box::pin(bridge_capability(&app,&run)).await;
  let profile:ProfileV1=serde_json::from_value(json!({"append_instructions":true,"instruction_role":"developer",
   "transport":"responses","reasoning_replay":"responses_items","cache_control":"implicit_prefix"})).unwrap();
  let original=run["input"].as_array().unwrap().clone();
  let schemas=vec![json!({"type":"function","name":"file_read","parameters":{"type":"object"}})];
  let working=Box::pin(project(&app,&mut run,&cap,&profile,&schemas,&original,&[],context_input::CallIdentity{turn:1,call:1},&CancellationToken::new())).await.unwrap();
  assert_eq!(working.input.len(),3);assert_eq!(working.input[1],original[0]);
  assert!(working.input[0]["content"].as_str().unwrap().contains("file guidance sentinel"));
  assert!(working.input[2]["content"].as_str().unwrap().contains("temporary menu sentinel"));
  assert_eq!(working.extra_json_bytes,serde_json::to_vec(&working.input).unwrap().len()-serde_json::to_vec(&original).unwrap().len());
  app.save(&mut run).await.unwrap();
  assert_eq!(run["input"],json!(original));
  assert_eq!(app.run("assets-run").await.unwrap()["input"],json!(original));
  let ledger=app.store.context_assets("assets-bridge","shared_default",app.store.current_user_id()).await.unwrap();
  assert_eq!(ledger.placements.len(),1);assert_eq!(ledger.placements[0].id,"fixture-skill");
  run["input"]=json!([{"role":"assistant","content":"canonical summary"}]);run["compactions"]=json!(1);
  app.store.save_run(&mut run,"context_compacted",&json!({"source":original,"source_digest":crate::digest(&original),"dropped_items":1,"segment_id":"assets-run-1"})).await.unwrap();
  let rebased=hydrate(&app,&mut run).await.unwrap();
  assert_eq!(rebased.selectors,vec![selected.clone()]);assert!(rebased.placements.is_empty());
  let operation=Operation::Unload{selector:selected.clone(),expected_revision:rebased.revision,request_id:"fixture-unload".into()};
  run["pending"]=json!({"name":"context_assets","call_id":"unload-fixture","arguments":{"action":"unload","selector":selected,"expected_revision":rebased.revision,"request_id":"fixture-unload"}});
  let tools=run["selected_tools"].clone();let skills=run["loaded_skills"].clone();
  record_tool_operation(&mut run,&json!({"status":"success","output":{"operation":operation}}),&tools,&skills).unwrap();
  run.as_object_mut().unwrap().remove("pending");app.save(&mut run).await.unwrap();
  let unloaded=hydrate(&app,&mut run).await.unwrap();assert!(unloaded.selectors.is_empty());
  assert_eq!(run["selected_tools"],json!([]));
  let saved=app.run("assets-run").await.unwrap();
  assert_eq!(saved["input"],json!([{"role":"assistant","content":"canonical summary"}]));
  assert!(saved.get("_context_assets_write").is_none()&&saved.get("_context_assets_operation").is_none());
 }
}
