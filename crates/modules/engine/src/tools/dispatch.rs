use crate::runtime::{Runtime,config,ports::*,models::ModelRecords};
use crate::ports::*;
use crate::context::abilities::Catalog;
use crate::tools::execution::{self as tool_execution,Settings,ActionCatalog};
use crate::{now,digest,key,status};
use anyhow::{anyhow,Result};
use serde_json::{json,Value};
use tokio_util::sync::CancellationToken;
use std::time::Duration;
use webagent_access as access;
use crate::tools::{self,failure,outcome};
impl<S:ScopedStorage+ModelRecords+ChatEffects+AdmissionEffects+NotificationEffects+UsageEffects+RunObservation+RunPresentation,C:config::SchemaCatalog,B:Catalog,A:crate::runtime::actions::Actions<S>+Clone+PartialEq,H:ChatAuthority<S>,M:ModuleBindings<S>,X:AgentPolicy<S>+NativePolicy+CredentialBindings<S>+crate::runtime::native::NativeDispatch<S,C,B,H::Binding>> Runtime<S,C,B,A,H,M,X> {
pub fn call(
    self,sid:String,run_id:String,generation:String,name:String,args:Value,pending_id:String,
    policy_version:u64,cancel:CancellationToken,
)->impl std::future::Future<Output=Result<tools::outcome::Outcome>>+Send {async move {let app=self;
    let start = std::time::Instant::now();
    outcome::capture(Box::pin(app.call_inner(sid,run_id,generation,name,args,pending_id,policy_version,cancel))).await
        .map_err(|error| failure::boundary(error, failure::Stage::Preparation, failure::Delivery::PossiblySent, start))
}
}
fn call_inner(
    self,sid:String,run_id:String,generation:String,name:String,mut args:Value,pending_id:String,
    policy_version:u64,cancel:CancellationToken,
)->impl std::future::Future<Output=Result<Value>>+Send {async move {let app=self;
    let visibility=crate::send_future(app.session_agent_abilities(&sid)).await?;
    let settings=tool_execution::resolve::<A>(&visibility,&name)?;
    let requested=args.as_object_mut().and_then(|object|object.remove("_execution_mode"));
    let mode=match (settings.mode.as_str(),requested.as_ref().and_then(Value::as_str)) {
        ("foreground",None|Some("foreground"))=>"foreground",
        ("background",None|Some("background"))=>"background",
        ("model",None|Some("foreground"))=>"foreground",
        ("model",Some("background"))=>"background",
        _=>return Ok(json!({"status":"denied","output":"Execution mode is not permitted by tool configuration"})),
    };
    if requested.as_ref().is_some_and(|value|!value.is_string()) {
        return Ok(json!({"status":"denied","output":"Invalid execution mode"}));
    }
    let run=crate::send_future(app.run(&run_id)).await?;
    if !crate::send_future(app.chat.participation_allows_run(app.store.records(),&run)).await? {return Ok(json!({"status":"denied","output":"Agent is inactive or participation changed"}));}
    if run["post_run_followup"]==true {
        if !tool_execution::background_capable(&name) {
            return Ok(json!({"status":"denied","output":"Post-run follow-up permits vetted reads only"}));
        }
        return crate::send_future(app.call_foreground_value(sid,run_id,generation,name,args,pending_id,policy_version,cancel)).await;
    }
    if crate::runtime::background::owns(&name) {
        return crate::send_future(app.background_control(&sid,&run_id,&name,&args,cancel)).await;
    }
    if mode=="background" {
        if !tool_execution::background_capable(&name) {
            return Ok(json!({"status":"denied","output":"Tool does not support background execution"}));
        }
        return crate::send_future(app.background_submit(&sid,&run_id,&generation,&name,&args,&pending_id,policy_version,&settings)).await;
    }
    crate::send_future(app.call_foreground_value(sid,run_id,generation,name,args,pending_id,policy_version,cancel)).await
}
}
pub fn call_foreground(
    self,sid:String,run_id:String,generation:String,name:String,args:Value,pending_id:String,
    policy_version:u64,cancel:CancellationToken,
)->impl std::future::Future<Output=Result<tools::outcome::Outcome>>+Send {async move {let app=self;
    outcome::capture(app.call_foreground_value(sid,run_id,generation,name,args,pending_id,policy_version,cancel)).await
}
}
async fn call_foreground_value(self,sid:String,run_id:String,generation:String,name:String,args:Value,pending_id:String,
 policy_version:u64,cancel:CancellationToken)->Result<Value> {let app=self;
    let start = std::time::Instant::now();
    Box::pin(app.call_foreground_inner(sid,run_id,generation,name,args,pending_id,policy_version,cancel)).await
        .map_err(|error| failure::boundary(error, failure::Stage::Preparation, failure::Delivery::PossiblySent, start))
}
fn call_foreground_inner(
    self,sid:String,run_id:String,generation:String,name:String,args:Value,pending_id:String,
    policy_version:u64,cancel:CancellationToken,
)->impl std::future::Future<Output=Result<Value>>+Send {async move {let app=self;
    let agent_id=app.session_agent_id(&sid).await?;
    let visibility=app.session_agent_abilities(&sid).await?;
    let settings=tool_execution::resolve::<A>(&visibility,&name)?;
    let total_budget=config::Config::<C>::read(&app.runtime_path)?.tools.max_parallel_calls;
    if cancel.is_cancelled() || app.shutoff().await? {return Err(failure::Failure::new(failure::Stage::Preparation, failure::Code::Cancelled, failure::Delivery::NotSent).into());}
    // Optional-origin work holds a package lifetime lease through the effect.
    // Uninstall cannot complete while an already authorized external write is running.
    let binding_run=app.run(&run_id).await?;
    let _module_lease=match app.modules.binding_lease(app.store.records(),&binding_run).await {
        Ok(lease)=>lease,
        Err(_)=>return Ok(json!({"status":"denied","output":"Origin module unavailable; no operation dispatched"})),
    };
    if !app.foreground_authorized(&sid,&run_id,&generation,&name,&args,&pending_id).await? {
        return Ok(json!({"status":"denied","output":"Tool authorization changed; no operation dispatched"}));
    }
    let mut run=app.run(&run_id).await?;
    let mut pending=if run["pending"]["id"]==pending_id {run["pending"].clone()} else {
        run["batch"].as_array().and_then(|items|items.iter().find(|i|i["pending"]["id"]==pending_id))
            .map(|i|i["pending"].clone()).unwrap_or(Value::Null)
    };
    if pending["new_intent_confirmation"]==true && pending["human_approved"]!=true {
        return Ok(json!({"status":"denied","output":"Additional matching action needs specific human authorization"}));
    }
    if A::supported(&name) && pending["action_id"].is_null() {
        let _guard=app.handles.gate.lock().await;
        run=app.run(&run_id).await?;
        let batch_index=run["batch"].as_array().and_then(|items|items.iter().position(|i|i["pending"]["id"]==pending_id));
        let original=run["pending"].clone();
        run["pending"]=pending.clone();
        if let Some(result)=app.actions.preflight(app.store.records(),&mut run,&name,&args,&visibility).await? {return Ok(result);}
        pending=run["pending"].clone();
        if let Some(index)=batch_index {run["batch"][index]["pending"]=pending.clone();run["pending"]=original;}
        app.save(&mut run).await?;
    }
    if pending["new_intent_confirmation"]==true && pending["human_approved"]!=true {
        return Ok(json!({"status":"denied","output":"Additional matching action needs specific human authorization"}));
    }
    let claimed=app.actions.claim(app.store.records(),&run,&pending).await?;
    if let Some(claimed)=&claimed {
        if claimed["claimed"]!=true {return Ok(A::response(&claimed["action"]));}
    }
    let result=async {
        let _permit=tool_execution::acquire_with_budget::<A>(app.store.current_user_id(),&agent_id,&name,&settings,&cancel,total_budget).await?;
        if config::Config::<C>::read(&app.runtime_path)?.tools.max_parallel_calls!=total_budget
            || tool_execution::resolve::<A>(&app.session_agent_abilities(&sid).await?,&name)?!=settings
            || !app.foreground_authorized(&sid,&run_id,&generation,&name,&args,&pending_id).await?
            || cancel.is_cancelled() || app.shutoff().await? {
            return Ok(json!({"status":"denied","output":"Authorization changed before execution; no operation dispatched"}));
        }
        let mut scoped=app.clone();
        scoped.store=scoped.store.for_agent(&agent_id).await?;
        let clean=A::strip(&args);
        let execution_id=pending["action_pending_id"].as_str().unwrap_or(&pending_id).to_owned();
        #[cfg(any(test,feature="test-support"))] if name=="test_owner_purchase" {
            if let Some(claimed)=&claimed {return scoped.native.test_purchase(scoped.native_view(),&run,&claimed["action"],&clean).await;}
        }
        let started=std::time::Instant::now();
        scoped.session_operation(&sid,cancel.clone(),
            scoped.clone().call_raw(sid.clone(),run_id.clone(),generation.clone(),name.clone(),clean,execution_id,policy_version,cancel.clone())).await
            .map_err(|error| failure::boundary(error,failure::Stage::Execution,failure::Delivery::PossiblySent,started))
    }.await;
    if let Some(claimed)=claimed {return app.actions.finish(app.store.records(),&run,&claimed["action"],&result).await;}
    result
}
}
pub async fn foreground_authorized(&self,sid:&str,run_id:&str,generation:&str,name:&str,args:&Value,pending_id:&str)->Result<bool> {let app=self;
    let agent_id=app.session_agent_id(sid).await?;
        let _guard=app.handles.gate.lock().await;
        if !app.abilities.native_snapshot_current()? {return Ok(false);}
        let background=app.background_dispatch_authorized(sid,run_id,generation,pending_id,name,args).await?;
        let authorized=match background {
            Some(authorized)=>authorized,
            None=>{
                let run=app.run(run_id).await?;
                if !app.chat.participation_allows_run(app.store.records(),&run).await? {return Ok(false);}
                let pending=if run["pending"]["id"]==pending_id {Some(run["pending"].clone())} else {
                    run["batch"].as_array().and_then(|items|items.iter().find(|item|item["pending"]["id"]==pending_id)).map(|item|item["pending"].clone())
                };
                if let Some(pending)=pending {
                    if !app.native.valid(app.store.records(),&run,&pending).await? || (!pending["authorization"].is_null() && pending["human_approved"]!=true) {return Ok(false);}
                    let (policy,mode_revision,visibility,state)=app.ability_controls_for(sid,run["requester_user_id"].as_str().unwrap_or(app.store.current_user_id())).await?;
                    let access=app.chat.live_access(app.store.records(),run["requester_user_id"].as_str().unwrap_or(app.store.current_user_id()),sid,&agent_id).await?;
                    let mut mode=app.session_mode(sid).await?;
                    if run["post_run_followup"]==true {mode["mode"]=json!("read_only");mode["execution_mode"]=json!("ask");}
                    let effective=app.live_tool_decision(&access,name,&policy,&visibility,&state,&run["loaded_abilities"],mode["mode"]=="read_only").for_execution_mode(&mode);
                    let mut expected_args=pending["arguments"].clone();
                    if let Some(object)=expected_args.as_object_mut() {object.remove("_execution_mode");}
                    run["session_id"]==sid && run["generation"]==generation && run["status"]=="running"
                        && pending["state"]=="executing" && expected_args==*args
                        && pending["policy_version"]==policy["version"]
                        && pending["mode_revision"]==json!(mode_revision)
                        && pending["visibility_revision"]==visibility["revision"]
                        && pending["ability_state_revision"]==state["revision"]
                        && pending["access_revision"]==json!(access.revision.clone())
                        && access.admitted() && app.run_model_authorized(&run).await
                        && effective.permission!=access::Permission::Deny
                        && (effective.permission!=access::Permission::Ask || pending["human_approved"]==true)
                        && (run["post_run_followup"]!=true || tool_execution::background_capable(name))
                } else {false}
            },
        };
        Ok(authorized)

}
}
