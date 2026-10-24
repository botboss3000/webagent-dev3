//! Compiled Administrator contracts. Model arguments never choose identity,
//! agent, workspace, credentials, host origin, or a Docker command.
pub(crate) fn supported_schema_name(name:&str)->bool {
    crate::source_control::owns(name) || matches!(name,"code_index_scan"|"code_index_lookup"|"code_index_coverage"|"code_index_trace"|"code_index_progress"|"git_tool"|"resolve_conflict"|"commit_and_push"|"terminal_open"|"terminal_read"|"terminal_send"|"terminal_wait"|"terminal_list"|"terminal_close"|"v3_workspace_status"|"v3_workspace_action"|"v3_workspace_receipt"|"v3_cargo_task"|"v3_build_work"|"v3_source_list"|"v3_source_read"|"v3_source_prepare"|"v3_source_apply_prepared"|"v3_source_apply"|"v3_source_receipt"|"v3_build_status"|"v3_build_worker_restart"|"v3_build_image"|"v3_worker_submit"|"v3_worker_job"|"read_diagnostics"|"login_as_current_user"|"login_as_current_user_v3"|"dev_browser_url"|"dev_browser_action"|"dev_browser_record"|"start_tunnel"|"get_tunnel_url"|"repository_access"|"build_work"|"performance_work"|"code_index_maintenance")
}

use crate::*;
use anyhow::{bail,Result};
use serde_json::{json,Value};
use tokio_util::sync::CancellationToken;

pub async fn workspace_host(app:&App,sid:&str,name:&str,args:&Value,_trusted:Option<Value>,cancel:CancellationToken)->Result<Value>{
    let socket=build_socket(app);
    let fingerprint=digest(&json!([app.store.current_user_id(),sid,name,args]));
    let authority=||json!({"user_id":app.store.current_user_id(),"session_id":sid,"lease_until":now()+30,"access_fingerprint":fingerprint});
    let future=tools::execute_host_trusted(name,args,sid,&socket,Some(authority()),cancel.clone());
    tokio::pin!(future);
    loop {tokio::select!{
        result=&mut future=>{
            let reply=result?;
            if reply["status"]=="success" {
                if !reply["output"].is_object(){bail!("Malformed workspace service response");}
                return Ok(reply["output"].clone());
            }
            return Ok(reply);
        },
        _=cancel.cancelled()=>{
            let _=tools::execute_host_trusted("administrator_host_revoke",&json!({}),sid,&socket,Some(authority()),CancellationToken::new()).await;
            bail!("Workspace operation interrupted; inspect its receipt");
        },
        _=tokio::time::sleep(Duration::from_secs(10))=>{
            tools::execute_host_trusted("administrator_host_lease",&json!({}),sid,&socket,Some(authority()),cancel.clone()).await?;
        }
    }}
}
pub fn build_socket(app:&App)->std::path::PathBuf {app.build_socket.clone()}

