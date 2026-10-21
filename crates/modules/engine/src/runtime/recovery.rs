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
use anyhow::bail;
impl<S:ScopedStorage+ModelRecords+ToolJournal+ChatEffects+AdmissionEffects+NotificationEffects+UsageEffects+RunObservation+RunPresentation,C:config::SchemaCatalog,B:Catalog,A:super::actions::Actions<S>+Clone+PartialEq,H:ChatAuthority<S>,M:ModuleBindings<S>,X:AgentPolicy<S>+NativePolicy+CredentialBindings<S>+SkillSources<S>+PromptSlots<S>+MainGuidance<S>+ModelDiagnostics<S>+InstallationAccess<S>+ContextDiagnostics<S>+ProviderUsage+StartupPolicy<S>+FollowupQueue<S>+super::native::NativeDispatch<S,C,B,H::Binding>> Runtime<S,C,B,A,H,M,X> {
// Reopen unfinished runs after interruption; hold uncertain writes for review.

pub fn reconcile_late_tool_result(run: &mut Value, result: Result<Value>) -> Result<bool> {
    if run["uncertain"] != true { return Ok(false); }
    let value = match result {
        Ok(value) if !matches!(value["status"].as_str(),
            Some("cancelled" | "timeout" | "uncertain")) => value,
        _ => return Ok(false),
    };
    Self::append_tool(run, &value)?;
    run["uncertain"] = json!(false);
    if status(run) == "recovery" {
        run["status"] = json!(if run["error"] == "Cancelled by user" {
            "cancelled"
        } else {
            "stopped"
        });
    }
    Ok(true)
}


fn pending_build_finish(run:&Value)->bool {
    matches!(run["pending"]["name"].as_str(),Some("v3_build_work"|"build_work"))
        && run["pending"]["arguments"]["action"]=="finish"
        && run["pending"]["state"]=="executing"
}
pub fn attach_external_build_result(run:&mut Value,result:&Value)->Result<()> {
    if status(run)!="waiting_external" || !Self::pending_build_finish(run)
        || run["pending"]["external_job"]["kind"]!="v3_build" {
        bail!("No pending external build result");
    }
    Self::append_tool(run,result)?;
    run["status"]=json!("queued");
    run["error"]=Value::Null;
    Ok(())
}
async fn build_result(app:&Self,run:&Value)->Result<Option<Value>> {
    let sid=run["session_id"].as_str().ok_or_else(||anyhow!("Missing session"))?;
    let rid=run["id"].as_str().ok_or_else(||anyhow!("Missing run"))?;
    let task=run["pending"]["arguments"]["task_id"].as_str()
        .ok_or_else(||anyhow!("Missing build task"))?;
    let authority=if run["pending"]["name"]=="build_work" {"build_work_finish"} else {"v3_build_work_finish"};
    let fingerprint=app.native.host_fingerprint(app.native_view(),sid,rid,authority).await?;
    let trusted=json!({"user_id":app.store.current_user_id(),"session_id":sid,
        "lease_until":now()+30,"access_fingerprint":fingerprint});
    let socket_path=app.native.recovery_build_socket(app.native_view());let socket=&socket_path;
    let mut job_id=run["pending"]["external_job"]["job_id"].as_str().map(str::to_owned);
    if job_id.is_none() {
        // The broker may have admitted finish just before the loop stopped.
        // Resolve its task receipt; never submit another finish.
        let status=crate::tools::transport::execute_host_trusted("v3_build_work",&json!({"action":"status","task_id":task}),sid,
            socket,Some(trusted.clone()),CancellationToken::new()).await?;
        if status["status"]!="success" {bail!("Build task receipt unavailable");}
        let item=status["output"]["own"].as_array().and_then(|items|items.iter()
            .find(|item|item["task_id"]==task));
        let Some(item)=item else {
            return Ok(Some(json!({"status":"error","output":{
                "task_id":task,"status":"not_submitted","reason":"Task receipt unavailable; no build was submitted"}})));
        };
        if item["inputs_sha256"]!=run["pending"]["arguments"]["expected_inputs_sha256"]
            && item["status"]=="closed" {bail!("Build task digest changed");}
        job_id=item["job_id"].as_str().map(str::to_owned);
        if job_id.is_none() && item["status"]=="open" {
            return Ok(Some(json!({"status":"error","output":{
                "task_id":task,"status":"not_submitted","reason":"Task remains open; no build was submitted"}})));
        }
        if job_id.is_none() {return Ok(None);}
    }
    let job_id=job_id.unwrap();
    let response=crate::tools::transport::execute_host_trusted("v3_worker_job",&json!({"job_id":job_id}),sid,
        socket,Some(trusted.clone()),CancellationToken::new()).await?;
    if response["status"]!="success" {bail!("Build job receipt unavailable");}
    let job=&response["output"];
    let state=job["status"].as_str().unwrap_or("");
    if !matches!(state,"completed"|"failed"|"stale"|"cancelled"|"uncertain"|"interrupted"|"needs_decision") {
        return Ok(None);
    }
    let mut output=json!({"task_id":task,"job_id":job_id,"status":state,
        "phase":job["phase"],"reason":job["reason"],"log":job["log"],"synchronization":job["synchronization"]});
    if state=="completed" {
        let health=crate::tools::transport::execute_host_trusted("v3_build_status",&json!({}),sid,
            socket,Some(trusted),CancellationToken::new()).await?;
        if health["status"]!="success" {return Ok(None);}
        let containers=&health["output"]["containers"];
        output["containers"]=containers.clone();
        output["verified_healthy"]=json!(["api","loop"].iter().all(|role|
            containers[*role]["state"]=="running" && containers[*role]["health"]=="healthy"));
    }
    Ok(Some(json!({"status":"success","output":output})))
}
// After a restart, put safe unfinished work back in the queue. An interrupted
// external write is held for review because its effect may already have happened.
pub async fn recover(app: &Self, startup: bool) -> Result<()> {
    let _guard = app.handles.gate.lock().await;
    // Only boot recovery obeys the optional resume control. Uncertain effects
    // are always classified, and ordinary lease recovery retains its behavior.
    let resume = if !startup {true} else {
        match app.native.startup_resume(app.store.records()).await {
            Ok(enabled) => enabled,
            Err(_) => {
                tracing::error!("Startup resume configuration unavailable; safe interrupted runs remain stopped");
                false
            }
        }
    };
    for user_app in app.user_views().await? {
    for view in user_app.store.active_runs().await? {
        let id = view["id"].as_str().unwrap();
        if !matches!(status(&view),"running"|"waiting_external")
            || (!startup && app.active.lock().await.contains_key(id)) {
            continue;
        }
        let mut run = user_app.run(id).await?;
        if status(&run)=="waiting_external" || (status(&run)=="running" && Self::pending_build_finish(&run)) {
            if status(&run)=="running" {
                run["pending"]["external_job"]=json!({"kind":"v3_build",
                    "task_id":run["pending"]["arguments"]["task_id"],
                    "expected_inputs_sha256":run["pending"]["arguments"]["expected_inputs_sha256"],
                    "submitted_at":now()});
                run["status"]=json!("waiting_external");
                run["error"]=Value::Null;
                user_app.save_override(&mut run).await?;
            }
            if !resume || app.shutoff().await? || !user_app.run_model_authorized(&run).await {
                run["status"]=json!("stopped");
                run["error"]=json!("Build result awaits authorized resume");
                user_app.save_override(&mut run).await?;
                continue;
            }
            match tokio::time::timeout(Duration::from_secs(3),Self::build_result(&user_app,&run)).await {
                Ok(Ok(Some(result)))=>{
                    Self::attach_external_build_result(&mut run,&result)?;
                    user_app.store.save_run_override(&mut run,"tool_result",
                        &json!({"run_id":id,"name":"v3_build_work","result":result})).await?;
                },
                Ok(Err(error))=>{
                    tracing::warn!(run_id=%id,"Build receipt reconciliation deferred: {error}");
                },
                _=>{},
            }
            continue;
        }
        if status(&run)!="running" {continue;}
        if (run["pending"]["state"] == "executing"
            && !A::is_read(run["pending"]["name"].as_str().unwrap_or("")))
            || super::batch::executing_write::<A>(&run)
        {
            user_app.actions.interrupt_run(user_app.store.records(),&run).await?;
            run["status"] = json!("recovery");
            run["uncertain"] = json!(true);
            super::batch::mark_uncertain_writes::<A>(&mut run);
            run["error"] = json!("Tool outcome is uncertain after interruption");
            if user_app.actions.recover_results(user_app.store.records(),&mut run).await? {
                run["uncertain"]=json!(false);
                let permitted=resume && !app.shutoff().await? && user_app.run_model_authorized(&run).await
                    && user_app.chat.participation_allows_run(user_app.store.records(),&run).await?;
                run["status"]=json!(if permitted {"queued"} else {"stopped"});
                run["error"]=json!("Recovered committed action result without replaying its effect");
            }
        } else {
                let allowed=user_app.run_model_authorized(&run).await;
            run["status"] = json!(if !resume || app.shutoff().await? || !allowed {
                "stopped"
            } else {
                "queued"
            });
            if run["pending"]["state"] == "executing" {
                run["pending"]["state"] = json!("requested");
            }
            if let Some(items)=run["batch"].as_array_mut() {
                for item in items { if item["state"]=="executing" {item["state"]=json!("requested");item["pending"]["state"]=json!("requested");} }
            }
            run["error"] = json!(if !resume {"Automatic startup resume is disabled; explicit resume required"} else {"Recovered after interruption"});
        }
        user_app.save_override(&mut run).await?;
    }
    }
    Ok(())
}

}
