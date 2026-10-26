use super::{Runtime,config,ports::*,models::ModelRecords};
use crate::ports::*;
use crate::context::abilities::Catalog;
use crate::tools::{self,execution as tool_execution};
use crate::{now,status,digest,uuid,workflow};
use anyhow::{anyhow,Result};
use serde_json::{json,Value};
use std::{sync::Arc,time::Duration};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use webagent_access as access;
// A model turn may request several independent tools. Rust authorizes each
// one; the Python worker only receives calls that passed its own gate.

use tokio::task::JoinSet;

// A result commit takes the same gate as tool authorization. Tool futures must
// keep being polled while that commit waits, including the future holding the
// gate. A parent-polled FuturesUnordered can deadlock at this boundary.
#[cfg(test)]
fn spawn_batch_calls<T, F>(calls: impl IntoIterator<Item = F>) -> JoinSet<T>
where
    T: Send + 'static,
    F: std::future::Future<Output = T> + Send + 'static,
{
    let mut tasks = JoinSet::new();
    for call in calls {
        tasks.spawn(call);
    }
    tasks
}

fn denied(message:&str)->Value { json!({"status":"denied","output":message}) }

pub fn executing_write<A:tool_execution::ActionCatalog>(run:&Value)->bool {
    run["batch"].as_array().is_some_and(|items|items.iter().any(|item|
        item["state"]=="executing"
            && !A::is_read(item["pending"]["name"].as_str().unwrap_or(""))))
}
pub fn uncertain_count(run:&Value)->usize {
    run["batch"].as_array().map(|items|items.iter()
        .filter(|item|item["state"]=="uncertain").count()).unwrap_or(0)
}
pub fn mark_uncertain_writes<A:tool_execution::ActionCatalog>(run:&mut Value) {
    if let Some(items)=run["batch"].as_array_mut() {
        for item in items {if item["state"]=="executing"
            && !A::is_read(item["pending"]["name"].as_str().unwrap_or("")) {
            item["state"]=json!("uncertain");
        }}
    }
}