pub fn schemas()->Vec<Value> {
    crate::live_assets::native_schema_snapshot_at(None).unwrap_or_else(|_| {
        tracing::error!("Native Administrator schemas unavailable; withdrawing model-facing schemas");
        Vec::new()
    })
}
pub fn owns(name:&str)->bool {
    matches!(name, "file_list"|"file_read"|"file_search"|"file_edit"|"file_write") || supported_schema_name(name)
}
fn request_id(args:&Value)->Result<&str> {
    let value=args["request_id"].as_str().unwrap_or("");
    if value.is_empty() || value.len()>80 || !value.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_'||b==b'-') {
        bail!("Canonical bounded request_id required");
    }
    Ok(value)
}
async fn browser_authority(app:&App,sid:&str)->Result<Value> {
    let uid=crate::shared_chat::actor(app);
    let account=app.store.account_by_id(uid).await?.ok_or_else(||anyhow!("Caller unavailable"))?;
    let key=format!("administrator_browser:{sid}");
    if let Some(old)=app.store.get(&key).await? {
        if old["user_id"]==uid && old["generation"]==account["generation"] && old["expires_at"].as_u64().unwrap_or(0)>now()+30 {
            return Ok(old);
        }
    }
    let generation=account["generation"].as_str().ok_or_else(||anyhow!("Caller unavailable"))?;
    let session=app.store.accounts_repository().issue_trusted_browser_session(uid,generation,now(),"V3 current-caller Administrator browser",600).await?;
    let token=session.token;let expires=session.expires;
    let trusted=json!({"session_id":sid,"user_id":uid,"cookie":token,
        "expires_at":expires,"generation":account["generation"],"agent_id":session_agent_id(app,sid).await?,"allowed_session_ids":[sid]});
    app.store.put(&key,&trusted).await?;Ok(trusted)
}
async fn worker(app:&App,sid:&str,name:&str,args:&Value,trusted:Option<Value>,cancel:CancellationToken)->Result<Value> {
    tools::execute_trusted(name,args,sid,&app.tool_socket,trusted,cancel).await
}
pub(crate) async fn host_fingerprint(app:&App,sid:&str,run_id:&str,name:&str)->Result<String> {
    if app.shutoff().await? {return Err(super::failure::Failure::new(super::failure::Stage::Authorization, super::failure::Code::AuthorityStopped, super::failure::Delivery::NotSent).into());}
    let agent=session_agent_id(app,sid).await?;
    let mut scoped=app.clone();scoped.store=app.store.for_agent(&agent).await?;
    let run=scoped.run(run_id).await?;
    if run["session_id"]!=sid {return Err(super::failure::Failure::new(super::failure::Stage::Authorization, super::failure::Code::ScopeChanged, super::failure::Delivery::NotSent).into());}
    if name=="performance_work" && !performance_run_live(&run){return Err(super::failure::Failure::new(super::failure::Stage::Authorization, super::failure::Code::AuthorityStopped, super::failure::Delivery::NotSent).into());}
    let (policy,revision,visibility,state)=ability_controls(&scoped,sid).await?;
    let mode=session_mode(&scoped,sid).await?;
    let access=crate::shared_chat::live_access(&scoped.store,crate::shared_chat::actor(&scoped),sid,&agent).await?;
    if name=="performance_work" && !crate::optimization::available(){return Err(super::failure::Failure::new(super::failure::Stage::Authorization, super::failure::Code::AuthorityStopped, super::failure::Delivery::NotSent).into())}
    let work_finish=matches!(name,"v3_build_work_finish"|"build_work_finish");
    let policy_name=if work_finish {"build_work"} else {name};
    let work_permission=live_tool_decision(&scoped,&access,policy_name,
        &policy,&visibility,&state,&run["loaded_abilities"],mode["mode"]=="read_only").for_execution_mode(&mode).permission;
    if !access.principal.administrator() || !access.admitted() || work_permission==access::Permission::Deny {
        return Err(super::failure::Failure::new(super::failure::Stage::Authorization, super::failure::Code::AuthorizationRejected, super::failure::Delivery::NotSent).into());
    }
    // Moving public names never removes the old broker authorization ceilings.
    let legacy = match name {
        "file_list"|"read_directory" => Some("v3_source_list"),
        "file_read"|"file_search"|"read_source"|"search_source" => Some("v3_source_read"),
        "file_edit"|"file_write"|"write_source"|"edit_source"|"patch_source"|"delete_source" => Some("v3_source_apply"),
        "build_work"|"build_work_finish"|"v3_build_work_finish" => Some("v3_build_work"),
        _ => None,
    };
    if let Some(legacy_name)=legacy {
        let installation=access::Permission::from_policy(policy_decision(&policy,legacy_name))
            .unwrap_or(access::Permission::Deny);
        let ceiling=access.tool_decision_for_abilities(legacy_name,&["v3_build".into()],
            installation,true,true,mode["mode"]=="read_only",tools::is_read(legacy_name))
            .for_execution_mode(&mode).permission;
        if ceiling==access::Permission::Deny || (ceiling==access::Permission::Ask && work_permission!=access::Permission::Ask) {
            return Err(super::failure::Failure::new(super::failure::Stage::Authorization, super::failure::Code::LegacyAuthorizationRejected, super::failure::Delivery::NotSent).into());
        }
    }
    if work_finish {
        let installation=access::Permission::from_policy(policy_decision(&policy,"v3_worker_submit"))
            .unwrap_or(access::Permission::Deny);
        let ceiling=access.tool_decision_for_abilities("v3_worker_submit",&["v3_build".into()],
            installation,true,true,mode["mode"]=="read_only",false).for_execution_mode(&mode).permission;
        if ceiling==access::Permission::Deny || (ceiling==access::Permission::Ask && work_permission!=access::Permission::Ask) {
            return Err(super::failure::Failure::new(super::failure::Stage::Authorization, super::failure::Code::DeploymentAuthorizationRejected, super::failure::Delivery::NotSent).into());
        }
    }
    // An Ask finish reaches this host only after explicit approval. Its queued job
    // retains that authorization while the scoped policy fingerprint stays valid.
    // A finish can replace this loop's ability bundle while its broker job is
    // still live. Policy, activation, and access remain in the fingerprint;
    // bundle bytes alone cannot revoke the job during its own deployment.
    let bundle=if work_finish {Value::Null} else {json!(scoped.abilities.live_version())};
    Ok(digest(&json!([crate::shared_chat::actor(&scoped),sid,agent,access.context_fingerprint(),
        policy,revision,visibility,state,mode,run["loaded_abilities"],bundle,
        if name=="performance_work"{crate::optimization::generation()}else{None}])))
}
fn build_work_arguments(args:&Value)->Result<Value> {
    let object=args.as_object().ok_or_else(||anyhow!("Build work arguments must be an object"))?;
    const KNOWN:&[&str]=&["action","task_id","summary","mode","expected_inputs_sha256","sync_decision","expected_sync_digest","offset"];
    if object.keys().any(|key|!KNOWN.contains(&key.as_str())) {bail!("Unknown build work argument");}
    let relevant:&[&str]=match args["action"].as_str().unwrap_or("") {
        "status"=>&["action","task_id"],
        "begin"=>&["action","task_id","summary","mode"],
        "finish"=>&["action","task_id","expected_inputs_sha256","sync_decision","expected_sync_digest"],
        "preflight"=>&["action","task_id","offset"],
        "decide"=>&["action","task_id","sync_decision","expected_sync_digest"],
        "abandon"=>&["action","task_id"],
        _=>bail!("Unknown build work action"),
    };
    let mut forwarded=serde_json::Map::new();
    for key in relevant {if let Some(value)=object.get(*key) {forwarded.insert((*key).into(),value.clone());}}
    Ok(Value::Object(forwarded))
}
fn performance_effect_action(args:&Value)->bool {
    matches!(args["action"].as_str(),Some("benchmark"|"profile_cpu"|"profile_allocations"))
}
fn performance_pending(receipt:&Value)->bool {
    receipt["status"]=="success"
        && matches!(receipt["output"]["status"].as_str(),Some("queued"|"running"))
}
fn performance_run_live(run:&Value)->bool {
    run["status"]=="running" && run["error"]!="Cancelled by user"
}

