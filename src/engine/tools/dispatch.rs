//! Root composition for Engine-owned dispatch; foreign handlers stay in owner adapters.
use crate::*;
pub(crate) async fn call(
    app:App,sid:String,run_id:String,generation:String,name:String,args:Value,pending_id:String,
    policy_version:u64,cancel:CancellationToken,
)->Result<tools::outcome::Outcome> {app.engine().call(sid,run_id,generation,name,args,pending_id,policy_version,cancel).await}
pub(crate) async fn call_foreground(
    app:App,sid:String,run_id:String,generation:String,name:String,args:Value,pending_id:String,
    policy_version:u64,cancel:CancellationToken,
)->Result<tools::outcome::Outcome> {app.engine().call_foreground(sid,run_id,generation,name,args,pending_id,policy_version,cancel).await}
pub(crate) async fn call_raw(
    app:App,sid:String,run_id:String,generation:String,name:String,args:Value,pending_id:String,
    policy_version:u64,cancel:CancellationToken,
)->Result<Value> {app.engine().call_raw(sid,run_id,generation,name,args,pending_id,policy_version,cancel).await}
async fn foreground_authorized(app:&App,sid:&str,run_id:&str,generation:&str,name:&str,args:&Value,pending_id:&str)->Result<bool> {app.engine().foreground_authorized(sid,run_id,generation,name,args,pending_id).await}

#[cfg(test)]
mod native_schema_dispatch_tests {
 use super::*;
 #[tokio::test]
 async fn stale_native_parameters_reject_at_real_foreground_fence_before_missing_run_lookup() {
  let (_state,mut app)=crate::runtime_tests::fixture_uninitialized().await;
  let source=crate::live_assets::native_schema_file_tests::source_fixture();
  let plugins=source.path().join("plugins/abilities");
  let snapshot=crate::abilities::Catalog::load_current(&plugins).unwrap();assert!(snapshot.native_snapshot_current().unwrap());
  app.abilities=Arc::new(snapshot);
  let sid=uuid::Uuid::new_v4().to_string();
  app.store.put(&key("session",&sid),&json!({"id":sid,"agent_id":"shared_default","status":"active"})).await.unwrap();
  assert!(app.store.get("run:missing-native-fixture").await.unwrap().is_none());
  let before=foreground_authorized(&app,&sid,"missing-native-fixture","unused","code_index_scan",&json!({"action":"discover"}),"unused").await.unwrap_err();
  assert_eq!(before.to_string(),"Run not found");
  crate::live_assets::native_schema_file_tests::change_parameter(source.path());
  assert!(!app.abilities.native_snapshot_current().unwrap());
  // The real guarded dispatch comparison must return false before consulting missing run/approval state.
  // This invokes no handler or provider; a moved/omitted comparison would continue into missing-run work.
  assert!(!foreground_authorized(&app,&sid,"missing-native-fixture","unused","code_index_scan",&json!({"action":"discover"}),"unused").await.unwrap());
  assert!(app.store.get("run:missing-native-fixture").await.unwrap().is_none());
 }
}