impl<S:ScopedStorage+ModelRecords+ToolJournal+ChatEffects+AdmissionEffects+NotificationEffects+UsageEffects+RunObservation+RunPresentation,C:config::SchemaCatalog,B:Catalog,A:super::actions::Actions<S>+Clone+PartialEq,H:ChatAuthority<S>,M:ModuleBindings<S>,X:AgentPolicy<S>+NativePolicy+CredentialBindings<S>+SkillSources<S>+PromptSlots<S>+MainGuidance<S>+ModelDiagnostics<S>+InstallationAccess<S>+ContextDiagnostics<S>+ProviderUsage+StartupPolicy<S>+FollowupQueue<S>+super::native::NativeDispatch<S,C,B,H::Binding>> Runtime<S,C,B,A,H,M,X> {
pub async fn execute_parallel_batch(
    app:&Self,id:&str,generation:&str,cancel:&CancellationToken,
    workflow_policy:&workflow::Policy,access:&access::authority::LiveAccess,
    mut run:Value,guard:tokio::sync::MutexGuard<'_,()>,
)->Result<()> {
    let calls=run["calls"].as_array().ok_or_else(||anyhow!("Missing tool calls"))?.clone();
    let mut ids=std::collections::BTreeSet::new();
    if calls.iter().any(|call|call["call_id"].as_str().is_none_or(|id|id.is_empty() || !ids.insert(id.to_owned()))) {
        run["status"]=json!("failed");
        run["error"]=json!("Provider returned missing or duplicate tool call identifiers");
        app.save(&mut run).await?;
        return Ok(());
    }
    if run["batch"].is_null() {
        run["batch"]=json!(calls.iter().map(|_|json!({"state":"new","pending":null,
            "result":null,"pre_tool_receipt":null})).collect::<Vec<_>>());
    }
    if run["batch"].as_array().map(Vec::len)!=Some(calls.len()) {
        return Err(anyhow!("Batch checkpoint length mismatch"));
    }
    let sid=run["session_id"].as_str().ok_or_else(||anyhow!("Missing session"))?.to_owned();
    let agent_id=run["agent_id"].as_str().unwrap_or("shared_default").to_owned();
    let (policy,mode_revision,visibility,ability_state)=app.ability_controls_for(&sid,run["requester_user_id"].as_str().unwrap_or(app.store.current_user_id())).await?;
    let execution_mode=app.session_mode(&sid).await?;
    let execution_mode=if run["post_run_followup"]==true {
        json!({"mode":"read_only","execution_mode":"ask","revision":execution_mode["revision"]})
    } else {execution_mode};
    let read_only=execution_mode["mode"]=="read_only";
    let saved_index=run["call_index"].as_u64().unwrap_or(0);
    for (idx,model_call) in calls.iter().enumerate() {
        if run["batch"][idx]["state"]!="new" {continue;}
        let name=model_call["name"].as_str().unwrap_or("").to_owned();
        let args=match model_call["arguments"].as_str().map(serde_json::from_str::<Value>) {
            Some(Ok(v)) if v.is_object()=>v,
            _=>json!({"invalid_arguments":true}),
        };
        run["call_index"]=json!(idx);
        workflow::record(&mut run,workflow_policy,&workflow::Input::PreTool(workflow::PreToolInput {
            name:name.clone(),arguments_digest:digest(&args),
            policy_version:policy["version"].as_u64().unwrap_or(0),mode_revision,
            visibility_revision:visibility["revision"].as_u64().unwrap_or(0),
            ability_state_revision:ability_state["revision"].as_u64().unwrap_or(0),
        }),workflow::Decision::Pass)?;
        let receipt=run["workflow"]["receipts"]["pre_tool"].clone();
        let mut pending=json!({"id":uuid(),"name":name,"arguments":args,"call_id":model_call["call_id"],
            "digest":digest(&args),"policy_version":policy["version"],"mode_revision":mode_revision,
            "visibility_revision":visibility["revision"],"ability_state_revision":ability_state["revision"],
            "access_revision":access.revision.clone(),"state":"requested","decision":null});
        tool_execution::resolve::<A>(&visibility,&name)?;
        let mut effective=app.live_tool_decision(access,&name,&policy,&visibility,
            &ability_state,&run["loaded_abilities"],read_only).for_execution_mode(&execution_mode);
        let mut result=if run["post_run_followup"]==true
            && !tool_execution::background_capable(&name) {
            Some(denied("Tool is unavailable during a background follow-up"))
        } else if args.get("invalid_arguments").is_some()
            || effective.permission==access::Permission::Deny {
            Some(denied("Tool invocation denied by policy or invalid arguments"))
        } else {None};
        if result.is_none() {
            let mut view=run.clone();view["pending"]=pending.clone();
            result=match app.actions.preflight(app.store.records(),&mut view,&name,&args,&visibility).await {
                Ok(result)=>result,
                Err(_)=>Some(denied("Invalid action metadata or unavailable action record")),
            };
            if result.is_none() && app.native.messaging_preflight(app.native_view(),&mut view,&name,&args).await.is_err(){
                result=Some(denied("Private messaging destination or authority unavailable"));
            }
            pending=view["pending"].clone();
            if pending["new_intent_confirmation"]==true {effective.permission=access::Permission::Ask;}
        }
        let state=if result.is_some() {"done"}
            else if effective.permission==access::Permission::Ask {"waiting"}
            else {"requested"};
        run["batch"][idx]=json!({"state":state,"pending":pending,"result":result,
            "pre_tool_receipt":receipt});
    }
    run["call_index"]=json!(saved_index);

    // APPROVAL-IDENTITY: only the matching call consumes the administrator's
    // decision. Other calls retain distinct IDs, revisions, and arguments.
    // A mode switch revokes the old approval and queues this run. Convert only
    // calls that had not dispatched; their other policy revisions are checked
    // again below before any worker sees them.
    if execution_mode["mode"]=="write" && execution_mode["execution_mode"]=="auto" {
        for item in run["batch"].as_array_mut().unwrap() {
            if item["state"]!="waiting" || item["pending"]["new_intent_confirmation"]==true {continue;}
            item["state"]=json!("requested");
            item["pending"]["id"]=json!(uuid());
            item["pending"]["mode_revision"]=json!(mode_revision);
            item["pending"]["decision"]=Value::Null;
        }
        run["pending"]=Value::Null;
    }
    if !run["pending"].is_null() {
        let approval_id=run["pending"]["id"].clone();
        for idx in 0..calls.len() {
            if run["batch"][idx]["pending"]["id"]!=approval_id {continue;}
            match run["pending"]["decision"].as_str() {
                Some("approve")=>{
                    run["batch"][idx]["pending"]["decision"]=json!("approve");
                    run["batch"][idx]["state"]=json!("requested");
                    run["pending"]=Value::Null;
                },
                Some("deny")=>{
                    app.actions.not_dispatched(app.store.records(),&run,&run["batch"][idx]["pending"]).await?;
                    run["batch"][idx]["result"]=denied("Tool approval denied");
                    run["batch"][idx]["state"]=json!("done");
                    run["pending"]=Value::Null;
                },
                _=>{},
            }
            break;
        }
    }

    let mut dispatch=Vec::new();
    let fresh_access=app.native.installation_access(app.store.records(),app.store.current_user_id(),&agent_id).await?;
    for idx in 0..calls.len() {
        if run["batch"][idx]["state"]!="requested" {continue;}
        let pending=run["batch"][idx]["pending"].clone();
        let name=pending["name"].as_str().unwrap_or("").to_owned();
        let args=pending["arguments"].clone();
        let effective=app.live_tool_decision(&fresh_access,&name,&policy,&visibility,
            &ability_state,&run["loaded_abilities"],read_only).for_execution_mode(&execution_mode);
        let fresh=pending["policy_version"]==policy["version"]
            && pending["access_revision"]==json!(fresh_access.revision.clone())
            && pending["mode_revision"]==json!(mode_revision)
            && pending["visibility_revision"]==visibility["revision"]
            && pending["ability_state_revision"]==ability_state["revision"];
        tool_execution::resolve::<A>(&visibility,&name)?;
        if !fresh || (run["post_run_followup"]==true
                && !tool_execution::background_capable(&name))
            || effective.permission==access::Permission::Deny
            || (effective.permission==access::Permission::Ask && pending["decision"]!="approve") {
            run["batch"][idx]["result"]=denied("Tool authorization changed before parallel dispatch");
            run["batch"][idx]["state"]=json!("done");
            continue;
        }
        // PRE-TOOL-LEASE: an approved call may resume under a new generation.
        // Bind its receipt to the exact dispatch that is about to occur.
        run["call_index"]=json!(idx);
        workflow::record(&mut run,workflow_policy,&workflow::Input::PreTool(workflow::PreToolInput {
            name:name.clone(),arguments_digest:digest(&args),
            policy_version:policy["version"].as_u64().unwrap_or(0),mode_revision,
            visibility_revision:visibility["revision"].as_u64().unwrap_or(0),
            ability_state_revision:ability_state["revision"].as_u64().unwrap_or(0),
        }),workflow::Decision::Pass)?;
        run["batch"][idx]["pre_tool_receipt"]=run["workflow"]["receipts"]["pre_tool"].clone();
        run["call_index"]=json!(saved_index);
        run["batch"][idx]["state"]=json!("executing");
        run["batch"][idx]["pending"]["failure"]=Value::Null;
        run["batch"][idx]["pending"]["state"]=json!("executing");
        run["batch"][idx]["pending"]["human_approved"]=json!(pending["decision"]=="approve");
        run["batch"][idx]["pending"]["decision"]=json!("consumed");
        dispatch.push((idx,name,args,pending["id"].as_str().unwrap_or("").to_owned(),
            pending["policy_version"].as_u64().unwrap_or(0)));
    }
    run["pending"]=Value::Null;
    app.save(&mut run).await?;
    if !dispatch.is_empty() {
        drop(guard);
        let mut identities=std::collections::HashMap::new();
        let mut futures=JoinSet::new();
        for (idx,name,args,pending_id,policy_version) in dispatch {
            let task_app=app.clone();let task_sid=sid.clone();let task_run=id.to_owned();
            let task_cancel=cancel.clone();
            let task_generation=generation.to_owned();
            let identity=(idx,name.clone(),pending_id.clone(),std::time::Instant::now());
            let handle=futures.spawn(async move {
                let result=task_app.call(task_sid,task_run,task_generation,name.clone(),args,
                    pending_id.clone(),policy_version,task_cancel).await;
                (idx,name,pending_id,result)
            });
            identities.insert(handle.id(),identity);
        }
        while let Some(completed)=futures.join_next_with_id().await {
            // Dropping the JoinSet aborts outstanding tasks. Durable executing
            // checkpoints remain authoritative for safe-read/uncertain-write recovery.
            let (idx,name,pending_id,result)=match completed {
                Ok((task_id, result))=>{identities.remove(&task_id);result},
                Err(error)=>{
                    let started=identities.get(&error.id()).map(|(_,_,_,started)|*started).unwrap_or_else(std::time::Instant::now);
                    let failure=tools::failure::task_error(Some(&error),started);
                    if let Some((idx,_,pending_id,_))=identities.remove(&error.id()) {
                        let _guard=app.handles.gate.lock().await;
                        let mut current=app.run(id).await?;
                        if current["generation"]==generation && current["batch"][idx]["pending"]["id"]==pending_id {
                            tools::failure::record(&mut current,&failure,Some(idx));
                            app.save(&mut current).await?;
                        }
                    }
                    return Err(failure);
                },
            };
            let _result_guard=app.handles.gate.lock().await;
            let mut current=app.run(id).await?;
            if current["generation"]!=generation
                || current["batch"][idx]["pending"]["id"]!=pending_id {continue;}
            if let Err(error)=&result {tools::failure::record(&mut current,error,Some(idx));}
            // A definite refusal had no effect; it completes as an error, never as uncertain.
            let refused=if result.is_err() {tools::failure::refused_before_dispatch(&current["batch"][idx]["pending"]["failure"])} else {None};
            let interrupted=refused.is_none() && match &result {
                Ok(value)=>value["uncertain"]==true || matches!(value["status"].as_str(),Some("cancelled"|"timeout"|"uncertain")),
                Err(_)=>true,
            };
            if interrupted && !A::is_read(&name) {
                current["batch"][idx]["state"]=json!("uncertain");
                current["uncertain"]=json!(true);
                current["status"]=json!("recovery");
                current["error"]=json!("Parallel write outcome unknown; review each call");
            } else {
                current["batch"][idx]["result"]=match result {
                    Ok(value)=>{current["batch"][idx]["execution_metadata"]=serde_json::to_value(&value.metadata)?;value.content},
                    Err(_)=>refused.as_ref().map(tools::failure::refused_result)
                        .unwrap_or_else(||json!({"status":"error","output":"Read tool interrupted"})),
                };
                current["batch"][idx]["state"]=json!("done");
                if current["status"]=="recovery" && uncertain_count(&current)==0 {
                    current["uncertain"]=json!(false);
                    current["status"]=json!("stopped");
                }
            }
            // STORAGE-RESULT-COMMIT: a completed write is durable before the
            // next future is awaited, so recovery only reviews unresolved calls.
            app.save(&mut current).await?;
        }
        let _approval_guard=app.handles.gate.lock().await;
        let mut current=app.run(id).await?;
        if current["generation"]!=generation || current["status"]!="running"
            || uncertain_count(&current)>0 || cancel.is_cancelled() {
            return Ok(());
        }
        if let Some(item)=current["batch"].as_array().unwrap().iter().find(|item|item["state"]=="waiting") {
            current["pending"]=item["pending"].clone();
            current["pending"]["state"]=json!("waiting");
            current["status"]=json!("waiting_approval");
            let mut payload=current["pending"].clone();payload["run_id"]=json!(id);
            app.store.save_run(&mut current,"approval_required",&payload).await?;
        }
        return Ok(());
    }
    if let Some(item)=run["batch"].as_array().unwrap().iter().find(|item|item["state"]=="waiting") {
        run["pending"]=item["pending"].clone();
        run["pending"]["state"]=json!("waiting");
        run["status"]=json!("waiting_approval");
        let mut payload=run["pending"].clone();payload["run_id"]=json!(id);
        app.store.save_run(&mut run,"approval_required",&payload).await?;
        return Ok(());
    }
    if run["batch"].as_array().unwrap().iter().all(|item|item["state"]=="done") {
        // STORAGE-ORDER: save one output at a time. The store indexes the last
        // appended output of each save; call_index makes restart idempotent.
        for idx in saved_index as usize..calls.len() {
            let item=run["batch"][idx].clone();
            run["pending"]=item["pending"].clone();
            run["workflow"]["receipts"]["pre_tool"]=item["pre_tool_receipt"].clone();
            if !item["execution_metadata"].is_null(){
                let metadata=serde_json::from_value(item["execution_metadata"].clone())?;tools::outcome::stage(&mut run,&metadata)?;
            }
            Self::append_tool(&mut run,&item["result"])?;
            let call_id=item["pending"]["call_id"].as_str().unwrap();
            let result_receipt=run["workflow"]["receipts"]["tool_result"].clone();
            run["workflow"]["batch_receipts"][call_id]=json!({
                "pre_tool":item["pre_tool_receipt"],"tool_result":result_receipt,
            });
            app.store.save_run(&mut run,"tool_result",
                &json!({"run_id":id,"name":item["pending"]["name"],"result":item["result"]})).await?;
        }
        run["batch"]=Value::Null;
        run["phase"]=json!("model");
        app.save(&mut run).await?;
        return Ok(());
    }
    Ok(())
}



}
#[cfg(test)]
mod progress_tests {
    use super::*;
    use futures_util::{future::BoxFuture, stream::FuturesUnordered, StreamExt};
    use tokio::sync::oneshot;