/// Cancellation/reconciliation addresses the existing durable request only.
/// It deliberately uses the admitted fingerprint rather than granting fresh
/// performance authority after the caller has cancelled or lost access.
async fn performance_cancel_existing(
    app:&App,sid:&str,forwarded:&Value,fingerprint:&str,last:Value,
)->Value {
    let trusted=json!({"user_id":crate::shared_chat::actor(app),"session_id":sid,
        "lease_until":now()+30,"access_fingerprint":fingerprint});
    let mut args=forwarded.clone();args["action"]=json!("cancel");
    let socket_path=build_socket(app);let socket=&socket_path;
    let _=tokio::time::timeout(Duration::from_secs(3),
        tools::execute_host_trusted("performance_work",&args,sid,socket,Some(trusted.clone()),
            CancellationToken::new())).await;
    args["action"]=json!("status");
    for _ in 0..3 {
        if let Ok(Ok(receipt))=tokio::time::timeout(Duration::from_secs(2),
            tools::execute_host_trusted("performance_work",&args,sid,socket,Some(trusted.clone()),
                CancellationToken::new())).await {
            if receipt["status"]=="success" && !performance_pending(&receipt) {
                return receipt;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    json!({"status":"uncertain","uncertain":true,
        "output":{"receipt":last,"cancel_requested":true,
            "reason":"Performance interruption requires inspection of the existing request; never re-admit it."}})
}

async fn performance(app:&App,sid:&str,run_id:&str,args:&Value,cancel:CancellationToken)->Result<Value>{
    if args.get("settings").is_some()||args.get("plugin_generation").is_some(){bail!("Effective performance settings are runtime owned")}
    let run=app.run(run_id).await?;
    let (_,_,visibility,_)=ability_controls(app,sid).await?;
    let settings_value=run.get("optimization_request").and_then(|v|v.get("settings"))
        .cloned().or_else(||visibility["settings"].get("optimization").cloned());
    let settings:crate::optimization_settings::OptimizationSettings=match settings_value {
        Some(value)=>serde_json::from_value(value)?,None=>Default::default()
    };
    settings.validate().map_err(anyhow::Error::msg)?;
    let mut forwarded=args.clone();
    forwarded["settings"]=serde_json::to_value(settings)?;
    forwarded["plugin_generation"]=json!(crate::optimization::generation().ok_or_else(||anyhow!("Optimization plugin unavailable"))?);
    let fingerprint=host_fingerprint(app,sid,run_id,"performance_work").await?;
    let wait=performance_effect_action(args);
    // Receipt reads and explicit cancellation cannot replace the renewal record
    // belonging to an independently executing performance job.
    if wait {
        app.store.put(&format!("administrator_host:{sid}"),&json!({"session_id":sid,
            "run_id":run_id,"tool_name":"performance_work","access_fingerprint":fingerprint})).await?;
    }
    let admitted=host(app,sid,run_id,"performance_work",&forwarded,cancel.clone()).await;
    let mut receipt=match admitted {
        Ok(value)=>value,
        Err(error) if wait=>{
            // Submission may have reached the broker. Reconcile/cancel by the
            // original identity rather than repeat an admission with new IDs.
            let result=performance_cancel_existing(app,sid,&forwarded,&fingerprint,
                json!({"status":"unknown","reason":"Performance admission acknowledgement interrupted"})).await;
            if result["status"]=="success" {return Ok(result);}
            tracing::warn!("Performance admission interrupted; existing receipt retained");
            let _=error;
            return Ok(result);
        }
        Err(error)=>return Err(error),
    };
    if !wait {return Ok(receipt);}
    let mut status_args=forwarded.clone();status_args["action"]=json!("status");
    while performance_pending(&receipt) {
        tokio::select! {
            _=cancel.cancelled()=>{
                return Ok(performance_cancel_existing(app,sid,&forwarded,&fingerprint,receipt).await);
            }
            _=tokio::time::sleep(Duration::from_secs(1))=>{}
        }
        let fresh=host_fingerprint(app,sid,run_id,"performance_work").await;
        if !fresh.is_ok_and(|value|value==fingerprint) {
            return Ok(performance_cancel_existing(app,sid,&forwarded,&fingerprint,receipt).await);
        }
        // Status uses the same request, pinned inputs and runtime settings.
        // The broker never schedules effects for a status request.
        match tokio::time::timeout(Duration::from_secs(5),
            host(app,sid,run_id,"performance_work",&status_args,cancel.clone())).await {
            Ok(Ok(value)) if value["status"]=="success"=>receipt=value,
            _=>return Ok(performance_cancel_existing(app,sid,&forwarded,&fingerprint,receipt).await),
        }
    }
    Ok(receipt)
}
async fn host(app:&App,sid:&str,run_id:&str,name:&str,args:&Value,cancel:CancellationToken)->Result<Value> {
    let fingerprint_name=if name=="build_work" && matches!(args["action"].as_str(),Some("finish"|"decide")) {"build_work_finish"} else if name=="v3_build_work" && matches!(args["action"].as_str(),Some("finish"|"decide")) {"v3_build_work_finish"} else {name};
    let started=std::time::Instant::now();
    let fingerprint=host_fingerprint(app,sid,run_id,fingerprint_name).await.map_err(|error| super::failure::boundary(error, super::failure::Stage::Authorization, super::failure::Delivery::NotSent, started)).map_err(|error|super::failure::request(error,args))?;
    let trusted=json!({"user_id":crate::shared_chat::actor(app),"session_id":sid,
        "lease_until":now()+30,"access_fingerprint":fingerprint});
    if matches!(name,"v3_build_image"|"v3_worker_submit") || (matches!(name,"v3_build_work"|"build_work") && matches!(args["action"].as_str(),Some("finish"|"decide"|"check"))) {
        app.store.put(&format!("administrator_host:{sid}"),&json!({"session_id":sid,
            "run_id":run_id,"tool_name":fingerprint_name,"access_fingerprint":fingerprint})).await?;
    }
    let mut forwarded=if name=="v3_build_work" {build_work_arguments(args)?} else {args.clone()};
    if let Some(object)=forwarded.as_object_mut(){object.retain(|_,value|!value.is_null());}
    tools::execute_host_trusted(name,&forwarded,sid,&build_socket(app),Some(trusted),cancel).await
}

fn workspace_worker_arguments(args:&Value)->Result<Value> {
    let object=args.as_object().ok_or_else(||anyhow!("Object required"))?;
    const KNOWN:&[&str]=&["request_id","action","path","offset","limit","patch","expected_review_sha256","timeout_seconds","summary"];
    if object.keys().any(|key|!KNOWN.contains(&key.as_str())) {bail!("Unknown workspace action argument");}
    let action=args["action"].as_str().unwrap_or("");
    let relevant:&[&str]=match action {
        "seed"|"review"=>&[],
        "read"=>&["path","offset","limit"],
        "patch"=>&["patch","expected_review_sha256"],
        "check"|"test"=>&["expected_review_sha256","timeout_seconds"],
        "propose"=>&["expected_review_sha256","summary"],
        _=>bail!("Unknown fixed workspace action"),
    };
    let mut projected=json!({"action":action});
    for key in relevant {
        if let Some(value)=object.get(*key).filter(|value|!value.is_null()) {
            projected[*key]=value.clone();
        }
    }
    Ok(projected)
}

async fn receipt_action(app:&App,sid:&str,run_id:&str,args:&Value,pending_id:&str,policy_version:u64,cancel:CancellationToken)->Result<Value> {
    let forwarded=workspace_worker_arguments(args)?;
    let id=request_id(args)?;
    let key=format!("administrator_receipt:{sid}:{id}");
    let fingerprint=digest(args);
    {
        let _guard=app.gate.lock().await;
        if let Some(old)=app.store.get(&key).await? {
            if old["request_sha256"]!=fingerprint {bail!("request_id already used with different arguments");}
            let interrupted=old["status"]=="executing" || old["status"]=="uncertain";
            return Ok(json!({"status":if interrupted{"uncertain"}else{old["status"].as_str().unwrap_or("error")},
                "uncertain":interrupted,"output":old,"replayed_receipt":true}));
        }
        app.store.put(&key,&json!({"request_id":id,"session_id":sid,
            "request_sha256":fingerprint,"status":"executing","created_at":now(),
            "note":"If interrupted, reconcile this receipt; never replay the mutation automatically"})).await?;
    }
    let action=args["action"].as_str().unwrap_or("");
    let result=if action=="propose" {
        propose_promotion(app,sid,pending_id,run_id,&json!({
            "expected_review_sha256":args["expected_review_sha256"],"summary":args["summary"]}),policy_version,cancel).await
    } else if matches!(action,"check"|"test") {
        workspace_host(app,sid,"v3_cargo_task",&json!({"task":action,
            "expected_review_sha256":args["expected_review_sha256"],
            "timeout_seconds":args.get("timeout_seconds").filter(|value|!value.is_null()).cloned().unwrap_or(json!(600))}),None,cancel).await
    } else {
        workspace_host(app,sid,"v3_workspace_action",&forwarded,None,cancel).await
    };
    let value=match result {
        Ok(value)=>value,
        Err(_)=>json!({"status":"uncertain","output":"Operation interrupted; inspect source and receipt before another request","uncertain":true}),
    };
    let receipt=json!({"request_id":id,"session_id":sid,"request_sha256":fingerprint,
        "status":value["status"],"result":value,"finished_at":now()});
    app.store.put(&key,&receipt).await?;
    Ok(json!({"status":receipt["status"],"uncertain":receipt["result"]["uncertain"]==true,"output":receipt}))
}
fn diagnostics_target<'a>(args:&'a Value,current:&'a str)->Option<&'a str> {
    match args.get("run_id") {
        None|Some(Value::Null)=>Some(current),
        Some(Value::String(value)) if value.is_empty() || value=="current"=>Some(current),
        Some(Value::String(value))=>uuid::Uuid::parse_str(value).ok()
            .filter(|id|id.to_string()==*value).map(|_|value.as_str()),
        _=>None,
    }
}
fn diagnostics_response(selected:Option<&Value>,sid:&str)->Value {
    match selected.filter(|run|run["session_id"].as_str()==Some(sid)) {
        Some(run)=>json!({"status":"success","output":task_progress::project(run)}),
        None=>json!({"status":"error","error_kind":"diagnostics_unavailable",
            "output":"Diagnostics unavailable in this session; omit run_id or use current"}),
    }
}

/// Relative source writes enter an owned draft; explicit runtime/workspace paths retain their tools.
fn implicit_source_write(name: &str, args: &Value) -> Result<Option<String>> {
    if !matches!(name, "file_edit" | "file_write") { return Ok(None); }
    let raw = args["path"].as_str().ok_or_else(|| anyhow!("Source write path required"))?;
    let relative = if let Some(value) = raw.strip_prefix("/live-checkout/") { value }
        else if raw.starts_with('/') { return Ok(None); } else { raw };
    if relative.is_empty() || relative.len() > 240 || relative.split('/').any(|p| p.is_empty() || p == "." || p == "..") {
        bail!("Source path must be relative without traversal");
    }
    if matches!(relative.split('/').next(), Some("data" | "workspaces" | "target" | ".git" | "build-jobs" | "deployment-secrets")) {
        bail!("Use the owning runtime or workspace API for this excluded source path");
    }
    Ok(Some(relative.to_owned()))
}

/// Broker IDs select work; authenticated session identity is attached only by host().
fn broker_file_arguments(sid:&str,pending_id:&str,name:&str,args:&Value)->Result<Option<Value>> {
    let legacy=args.get("task_id").is_some_and(|v|!v.is_null());
    let selected=args.get("broker_id").is_some_and(|v|!v.is_null());
    if legacy && selected {bail!("Use broker_id or legacy task_id, not both");}
    // Ordinary reads retain the worker/background contract; private reads select returned work.
    if !legacy && !selected && !matches!(name,"file_edit"|"file_write") {return Ok(None);}
    let raw=args["path"].as_str().unwrap_or("");
    let path=raw.strip_prefix("/live-checkout/").unwrap_or(raw);
    if !legacy && !selected && raw.starts_with('/') && !raw.starts_with("/live-checkout/") {return Ok(None);}
    if selected && path.starts_with('/') {bail!("Broker ID requires a source-relative path");}
    if matches!(name,"file_edit"|"file_write") && !legacy {implicit_source_write(name,args)?;}
    let mut forwarded=args.clone();forwarded["path"]=json!(if path=="." {""} else {path});
    if (!legacy || matches!(name,"file_edit"|"file_write")) && forwarded["request_id"].is_null() {
        forwarded["request_id"]=json!(format!("broker-{}",digest(&json!([sid,pending_id]))));
    }
    Ok(Some(forwarded))
}

pub async fn execute(app:&App,sid:&str,run_id:&str,name:&str,args:&Value,pending_id:&str,policy_version:u64,cancel:CancellationToken)->Result<Value> {
    if cancel.is_cancelled(){bail!("Tool execution cancelled");}
    let agent=session_agent_id(app,sid).await?;
    let run=app.run(run_id).await?;
    if run["session_id"]!=sid || run["agent_id"].as_str().unwrap_or("shared_default")!=agent {bail!("Current run/session mismatch");}
    if crate::source_control::owns(name) {return Box::pin(crate::source_control::execute(app,sid,&run,name,args,pending_id,cancel)).await;}
    match name {
        "code_index_maintenance"=>super::code_index_maintenance::execute(app,sid,run_id,args,cancel).await,
        "code_index_lookup"=>{
            let mut result=worker(app,sid,name,args,None,cancel.clone()).await?;
            super::code_index_configuration::attach(app,args,&mut result,&cancel).await?;
            Ok(result)
        },
        "file_list"|"file_read"|"file_search"|"file_edit"|"file_write"=>{
            if let Some(draft)=broker_file_arguments(sid,pending_id,name,args)? {
                host(app,sid,run_id,name,&draft,cancel).await
            } else {
                let mut live=args.clone();
                if let Some(object)=live.as_object_mut(){
                    object.remove("task_id");object.remove("broker_id");
                    object.retain(|_,value|!value.is_null());
                    if object.contains_key("expected_inputs_sha256") || object.contains_key("request_id") {bail!("Task write arguments require task_id; live fallback denied");}
                }
                let trusted=run["asset_revision"].as_str().map(|revision|json!({"codebase_revision":revision}));
                worker(app,sid,name,&live,trusted,cancel).await
            }
        },
        "build_work"=>{
            let mut request=args.clone();
            if request["request_id"].is_null() {
                request["request_id"]=json!(format!("broker-{}",digest(&json!([sid,pending_id]))));
            }
            host(app,sid,run_id,name,&request,cancel).await
        },
        "performance_work"=>performance(app,sid,run_id,args,cancel).await,
        "repository_access"=>repository_access(app,sid,args,cancel).await,
        "read_diagnostics"=>{
            match diagnostics_target(args,run_id) {
                Some(target) if target==run_id=>Ok(diagnostics_response(Some(&run),sid)),
                Some(target)=>{
                    let selected=app.run(target).await.ok();
                    Ok(diagnostics_response(selected.as_ref(),sid))
                },
                None=>Ok(diagnostics_response(None,sid)),
            }
        },
        "dev_browser_url"=>{
            let path=args["path"].as_str().filter(|value|!value.is_empty()).map(str::to_owned).unwrap_or(format!("/app?session_id={sid}"));
            if !valid_development_path(&path,sid,&agent){bail!("Development path unavailable");}
            Ok(json!({"status":"success","output":{"url":format!("{}{path}",std::env::var("V3_PUBLIC_ORIGIN").unwrap_or_else(|_|"https://dev.webagent.live".into())),"authentication_required":true}}))
        },
        "login_as_current_user"|"login_as_current_user_v3"|"dev_browser_action"|"dev_browser_record"=>{
            worker(app,sid,name,args,Some(browser_authority(app,sid).await?),cancel).await
        },
        "v3_workspace_receipt"=>{
            let id=request_id(args)?;
            let receipt=app.store.get(&format!("administrator_receipt:{sid}:{id}")).await?
                .ok_or_else(||anyhow!("Receipt not found in current session"))?;
            Ok(json!({"status":receipt["status"],"uncertain":receipt["result"]["uncertain"]==true,"output":receipt}))
        },
        "v3_workspace_action"=>receipt_action(app,sid,run_id,args,pending_id,policy_version,cancel).await,
        "v3_build_status"|"v3_build_worker_restart"|"v3_build_image"|"v3_worker_submit"|"v3_worker_job"|"v3_build_work"|"v3_source_list"|"v3_source_read"|"v3_source_prepare"|"v3_source_apply"|"v3_source_apply_prepared"|"v3_source_receipt"=>host(app,sid,run_id,name,args,cancel).await,
        "git_tool"|"resolve_conflict"|"commit_and_push"=>{
            let trusted=crate::api::repository::transport(app,sid).await?;
            if live_git_operation(name,args)? {
                let access=crate::shared_chat::live_access(&app.store,crate::shared_chat::actor(app),sid,&agent).await?;
                if !access.admitted() || !access.principal.administrator() {
                    bail!("Live checkout Git requires current administrator authority");
                }
                let mut request=args.clone();
                request["action"]=json!("git_tool");
                worker(app,sid,"administrator_file_git",&request,
                    Some(json!({"repository":trusted})),cancel).await
            } else {
                worker(app,sid,name,args,Some(trusted),cancel).await
            }
        },
        _=>worker(app,sid,name,args,None,cancel).await,
    }
}
/// The shared checkout is published release source. Inspection and pushing an
/// existing commit are allowed; source and index mutations remain broker owned.
fn live_git_operation(name:&str,args:&Value)->Result<bool> {
    if args["repo"]!="__root__" {return Ok(false);}
    if name!="git_tool" || !matches!(args["operation"].as_str(),
        Some("status"|"diff"|"log"|"show"|"branch_list"|"tag_list"|"push")) {
        bail!("Live checkout Git supports inspection and push only; commit through the release broker");
    }
    Ok(true)
}
pub fn valid_development_path(path:&str,sid:&str,agent:&str)->bool {
    if path.len()>1024 || !path.starts_with('/') || path.starts_with("//") || path.contains('\\') || path.chars().any(char::is_control) {return false;}
    if path=="/app" || path.starts_with("/app?") {
        return reqwest::Url::parse(&format!("{}{path}",std::env::var("V3_PUBLIC_ORIGIN").unwrap_or_else(|_|"https://dev.webagent.live".into()))).is_ok_and(|url|
            url.query_pairs().all(|(key,value)|(key!="session_id"||value==sid)&&(key!="agent"||value==agent)));
    }
    path=="/api/v3/auth/me" || path.starts_with(&format!("/api/v3/sessions/{sid}/"))
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn automatic_broker_file_selection_is_retry_stable_and_runtime_scoped() {
        let args=json!({"path":"src/app.rs","broker_id":"new"});
        let first=broker_file_arguments("session-one","call-one","file_read",&args).unwrap().unwrap();
        assert_eq!(first["broker_id"],"new");assert!(first["request_id"].is_string());
        assert_eq!(Some(first.clone()),broker_file_arguments("session-one","call-one","file_read",&args).unwrap());
        assert_ne!(first["request_id"],broker_file_arguments("session-two","call-one","file_read",&args).unwrap().unwrap()["request_id"]);
        assert!(broker_file_arguments("s","c","file_read",&json!({"path":"src/a.rs"})).unwrap().is_none());
        assert!(broker_file_arguments("s","c","file_search",&json!({"query":"fn", "regex":true,"path":"src"})).unwrap().is_none());
        assert!(broker_file_arguments("s","c","file_read",&json!({"path":"/workspaces/own/file"})).unwrap().is_none());
        assert!(broker_file_arguments("s","c","file_read",&json!({"path":"src/a.rs","broker_id":"new","task_id":"old-task"})).is_err());
        assert!(broker_file_arguments("s","c","file_read",&json!({"path":"/state/private","broker_id":"new"})).is_err());
        assert_eq!(broker_file_arguments("s","c","file_read",&json!({"path":"/live-checkout/src/a.rs","broker_id":"selected-work"})).unwrap().unwrap()["path"],"src/a.rs");
    }
    #[test] fn implicit_source_writes_are_broker_scoped() {
        assert_eq!(implicit_source_write("file_write",&json!({"path":"src/app.rs"})).unwrap(),Some("src/app.rs".into()));
        assert_eq!(implicit_source_write("file_edit",&json!({"path":"/live-checkout/ui/app.css"})).unwrap(),Some("ui/app.css".into()));
        assert_eq!(implicit_source_write("file_write",&json!({"path":"/workspaces/own/file"})).unwrap(),None);
        assert_eq!(implicit_source_write("file_read",&json!({"path":"src/app.rs"})).unwrap(),None);
        for path in ["/live-checkout/../data/key","../src/app.rs","data/config/chat.json"] {
            assert!(implicit_source_write("file_write",&json!({"path":path})).is_err());
        }
    }
    #[test] fn work_arguments_drop_irrelevant_optional_fields() {
        let zero="0".repeat(64);
        let begin=json!({"action":"begin","task_id":"v3-rust-parallel-1005a",
            "summary":"Rust test","mode":"isolated","expected_inputs_sha256":zero});
        assert_eq!(build_work_arguments(&begin).unwrap(),json!({"action":"begin",
            "task_id":"v3-rust-parallel-1005a","summary":"Rust test","mode":"isolated"}));
        assert_eq!(build_work_arguments(&json!({"action":"status","task_id":"ignored",
            "expected_inputs_sha256":zero})).unwrap(),json!({"action":"status","task_id":"ignored"}));
        let finish=json!({"action":"finish","task_id":"v3-rust-parallel-1005a",
            "expected_inputs_sha256":zero,"summary":"ignored"});
        assert_eq!(build_work_arguments(&finish).unwrap(),json!({"action":"finish",
            "task_id":"v3-rust-parallel-1005a","expected_inputs_sha256":zero}));
        assert!(build_work_arguments(&json!({"action":"begin","unknown":true})).is_err());
        let decision=json!({"action":"decide","task_id":"sync-task","sync_decision":"proceed","expected_sync_digest":zero});
        assert_eq!(build_work_arguments(&decision).unwrap(),decision);
        let preflight=json!({"action":"preflight","task_id":"sync-task","offset":40});
        assert_eq!(build_work_arguments(&preflight).unwrap(),preflight);
    }
    #[test] fn live_git_route_is_read_or_push_only(){
        assert!(live_git_operation("git_tool",&json!({"repo":"__root__","operation":"status"})).unwrap());
        assert!(live_git_operation("git_tool",&json!({"repo":"__root__","operation":"push"})).unwrap());
        assert!(!live_git_operation("git_tool",&json!({"repo":"webagent-v3","operation":"push"})).unwrap());
        assert!(live_git_operation("git_tool",&json!({"repo":"__root__","operation":"add"})).is_err());
        assert!(live_git_operation("commit_and_push",&json!({"repo":"__root__"})).is_err());
    }
    #[test] fn routes_are_current_session_and_fixed_origin(){
        assert!(valid_development_path("/app?session_id=own","own","agent-a"));
        assert!(!valid_development_path("/app?session_id=other","own","agent-a"));
        assert!(!valid_development_path("//evil/app","own","agent-a"));
        assert!(!valid_development_path("/api/v3/sessions/other/","own","agent-a"));
        assert!(!valid_development_path("/app?session_id=own&session_id=other","own","agent-a"));
    }
}

