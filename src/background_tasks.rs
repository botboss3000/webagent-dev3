//! Root composition for the actual Engine background service and lifecycle.
use crate::*;
use crate::tool_execution::Settings;
pub(crate) use webagent_engine::runtime::background::{schemas,owns};
#[cfg(test)] fn identity(rid:&str,call_id:&str)->String {webagent_engine::runtime::background::fixture_task_identity(rid,call_id)}
pub(crate) async fn permitted(app:&App,task:&Value)->Result<bool> {app.engine().background_permitted(task).await}
pub(crate) async fn dispatch_authorized(app:&App,sid:&str,rid:&str,generation:&str,pending_id:&str,name:&str,args:&Value)->Result<Option<bool>> {app.engine().background_dispatch_authorized(sid,rid,generation,pending_id,name,args).await}
pub(crate) async fn submit(app:&App,sid:&str,rid:&str,generation:&str,name:&str,args:&Value,pending_id:&str,policy_version:u64,settings:&Settings)->Result<Value> {app.engine().background_submit(sid,rid,generation,name,args,pending_id,policy_version,settings).await}
pub(crate) async fn control(app:&App,sid:&str,_rid:&str,name:&str,args:&Value,cancel:CancellationToken)->Result<Value> {app.engine().background_control(sid,_rid,name,args,cancel).await}
pub(crate) async fn collect(app:&App,run:&mut Value)->Result<bool> {app.engine().background_collect(run).await}
pub(crate) async fn service(app:App) {app.engine().background_service().await}
async fn service_with_hints(app:App,hints:tokio::sync::watch::Receiver<u64>) {app.engine().background_service_with_hints(hints).await}
#[cfg(test)] async fn run_task(app:&App,task:Value)->Result<()> {app.engine().background_run_task(task).await}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn stable_identity_and_control_names() {
        assert_eq!(identity("r","c"),identity("r","c"));
        assert_ne!(identity("r","c"),identity("r2","c"));
        assert!(owns("task_wait"));assert!(!owns("task_start"));assert_eq!(schemas().len(),3);
    }
    #[tokio::test] async fn encrypted_task_cas_reopens() {
        let root=tempfile::tempdir().unwrap();let store=Store::open(root.path()).await.unwrap();
        let task=json!({"id":"test","user_id":"admin","session_id":"s","status":"queued","revision":0,"arguments":{"private":"secret"}});
        assert!(store.background_cas(&task,None).await.unwrap());
        assert!(!store.background_cas(&task,None).await.unwrap());
        let mut next=task.clone();next["status"]=json!("cancelled");
        assert!(store.background_cas(&next,Some(0)).await.unwrap());
        assert!(!store.background_cas(&task,Some(0)).await.unwrap());
        let reopened=Store::open(root.path()).await.unwrap();
        assert_eq!(reopened.background_get("test").await.unwrap().unwrap()["status"],"cancelled");
    }