    fn contending_calls(gate: Arc<Mutex<()>>) -> (Vec<BoxFuture<'static, usize>>, oneshot::Sender<()>) {
        let (held_tx, held_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let ready: BoxFuture<'static, usize> = Box::pin(async move {
            held_rx.await.unwrap();
            1
        });
        let holding: BoxFuture<'static, usize> = Box::pin(async move {
            let guard = gate.lock().await;
            held_tx.send(()).unwrap();
            release_rx.await.unwrap();
            drop(guard);
            2
        });
        (vec![ready, holding], release_tx)
    }

    #[tokio::test]
    async fn parent_polled_calls_reproduce_result_commit_deadlock() {
        let gate = Arc::new(Mutex::new(()));
        let (calls, release) = contending_calls(gate.clone());
        let mut calls: FuturesUnordered<_> = calls.into_iter().collect();
        assert_eq!(calls.next().await, Some(1));
        release.send(()).unwrap();
        // A woken child owns the gate, but next() is no longer polling it.
        assert!(tokio::time::timeout(Duration::from_millis(50), gate.lock()).await.is_err());
        drop(calls);
        assert!(tokio::time::timeout(Duration::from_secs(1), gate.lock()).await.is_ok());
    }

    #[tokio::test]
    async fn parallel_result_commit_allows_gate_owner_to_finish() {
        let gate = Arc::new(Mutex::new(()));
        let (calls, release) = contending_calls(gate.clone());
        let mut calls = spawn_batch_calls(calls);
        assert_eq!(calls.join_next().await.unwrap().unwrap(), 1);
        release.send(()).unwrap();
        let guard = tokio::time::timeout(Duration::from_secs(1), gate.lock())
            .await.expect("result commit must not starve the gate owner");
        drop(guard);
        assert_eq!(calls.join_next().await.unwrap().unwrap(), 2);
    }

    #[tokio::test]
    async fn dropping_parallel_calls_aborts_gate_owner() {
        let gate = Arc::new(Mutex::new(()));
        let (calls, _release) = contending_calls(gate.clone());
        let mut calls = spawn_batch_calls(calls);
        assert_eq!(calls.join_next().await.unwrap().unwrap(), 1);
        drop(calls);
        assert!(tokio::time::timeout(Duration::from_secs(1), gate.lock()).await.is_ok());
    }
}