async fn repository_access(app:&App,sid:&str,args:&Value,cancel:CancellationToken)->Result<Value> {
    if args.as_object().is_none_or(|m|m.keys().any(|k|k!="verify_remote") || m.get("verify_remote").is_some_and(|v|!v.is_boolean())) {
        bail!("repository_access accepts only verify_remote boolean");
    }
    let trusted=crate::api::repository::transport(app,sid).await?;
    let fingerprint=digest(&trusted);
    let verify=args["verify_remote"].as_bool().unwrap_or(true);
    let client=reqwest::Client::builder().timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none()).build()?;
    let mut rows=Vec::new();
    for connection in trusted["connections"].as_array().ok_or_else(||anyhow!("Repository configuration invalid"))? {
        let raw=connection["url"].as_str().unwrap_or("");
        let url=reqwest::Url::parse(raw)?;
        let parts=url.path().trim_matches('/').split('/').collect::<Vec<_>>();
        let mut metadata=json!({"verified":false,"reason":if verify{"provider_metadata_unavailable"}else{"not_requested"},
            "reported_roles":Value::Null,"push_permission":"unverified"});
        if verify && url.host_str()==Some("github.com") && parts.len()==2
            && parts.iter().all(|p|!p.is_empty() && p.bytes().all(|b|b.is_ascii_alphanumeric()||b"_.-".contains(&b))) {
            let repo=parts[1].strip_suffix(".git").unwrap_or(parts[1]);
            let mut request=client.get(format!("https://api.github.com/repos/{}/{}",parts[0],repo))
                .header("user-agent","WebAgent-v3-repository-metadata").header("accept","application/vnd.github+json");
            if let Some(token)=connection["token"].as_str(){request=request.bearer_auth(token);}
            let response=tokio::select!{_=cancel.cancelled()=>bail!("Repository metadata cancelled"),r=request.send()=>r};
            if let Ok(response)=response {
                let status=response.status().as_u16();
                if response.content_length().is_some_and(|v|v>65536){metadata["reason"]=json!("metadata_too_large");}
                else {
                    let mut response=response;let mut bytes=Vec::new();
                    loop {
                        let chunk=tokio::select!{_=cancel.cancelled()=>bail!("Repository metadata cancelled"),r=response.chunk()=>r};
                        match chunk {Ok(Some(chunk)) if bytes.len()+chunk.len()<=65536=>bytes.extend_from_slice(&chunk),Ok(None)=>break,_=>{bytes.clear();break;}}
                    }
                    if status==200 {
                        if let Ok(value)=serde_json::from_slice::<Value>(&bytes) {
                            let roles=json!({"pull":value["permissions"]["pull"].as_bool(),"push":value["permissions"]["push"].as_bool(),"admin":value["permissions"]["admin"].as_bool()});
                            metadata=json!({"verified":true,"http_status":status,"reported_roles":roles,
                                "push_permission":"unverified","note":"Repository roles do not establish token write scope or branch protection permission"});
                        }
                    } else {metadata["http_status"]=json!(status);metadata["reason"]=json!("metadata_read_unverified");}
                }
            }
        }
        rows.push(json!({"connection_id":connection["id"],"url":raw,"credential_present":connection["token"].is_string(),"revision":connection["revision"],"metadata":metadata}));
    }
    if digest(&crate::api::repository::transport(app,sid).await?)!=fingerprint{bail!("Repository grants or credential references changed during read");}
    Ok(json!({"status":"success","output":{"agent_id":trusted["agent_id"],"revision":trusted["revision"],"connections":rows,
        "mutations_performed":false,"credentials_exposed":false}}))
}