/// Idle storage stays untouched before recovery; a private hint triggers a fresh scan.
#[tokio::test]
async fn background_dispatch_idle_waits_and_wake_rescans() {
 use crate::runtime_tests::fixture;
 let (_root,app)=fixture().await;
 let (wake,hints)=tokio::sync::watch::channel(0);
 let service=tokio::spawn(service_with_hints(app.clone(),hints));
 let exists=||async {
  sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM sqlite_master WHERE name='background_task_index'")
   .fetch_one(&app.store.pool).await.unwrap()>0
 };
 tokio::time::timeout(Duration::from_secs(2),async {while !exists().await{tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
 sqlx::query("DROP TABLE background_task_index").execute(&app.store.pool).await.unwrap();
 tokio::time::sleep(Duration::from_millis(600)).await;
 assert!(!exists().await,"idle service scanned before the recovery deadline");
 wake.send_modify(|sequence|*sequence+=1);
 tokio::time::timeout(Duration::from_secs(1),async {while !exists().await{tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
 app.stop.cancel();tokio::time::timeout(Duration::from_secs(1),service).await.unwrap().unwrap();
}

/// Withdrawal holds accepted input; restoration resumes only host-marked, still-authorized work.
#[tokio::test]
async fn background_dispatch_withdrawal_selective_restoration() {
 use crate::runtime_tests::{fixture,conversation,queued};
 let (_root,app)=fixture().await;let sid=conversation(&app).await;
 let id=queued(&app,&sid).await;let mut run=app.run(&id).await.unwrap();let input=run["input"].clone();
 let controls=engine_components::Controls{revision:1,disabled:vec![engine_components::BACKGROUND.into()]};
 engine_components::save_controls(&app.store.root,&controls).unwrap();
 assert!(!engine_components::available(&app.store.root));engine_components::pause(&app,&mut run).await.unwrap();
 assert_eq!(run["component_pause"]["component"],engine_components::BACKGROUND);
 engine_components::restore(&app,&id).await.unwrap();assert_eq!(app.run(&id).await.unwrap()["status"],"paused");
 engine_components::save_controls(&app.store.root,&Default::default()).unwrap();
 engine_components::restore(&app,&id).await.unwrap();let restored=app.run(&id).await.unwrap();assert_eq!(restored["status"],"queued");assert_eq!(restored["input"],input);
 for (uncertain,marker) in [(false,Value::Null),(true,json!({"version":1,"component":engine_components::BACKGROUND}))] {
  let mut manual=app.run(&id).await.unwrap();manual["status"]=json!("paused");manual["uncertain"]=json!(uncertain);manual["component_pause"]=marker;app.save(&mut manual).await.unwrap();
  engine_components::restore(&app,&id).await.unwrap();assert_eq!(app.run(&id).await.unwrap()["status"],"paused");
 }
}
/// Retained queued reads survive restoration races; only already-admitted fresh claims may drain.
#[tokio::test]
async fn background_dispatch_retained_queue_and_claim_authority() {
 use crate::runtime_tests::{fixture,conversation,queued,activate_codebase};
 let (_root,app)=fixture().await;let sid=conversation(&app).await;
 activate_codebase(&app,&sid).await;let rid=queued(&app,&sid).await;
 let mut run=app.run(&rid).await.unwrap();
 assert!(app.store.claim_run(&mut run,"fixture-loop","background-generation").await.unwrap());
 run["loaded_abilities"]=json!(["codebase_admin"]);let args=json!({"path":"fixture"});
 let (policy,mode_revision,visibility,state)=ability_controls(&app,&sid).await.unwrap();
 let access=app.store.live_access(app.store.current_user_id(),"shared_default").await.unwrap();
 run["pending"]=json!({"id":"background-pending","name":"file_read","call_id":"background-call",
  "arguments":args,"digest":digest(&args),"policy_version":policy["version"],"mode_revision":mode_revision,
  "visibility_revision":visibility["revision"],"ability_state_revision":state["revision"],
  "access_revision":access.revision,"state":"executing","decision":"consumed","human_approved":false});
 app.save(&mut run).await.unwrap();let settings=tool_execution::resolve(&visibility,"file_read").unwrap();
 let handle=submit(&app,&sid,&rid,"background-generation","file_read",&args,"background-pending",
  policy["version"].as_u64().unwrap_or(0),&settings).await.unwrap();
 let id=handle["task_id"].as_str().unwrap();
 run=app.run(&rid).await.unwrap();run["pending"]=Value::Null;app.save(&mut run).await.unwrap();
 crate::engine_components::pause(&app,&mut run).await.unwrap();
 let original=app.store.background_get(id).await.unwrap().unwrap();
 run_task(&app,original.clone()).await.unwrap();
 assert_eq!(app.store.background_get(id).await.unwrap().unwrap(),original);
 let mut claimed=original;claimed["status"]=json!("running");claimed["claim_id"]=json!("read-claim");claimed["lease_until"]=json!(now()+30);
 assert!(app.store.background_cas(&claimed,Some(0)).await.unwrap());
 assert_eq!(dispatch_authorized(&app,&sid,&rid,"read-claim",id,"file_read",&args).await.unwrap(),Some(true));
 for invalid in [json!({"uncertain":true}),json!({"component_pause":null}),
   json!({"pending":{"state":"executing"}}),json!({"batch":[{"state":"executing"}]}),
   json!({"batch":[{"pending":{"external_job":"external"}}]})] {
  let mut current=app.run(&rid).await.unwrap();let preserved=current.clone();
  for (key,value) in invalid.as_object().unwrap(){current[key]=value.clone();}
  app.save(&mut current).await.unwrap();
  assert_eq!(dispatch_authorized(&app,&sid,&rid,"read-claim",id,"file_read",&args).await.unwrap(),Some(false));
  let mut reset=app.run(&rid).await.unwrap();
  for key in ["uncertain","component_pause","pending","batch"]{reset[key]=preserved[key].clone();}
  app.save(&mut reset).await.unwrap();
 }
 assert_eq!(dispatch_authorized(&app,&sid,&rid,"other-claim",id,"file_read",&args).await.unwrap(),Some(false));
}

}

/// Bound individual timer allocations, never the configured rescan interval.
/// Intermediate chunks stay inside this future and cannot trigger durable scans.
async fn wait_for_background_retry(milliseconds:u64) {webagent_engine::runtime::background::wait_for_background_retry(milliseconds).await}
#[cfg(test)] mod rescan_timer_tests {
 use super::*;
 /// Huge values must remain pending and cancellable instead of overflowing a timer deadline.
 #[tokio::test] async fn huge_wait_does_not_overflow_or_return_early() {
  assert!(tokio::time::timeout(Duration::from_millis(20),wait_for_background_retry(u64::MAX)).await.is_err());
 }
 /// Subsecond periods remain usable and complete without the old one-second planner minimum.
 #[tokio::test] async fn subsecond_wait_reaches_its_deadline() {
  let start=tokio::time::Instant::now();
  tokio::time::timeout(Duration::from_secs(2),wait_for_background_retry(20)).await.unwrap();
  assert!(start.elapsed()>=Duration::from_millis(20));
 }
}

#[cfg(test)] mod rescan_adoption_tests {
 use super::*;
 /// A changed interval cancels an in-flight wait in either direction.
 #[tokio::test] async fn running_wait_adopts_shorter_and_longer_intervals() {
  let (_root,app)=crate::runtime_tests::fixture().await;
  app.runtime.write().await.scheduler.background_rescan_interval_ms=60000;
  let (wake,hints)=tokio::sync::watch::channel(0);
  let service=tokio::spawn(service_with_hints(app.clone(),hints));
  let exists=||async {
   sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM sqlite_master WHERE name='background_task_index'")
    .fetch_one(&app.store.pool).await.unwrap()>0
  };
  tokio::time::timeout(Duration::from_secs(2),async {
   while !exists().await {tokio::time::sleep(Duration::from_millis(10)).await;}
  }).await.unwrap();
  tokio::time::sleep(Duration::from_millis(50)).await;
  sqlx::query("DROP TABLE background_task_index").execute(&app.store.pool).await.unwrap();
  app.runtime.write().await.scheduler.background_rescan_interval_ms=100;
  wake.send_modify(|sequence|*sequence+=1);
  tokio::time::timeout(Duration::from_secs(1),async {
   while !exists().await {tokio::time::sleep(Duration::from_millis(10)).await;}
  }).await.unwrap();
  // Prove the new cadence independently of the hint-triggered scan.
  tokio::time::sleep(Duration::from_millis(30)).await;
  sqlx::query("DROP TABLE background_task_index").execute(&app.store.pool).await.unwrap();
  tokio::time::timeout(Duration::from_secs(1),async {
   while !exists().await {tokio::time::sleep(Duration::from_millis(10)).await;}
  }).await.unwrap();
  app.runtime.write().await.scheduler.background_rescan_interval_ms=60000;
  sqlx::query("DROP TABLE background_task_index").execute(&app.store.pool).await.unwrap();
  wake.send_modify(|sequence|*sequence+=1);
  tokio::time::timeout(Duration::from_secs(1),async {
   while !exists().await {tokio::time::sleep(Duration::from_millis(10)).await;}
  }).await.unwrap();
  tokio::time::sleep(Duration::from_millis(50)).await;
  sqlx::query("DROP TABLE background_task_index").execute(&app.store.pool).await.unwrap();
  tokio::time::sleep(Duration::from_millis(300)).await;
  assert!(!exists().await,"old short timer survived interval adoption");
  app.stop.cancel();tokio::time::timeout(Duration::from_secs(1),service).await.unwrap().unwrap();
 }
}
