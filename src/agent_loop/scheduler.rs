//! Claim queued runs and renew their leases while the loop is alive.
use super::*;
// A frozen Auto release can inherit runs paused by the previous approval policy.
// Only an undispatched tool is requeued; the claimed run checks live Deny again.
pub(crate) async fn requeue_auto_approval(app:&App,id:&str)->Result<()> {
    if app.active.lock().await.contains_key(id) {return Ok(());}
    let mut run=app.run(id).await?;
    let sid=run["session_id"].as_str().unwrap_or("").to_owned();
    if status(&run)!="waiting_approval" {return Ok(());}
    let mut restricted=Vec::new();
    fn requires_specific_approval(pending:&Value)->bool {
        !pending["authorization"].is_null() || pending["new_intent_confirmation"]==true
    }
    if requires_specific_approval(&run["pending"]){restricted.push(run["pending"].clone());}
    if let Some(batch)=run["batch"].as_array(){
        restricted.extend(batch.iter().map(|item|item["pending"].clone()).filter(requires_specific_approval));
    }
    if !restricted.is_empty(){
        let requester=run["requester_user_id"].as_str().unwrap_or(app.store.current_user_id());
        let agent=run["agent_id"].as_str().unwrap_or("shared_default");
        let access=crate::shared_chat::live_access(&app.store,requester,&sid,agent).await;
        let mut actor_app=app.clone();actor_app.chat_actor=Some(requester.to_owned());
        let controls=ability_controls(&actor_app,&sid).await;
        let mut invalid=!app.store.participation_allows_run(&run).await?
            || !app.run_model_authorized(&run).await
            || access.as_ref().map_or(true,|access|!access.admitted());
        for pending in restricted {
            let revisions_valid=match (&controls,&access) {
                (Ok((policy,mode,visibility,state)),Ok(access))=>
                    pending["policy_version"]==policy["version"]
                    && pending["mode_revision"].as_u64().unwrap_or(0)==*mode
                    && pending["visibility_revision"]==visibility["revision"]
                    && pending["ability_state_revision"]==state["revision"]
                    && pending["access_revision"]==json!(access.revision),
                _=>false,
            };
            if !revisions_valid || !crate::credential_authorization::valid(&app.store,&run,&pending).await? {invalid=true;break;}
        }
        if invalid {
            fn cancel(pending:&mut Value){
                if matches!(pending["state"].as_str(),Some("waiting"|"requested")){
                    pending["state"]=json!("cancelled");pending["decision"]=Value::Null;pending["human_approved"]=json!(false);
                }
            }
            cancel(&mut run["pending"]);
            if let Some(batch)=run["batch"].as_array_mut(){for item in batch{cancel(&mut item["pending"]);}}
            run["status"]=json!("stopped");run["error"]=json!("Specific action authorization changed; request a fresh action");
            app.save(&mut run).await?;
        }
        // Credential ownership and an additional matching intent always require
        // their specific human approval; Auto must not rewrite its stable ID.
        return Ok(());
    }
    if status(&run)!="waiting_approval" || run["pending"]["state"]!="waiting"
        || !session_mode(app,&sid).await.is_ok_and(|mode|
            mode["mode"]=="write" && mode["execution_mode"]=="auto") {
        return Ok(());
    }
    run["pending"]["id"]=json!(uuid());
    run["pending"]["state"]=json!("requested");
    run["pending"]["decision"]=Value::Null;
    run["status"]=json!("queued");
    app.save(&mut run).await
}
// Retire compacted reasoning outside the run loop. Each user-store job commits
// atomically and remains queued after a failed attempt or process restart.
async fn reasoning_cleanup_worker(app: App) {
    loop {
        if app.stop.is_cancelled() { break; }
        match user_apps(&app).await {
            Ok(users) => for user in users {
                if app.stop.is_cancelled() { break; }
                if let Err(error)=user.store.cleanup_one_reasoning().await {
                    tracing::warn!("Reasoning cleanup deferred: {error}");
                }
            },
            Err(error) => tracing::warn!("Reasoning cleanup user scan deferred: {error}"),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
// This is the loop container's dispatcher: announce that it is alive, notice
// queued work, claim a lease, and start at most the configured number of runs.
#[cfg(test)]
pub(crate) async fn scheduler(app: App) {
    scheduler_with_progress(app, watchdog::Progress::new()).await;
}

pub(crate) async fn scheduler_with_progress(app: App, progress: watchdog::Progress) {
    use watchdog::Stage;
    let wake_listener=crate::wake::listener(&app).await.ok();
    let naming=tokio::spawn(crate::session_naming::service(app.clone()));
    let cleanup=tokio::spawn(reasoning_cleanup_worker(app.clone()));
    let manager=tokio::spawn(crate::manager::scheduler(app.clone()));
    let modules=tokio::spawn(crate::modules::service(app.clone()));
    let background=tokio::spawn(crate::background_tasks::service(app.clone()));
    let owner = uuid();
    let heartbeat_app=app.clone();let heartbeat_owner=owner.clone();
    let heartbeat=tokio::spawn(async move{loop{if heartbeat_app.stop.is_cancelled(){break;}let _=heartbeat_app.store.write_loop_heartbeat(&heartbeat_owner).await;tokio::select!{_=heartbeat_app.stop.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(2))=>{}}}});
    let mut last_heartbeat = 0;
    let mut last_recovery = 0;
    loop {
        if app.stop.is_cancelled() || !app.ready.load(Ordering::Acquire) {
            break;
        }
        // Adopt scheduler and cache limits without replacing active runs.
        match loop_runtime::Config::read(&app.runtime_path) {
            Ok(config)=>{
                let mut previous=app.runtime.write().await;
                if previous.cache.document_budget_bytes!=config.cache.document_budget_bytes
                    ||previous.cache.context_budget_bytes!=config.cache.context_budget_bytes {
                    app.store.configure_cache(config.cache.document_budget_bytes,config.cache.context_budget_bytes);
                }
                let background_changed=previous.scheduler.background_rescan_interval_ms!=config.scheduler.background_rescan_interval_ms;
                *previous=config;
                drop(previous);
                if background_changed{crate::wake::notify(&app).await;}
            },
            Err(_)=>{tracing::error!("Live runtime configuration rejected");tokio::time::sleep(Duration::from_secs(1)).await;continue;}
        }
        let snapshot=live_assets::catalog(&app.abilities);
        let _=live_assets::record_adoption(&app.store.root,"loop",&snapshot);
        let _=crate::engine_components::adoption(&app.store.root,"loop");
        progress.at(Stage::Heartbeat);
        if now().saturating_sub(last_heartbeat)>=2 {
            if let Err(error)=app.store.write_loop_heartbeat(&owner).await {
                tracing::error!("Loop heartbeat failed: {error}");
            }
            last_heartbeat=now();
        }
        if now() - last_recovery >= app.runtime.read().await.scheduler.recovery_interval_ms.div_ceil(1000) {
            progress.at(Stage::Recovery);
            if recover(&app, false).await.is_err() {
                tracing::error!("Recovery scan failed");
            }
            last_recovery = now();
        }
        progress.at(Stage::DispatchGate);
        let _guard = app.gate.lock().await;
        if let Ok(false) = app.shutoff().await {
            progress.at(Stage::UserScan);
            let users=match user_apps(&app).await {
                Ok(users)=>users,
                Err(error)=>{tracing::error!("User queue scan failed: {}",error);Vec::new()}
            };
            for user_app in users {
            progress.at(Stage::Commands);
            if let Err(error)=user_app.store.acknowledge_loop_commands().await {
                tracing::error!("Loop command validation failed: {error}");
                continue;
            }
            if let Err(error)=crate::followup_queue::scheduler_tick(&user_app).await {
                tracing::warn!("Followup queue reconciliation deferred: {error}");
            }
            if let Err(error)=user_app.store.drain_message_queues().await {tracing::warn!("Message queue drain deferred: {error}");}
            progress.at(Stage::RunScan);
            if let Ok(runs) = user_app.store.active_runs().await {
                for view in runs {
                    let id = view["id"].as_str().unwrap().to_owned();
                    if !app.active.lock().await.contains_key(&id)
                        && crate::followup_queue::interrupt_requested(&user_app,&id).await.unwrap_or(false) {
                        let Ok(mut interrupted)=user_app.run(&id).await else {continue;};
                        if let Err(error)=super::run::settle_followup_interrupt(&user_app,&mut interrupted).await {
                            tracing::warn!(run_id=%id,"Followup interruption deferred: {error}");
                        }
                        continue;
                    }
                    if status(&view) == "paused" {
                        if let Err(_)=crate::engine_components::restore(&user_app,&id).await {tracing::warn!("Engine component restoration deferred");}
                        continue;
                    }
                    if status(&view) == "waiting_approval" {
                        if let Err(error)=requeue_auto_approval(&user_app,&id).await {
                            tracing::warn!(run_id=%id,"Auto approval reconciliation deferred: {error}");
                        }
                        continue;
                    }
                    if status(&view) != "queued" {continue;}
                    if !crate::engine_components::available(&user_app.store.root) {
                        if let Ok(mut run)=user_app.run(&id).await {let _=crate::engine_components::pause(&user_app,&mut run).await;}
                        continue;
                    }
                    progress.at(Stage::Claim);
                    let mut run = match user_app.run(&id).await {
                        Ok(r) => r,
                        Err(_) => continue,
                    };
                    let mut active = app.active.lock().await;
                    if active.contains_key(&id) {
                        continue;
                    }
                    let ceiling=app.runtime.read().await.scheduler.max_active_runs;
                    if ceiling!=0 && active.len()>=ceiling {break;}
                    let permit = match app.slots.clone().try_acquire_owned() {
                        Ok(p) => p,
                        Err(_) => break,
                    };
                    let cancel = app.stop.child_token();
                    active.insert(id.clone(), cancel.clone());
                    drop(active);
                    if run["context_hydration_pending"] == true {
                        let requester=run["requester_user_id"].as_str().unwrap_or(user_app.store.current_user_id());
                        let context=if requester==user_app.store.current_user_id() {user_app.store.context(run["session_id"].as_str().unwrap()).await}
                            else {user_app.store.shared_model_context(run["session_id"].as_str().unwrap()).await};
                        match context {
                            Ok(mut input) => {
                                // Keep saved notes beside the latest user turn as source data.
                                if requester==user_app.store.current_user_id(){memory::insert(&mut input,run["memory_context"].as_str());}
                                run["input"] = json!(input);
                                run["context_hydration_pending"] = json!(false);
                            },
                            Err(error) => {tracing::warn!(run_id=%id,"Context hydration failed: {}",error);app.active.lock().await.remove(&id);continue;}
                        }
                    }
                    let generation=uuid();
                    if !matches!(user_app.store.claim_run(&mut run,&owner,&generation).await,Ok(true)) {
                        app.active.lock().await.remove(&id);
                        continue;
                    }
                    let mut cloned = user_app.clone();
                    cloned.chat_actor=run["requester_user_id"].as_str().map(str::to_owned);
                    let requester=crate::shared_chat::actor(&cloned).to_owned();
                    match crate::shared_chat::resolve(&cloned,run["session_id"].as_str().unwrap_or(""),&requester).await {
                        Ok(access)=>cloned.chat_access=Some(access),
                        Err(_)=>{app.active.lock().await.remove(&id);continue;}
                    }
                    let lease_owner=owner.clone();
                    tokio::spawn(async move {
                        let epoch=run["lease_epoch"].as_i64().unwrap_or(0);
                        let watcher_store=cloned.store.clone();
                        let watcher_id=id.clone();
                        let watcher_generation=generation.clone();
                        let watcher_cancel=cancel.clone();
                        let interrupt_app=cloned.clone();
                        let interrupt_id=id.clone();
                        let interrupt_cancel=cancel.clone();
                        let interrupt_watcher=tokio::spawn(async move {
                            loop {
                                tokio::select! {
                                    _=interrupt_cancel.cancelled()=>break,
                                    _=tokio::time::sleep(Duration::from_millis(250))=>{},
                                }
                                match crate::followup_queue::interrupt_requested(&interrupt_app,&interrupt_id).await {
                                    Ok(true)=>{interrupt_cancel.cancel();break;},
                                    Ok(false)=>{},
                                    Err(error)=>tracing::warn!(run_id=%interrupt_id,"Interrupt observation deferred: {error}"),
                                }
                            }
                        });
                        let watcher=tokio::spawn(async move {
                            loop {
                                tokio::time::sleep(Duration::from_secs(5)).await;
                                if watcher_cancel.is_cancelled() {break;}
                                let renewed=watcher_store.renew_run_lease(
                                    &watcher_id,&watcher_generation,&lease_owner,epoch).await
                                    .unwrap_or(false);
                                if !renewed {watcher_cancel.cancel();break;}
                            }
                        });
                        if let Err(error) =
                            cloned.session_operation(run["session_id"].as_str().unwrap_or(""),cancel.clone(),
                                execute_run(cloned.clone(), id.clone(), generation, cancel.clone())).await
                        {
                            tracing::warn!(run_id=%id,"Run interrupted: {}",error);
                        }
                        // Join paths that return early (approval and parallel tools)
                        // also settle durable interruption after their awaits finish.
                        {
                            let _settle_guard=cloned.gate.lock().await;
                            if let Ok(mut final_run)=cloned.run(&id).await {
                            if let Err(error)=super::run::settle_followup_interrupt(&cloned,&mut final_run).await {
                                tracing::warn!(run_id=%id,"Followup handoff deferred: {error}");
                            }
                            }
                        }
                        cancel.cancel();
                        interrupt_watcher.abort();
                        watcher.abort();
                        cloned.active.lock().await.remove(&id);
                        drop(permit);
                        crate::wake::notify(&cloned).await;
                    });
                }
            }
            }
        }
        drop(_guard);
        progress.at(Stage::Idle);
        crate::wake::wait(&app).await;
    }
    if let Some(listener)=wake_listener {listener.abort();}
    naming.abort();
    cleanup.abort();
    manager.abort();
    background.abort();
    modules.abort();
    heartbeat.abort();
}

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