#[cfg(test)]
mod workspace_argument_tests {
    use super::*;
    #[test]
    fn seed_projects_schema_optional_defaults_without_changing_receipt_input() {
        let raw=json!({"action":"seed","expected_review_sha256":"0".repeat(64),"limit":1,"offset":1,
            "patch":"","path":"","request_id":"seed-exact","summary":"","timeout_seconds":1});
        let before=digest(&raw);
        assert_eq!(workspace_worker_arguments(&raw).unwrap(),json!({"action":"seed"}));
        assert_eq!(digest(&raw),before);
        assert_eq!(workspace_worker_arguments(&json!({"action":"review","path":null})).unwrap(),json!({"action":"review"}));
    }
    #[test]
    fn read_preserves_pagination_and_rejects_unknown_keys() {
        assert_eq!(workspace_worker_arguments(&json!({"action":"read","path":"README.md","offset":2,"limit":1,"patch":""})).unwrap(),
            json!({"action":"read","path":"README.md","offset":2,"limit":1}));
        assert_eq!(workspace_worker_arguments(&json!({"action":"read","path":"README.md","offset":null,"limit":null})).unwrap(),
            json!({"action":"read","path":"README.md"}));
        assert!(workspace_worker_arguments(&json!({"action":"seed","command":"id"})).is_err());
    }
}

