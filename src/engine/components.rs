//! Root compatibility for the independently compiled Engine component owner.
use crate::*;
pub(crate) use webagent_engine::components::*;
pub(crate) async fn pause(app:&App,run:&mut Value)->Result<()> {app.engine().component_pause(run).await}
pub(crate) async fn restore(app:&App,id:&str)->Result<()> {app.engine().component_restore(id).await}
pub(crate) async fn background_plan(app:&App,input:&webagent_background_contracts::Request)->Result<webagent_background_contracts::Plan> {webagent_engine::components::background_plan(&app.store.root,&app.stop,input).await}
pub(crate) async fn tool_results_plan(app:&App,run:&Value,input:&webagent_context_contracts::tool_results::Request,cancel:&CancellationToken)->Result<webagent_context_contracts::tool_results::Plan> {webagent_engine::components::tool_results_plan(&app.store.root,run,input,cancel).await}
pub(crate) async fn prepare<'a>(app:&App,run:&Value,request:context_input::Request<'a>,state:&mut context_accounting::ContextAccounting,cancel:&CancellationToken)->Result<context_assembly::Prepared<'a>> {webagent_engine::components::prepare(&app.store.root,run,request,state,cancel).await}
#[cfg(test)] mod lifecycle_tests {use super::*;
 #[tokio::test]
 /// Restore retained component pauses only after checking current dispatch authority.
 async fn restoration_preserves_manual_pauses_and_rechecks_access(){
  let (_root,app)=crate::runtime_tests::fixture().await;let sid=crate::runtime_tests::conversation(&app).await;
  let id=crate::runtime_tests::queued(&app,&sid).await;let mut run=app.run(&id).await.unwrap();
  let input=run["input"].clone();pause(&app,&mut run).await.unwrap();assert_eq!(run["status"],"paused");
  restore(&app,&id).await.unwrap();let restored=app.run(&id).await.unwrap();assert_eq!(restored["status"],"queued");assert_eq!(restored["input"],input);
  let mut manual=restored;manual["status"]=json!("paused");manual["component_pause"]=Value::Null;app.save(&mut manual).await.unwrap();
  restore(&app,&id).await.unwrap();assert_eq!(app.run(&id).await.unwrap()["status"],"paused");
  pause(&app,&mut manual).await.unwrap();manual["selection_plan"]=Value::Null;app.save(&mut manual).await.unwrap();
  restore(&app,&id).await.unwrap();assert_eq!(app.run(&id).await.unwrap()["status"],"stopped");
 }
 #[tokio::test]
 /// Fence ordinary and compaction dispatch when a component disappears during credential refresh.
 async fn removal_during_credentials_pauses_normal_and_system_dispatch(){
  use axum::routing::post;
  for compact in [false,true] {
   let (_root,mut app)=crate::runtime_tests::fixture().await;
   let calls=Arc::new(std::sync::atomic::AtomicUsize::new(0));let captured=calls.clone();
   let mock=Router::new().route("/responses",post(move|Json(_body):Json<Value>|{
    let calls=captured.clone();async move{
     calls.fetch_add(1,Ordering::Relaxed);let event=json!({"type":"response.completed","response":{"status":"completed","output":[]}});
     ([("content-type","text/event-stream")],format!("data: {event}\n\n"))
    }
   }));
   let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
   let server=tokio::spawn(async move{axum::serve(listener,mock).await.unwrap();});
   app.provider=provider::Provider::for_test(format!("http://{address}/responses"));
   let sid=crate::runtime_tests::conversation(&app).await;let id=crate::runtime_tests::queued(&app,&sid).await;
   let mut run=app.run(&id).await.unwrap();
   if compact{
    let mut config=context_compaction::Config::read(&app.compaction_config_path).unwrap();
    config.trigger_tokens=16000;config.tail_tokens=50;config.summary_tokens=30;config.summary_reserve_bytes=300;config.persist(&app.compaction_config_path).unwrap();
    let mut history:Vec<Value>=(0..24).map(|index|json!({"role":if index%2==0{"user"}else{"assistant"},"content":"old".repeat(1400)})).collect();
    history.push(json!({"role":"user","content":"latest"}));run["input"]=Value::Array(history);
    app.save(&mut run).await.unwrap();
   }
   assert!(app.store.claim_run(&mut run,"engine-race-loop","engine-race").await.unwrap());
   let credentials=app.refresh.lock().await;let task_app=app.clone();let task_id=id.clone();
   let task=tokio::spawn(async move{crate::agent_loop::execute_run(task_app,task_id,"engine-race".into(),CancellationToken::new()).await});
   tokio::time::timeout(Duration::from_secs(20),async{
    loop{let current=app.run(&id).await.unwrap();
     if if compact {current["engine_component_prepared"]["kind"]=="compaction"}else{!current["context_assembly_report"].is_null()} {break;}
     if task.is_finished(){panic!("run ended before credential boundary: {}",current["error"]);}
     tokio::time::sleep(Duration::from_millis(10)).await;
    }
   }).await.unwrap();
   save_controls(&app.store.root,&Controls{revision:1,disabled:vec![CONTEXT.into()]}).unwrap();
   drop(credentials);tokio::time::timeout(Duration::from_secs(10),task).await.unwrap().unwrap().unwrap();
   let paused=app.run(&id).await.unwrap();assert_eq!(paused["status"],"paused");assert_eq!(paused["component_pause"]["component"],CONTEXT);
   assert_eq!(calls.load(Ordering::Relaxed),0);server.abort();
  }
 }
 #[tokio::test]
 async fn pure_component_pauses_keep_the_specific_registered_identity() {
  let (_root,app)=Box::pin(crate::runtime_tests::fixture()).await;
  let sid=crate::runtime_tests::conversation(&app).await;let id=crate::runtime_tests::queued(&app,&sid).await;
  let mut run=app.run(&id).await.unwrap();let input=run["input"].clone();
  save_controls(&app.store.root,&Controls{revision:1,disabled:vec![ASSETS.into(),CAPABILITY.into(),PROJECTION.into()]}).unwrap();
  for component in [ASSETS,CAPABILITY,PROJECTION] {
   run["component_pause"]=json!({"component":component,"version":1});
   pause(&app,&mut run).await.unwrap();
   assert_eq!(run["component_pause"]["component"],component);
   assert_eq!(run["input"],input);assert!(retained_pause(&run));
   restore(&app,&id).await.unwrap();assert_eq!(app.run(&id).await.unwrap()["status"],"paused");
   let mut unsafe_run=run.clone();unsafe_run["pending"]=json!({"state":"executing"});
   assert!(!retained_pause(&unsafe_run));
  }
 }
}
#[cfg(test)] mod background_retry_tests {
 use super::*;
 /// A silent workload still recovers after the existing cooldown when healthy rescans are infrequent.
 #[tokio::test] async fn failure_cooldown_wakes_a_long_interval_without_a_hint() {
  let (_root,app)=crate::runtime_tests::fixture().await;
  app.runtime.write().await.scheduler.background_rescan_interval_ms=60000;
  sqlx::query("DROP TABLE IF EXISTS background_task_index").execute(&app.store.pool).await.unwrap();
  webagent_engine::components::fixture_background_failure_for(Duration::from_millis(200));
  let service=tokio::spawn(crate::background_tasks::service(app.clone()));
  let exists=||async {sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM sqlite_master WHERE name='background_task_index'")
   .fetch_one(&app.store.pool).await.unwrap()>0};
  tokio::time::sleep(Duration::from_millis(40)).await;assert!(!exists().await);
  tokio::time::timeout(Duration::from_secs(2),async {
   while !exists().await {tokio::time::sleep(Duration::from_millis(10)).await;}
  }).await.unwrap();
  app.stop.cancel();tokio::time::timeout(Duration::from_secs(1),service).await.unwrap().unwrap();
 }
}
