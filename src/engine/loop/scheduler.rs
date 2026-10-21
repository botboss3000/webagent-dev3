//! Root role composition only; Engine owns claims, execution and supervision.
use super::*;
use crate::engine_runtime::Engine;
pub(crate) async fn requeue_auto_approval(app:&App,id:&str)->Result<()> {Engine::requeue_auto_approval(&app.engine(),id).await}
#[cfg(test)] pub(crate) async fn scheduler(app:App) {Engine::scheduler_with_progress(app.engine(),watchdog::Progress::new()).await}
pub(crate) async fn scheduler_with_progress(app:App,progress:watchdog::Progress) {Engine::scheduler_with_progress(app.engine(),progress).await}
#[cfg(test)]
mod credential_reconciliation_tests {
 use super::*;
 #[tokio::test]
 async fn additional_action_approval_survives_auto_reconciliation_and_revocation(){
  for batch in [false,true] {
   let (_root,app)=crate::runtime_tests::fixture().await;
   let sid=crate::runtime_tests::conversation(&app).await;
   let rid=crate::runtime_tests::queued(&app,&sid).await;
   app.store.put(&format!("session_mode:{sid}"),&json!({"mode":"write","execution_mode":"auto","revision":0})).await.unwrap();
   let mut run=app.run(&rid).await.unwrap();
   let (policy,mode,visibility,state)=ability_controls(&app,&sid).await.unwrap();
   let access=crate::shared_chat::live_access(&app.store,app.store.current_user_id(),&sid,"shared_default").await.unwrap();
   run["status"]=json!("waiting_approval");
   let pending=json!({"id":"additional-wait","name":"save_own_skill","arguments":{},"state":"waiting","decision":null,
    "new_intent_confirmation":true,"action_id":"additional-action","policy_version":policy["version"],"mode_revision":mode,
    "visibility_revision":visibility["revision"],"ability_state_revision":state["revision"],"access_revision":access.revision});
   run["pending"]=if batch {Value::Null}else{pending.clone()};
   if batch {run["batch"]=json!([{"pending":pending},{"pending":{"id":"executing","state":"executing"}}]);}
   app.store.put(&format!("run:{rid}"),&run).await.unwrap();
   for _ in 0..3 {
    requeue_auto_approval(&app,&rid).await.unwrap();
    let waiting=app.run(&rid).await.unwrap();
    assert_eq!(waiting["status"],"waiting_approval");
    let held=if batch {&waiting["batch"][0]["pending"]}else{&waiting["pending"]};
    assert_eq!(held,&pending);
   }
   app.store.put(&format!("session_mode:{sid}"),&json!({"mode":"read_only","execution_mode":"ask","revision":mode+1})).await.unwrap();
   requeue_auto_approval(&app,&rid).await.unwrap();
   let stopped=app.run(&rid).await.unwrap();assert_eq!(stopped["status"],"stopped");
   let held=if batch {&stopped["batch"][0]["pending"]}else{&stopped["pending"]};
   assert_eq!(held["state"],"cancelled");assert!(held["decision"].is_null());
   if batch {assert_eq!(stopped["batch"][1]["pending"]["state"],"executing");}
  }
 }

 #[tokio::test]
 async fn restricted_waiting_authorization_is_never_auto_requeued_and_revocation_cancels_single_and_batch(){
  for (batch,change_mode) in [(false,false),(true,false),(false,true)] {
   let (_root,app)=crate::runtime_tests::fixture().await;
   let sid=crate::runtime_tests::conversation(&app).await;
   let rid=crate::runtime_tests::queued(&app,&sid).await;
   let owner=app.store.current_user_id();
   let credential=format!("tool_credential:{owner}:test");
   app.store.put(&credential,&json!({"owner_user_id":owner,"version":1,"revoked":false})).await.unwrap();
   app.store.put(&format!("session_mode:{sid}"),&json!({"mode":"write","execution_mode":"auto","revision":0})).await.unwrap();
   let mut run=app.run(&rid).await.unwrap();
   run["status"]=json!("waiting_approval");
   run["pending"]=json!({"id":"owner-wait","name":"test_owner_purchase","arguments":{},"state":"waiting","decision":null,"policy_version":1,"mode_revision":0,"visibility_revision":0,"ability_state_revision":0,"access_revision":{}});
   let (policy,mode,visibility,state)=ability_controls(&app,&sid).await.unwrap();
   let access=crate::shared_chat::live_access(&app.store,owner,&sid,"shared_default").await.unwrap();
   run["pending"]["policy_version"]=policy["version"].clone();
   run["pending"]["mode_revision"]=json!(mode);
   run["pending"]["visibility_revision"]=visibility["revision"].clone();
   run["pending"]["ability_state_revision"]=state["revision"].clone();
   run["pending"]["access_revision"]=json!(access.revision);
   run["pending"]["authorization"]=crate::credential_authorization::resolve(&app.store,&run,"test_owner_purchase",&json!({})).await.unwrap().unwrap();
   if batch {
    run["batch"]=json!([{"pending":run["pending"]},{"pending":{"id":"already-executing","state":"executing","effect_evidence":"keep"}}]);
    run["pending"]=Value::Null;
   }
   app.store.put(&format!("run:{rid}"),&run).await.unwrap();
   requeue_auto_approval(&app,&rid).await.unwrap();
   assert_eq!(app.run(&rid).await.unwrap()["status"],"waiting_approval");
   // Exercise both explicit revocation and a credential version replacement.
   if change_mode {
    app.store.put(&format!("session_mode:{sid}"),&json!({"mode":"read_only","execution_mode":"ask","revision":mode+1})).await.unwrap();
   } else {
    app.store.put(&credential,&json!({"owner_user_id":owner,"version":2,"revoked":!batch})).await.unwrap();
   }
   requeue_auto_approval(&app,&rid).await.unwrap();
   let stopped=app.run(&rid).await.unwrap();assert_eq!(stopped["status"],"stopped");
   let pending=if batch {&stopped["batch"][0]["pending"]}else{&stopped["pending"]};
   assert_eq!(pending["state"],"cancelled");assert!(pending["decision"].is_null());
   if batch {assert_eq!(stopped["batch"][1]["pending"]["state"],"executing");assert_eq!(stopped["batch"][1]["pending"]["effect_evidence"],"keep");}
   requeue_auto_approval(&app,&rid).await.unwrap();assert_eq!(app.run(&rid).await.unwrap()["status"],"stopped");
  }
 }
}