#[cfg(test)]
mod diagnostics_tests {
    use super::*;
    #[test]
    fn current_aliases_and_canonical_run_ids() {
        let current="11111111-1111-4111-8111-111111111111";
        for args in [json!({}),json!({"run_id":null}),json!({"run_id":""}),json!({"run_id":"current"})] {
            assert_eq!(diagnostics_target(&args,current),Some(current));
        }
        let explicit=json!({"run_id":"22222222-2222-4222-8222-222222222222"});
        assert_eq!(diagnostics_target(&explicit,current),explicit["run_id"].as_str());
        for args in [json!({"run_id":"not-a-run"}),json!({"run_id":42}),json!({"run_id":" current "})] {
            assert!(diagnostics_target(&args,current).is_none());
        }
    }
    #[test]
    fn unavailable_and_cross_session_responses_do_not_disclose_run_data() {
        let missing=diagnostics_response(None,"current-session");
        let other=json!({"session_id":"other-session","private":"never-return"});
        assert_eq!(diagnostics_response(Some(&other),"current-session"),missing);
        assert_eq!(missing["status"],"error");
        assert_eq!(missing["error_kind"],"diagnostics_unavailable");
        assert!(!missing.to_string().contains("never-return"));
    }
}

#[cfg(test)]
mod performance_foreground_tests {
    use super::*;

    #[test]
    fn durable_wait_only_waits_for_successful_nonterminal_receipts() {
        for state in ["queued","running"] {
            assert!(performance_pending(&json!({"status":"success","output":{"status":state}})));
        }
        for state in ["completed","failed","cancelled","interrupted"] {
            assert!(!performance_pending(&json!({"status":"success","output":{"status":state}})));
        }
        assert!(!performance_pending(&json!({"status":"error","output":{"status":"queued"}})));
        assert!(!performance_pending(&json!({"status":"success","output":{}})));
    }

    #[test]
    fn terminal_or_cancelled_performance_runs_cannot_renew_job_authority() {
        assert!(performance_run_live(&json!({"status":"running"})));
        for state in ["queued","waiting_approval","stopped","cancelled","completed","failed","recovery","paused"] {
            assert!(!performance_run_live(&json!({"status":state})));
        }
        assert!(!performance_run_live(&json!({"status":"running","error":"Cancelled by user"})));
    }
}


#[cfg(test)]
mod performance_renewal_registration_tests {
    use super::*;
    #[test]
    fn only_effect_actions_register_foreground_renewal_authority() {
        for action in ["benchmark","profile_cpu","profile_allocations"] {
            assert!(performance_effect_action(&json!({"action":action})));
        }
        for action in ["status","artifact","cancel","unknown"] {
            assert!(!performance_effect_action(&json!({"action":action})));
        }
        assert!(!performance_effect_action(&json!({})));
    }
}

/// Narrow UI aliases use their own tool decision plus unchanged protected source ceilings.
pub(crate) async fn ui_host(app:&App,sid:&str,rid:&str,tool:&str,operation:&str,args:&Value,cancel:CancellationToken)->Result<Value> {
    if !super::core_ui_admin::owns(tool)||!matches!(operation,"file_list"|"file_read"|"file_search"|"file_write"|"file_edit"|"v3_source_apply"){bail!("Invalid UI broker operation");}
    let fingerprint=host_fingerprint(app,sid,rid,tool).await?;
    let trusted=json!({"user_id":crate::shared_chat::actor(app),"session_id":sid,"lease_until":now()+30,"access_fingerprint":fingerprint});
    tools::execute_host_trusted(operation,args,sid,&build_socket(app),Some(trusted),cancel).await
}
