//! Dispatch one already authorized call. Policy and approval stay with the loop.
use crate::*;
pub(crate) async fn call(
    app:App,sid:String,run_id:String,generation:String,name:String,mut args:Value,pending_id:String,
    policy_version:u64,cancel:CancellationToken,
)->Result<Value> {
    let visibility=session_agent_abilities(&app,&sid).await?;
    let settings=crate::tool_execution::resolve(&visibility,&name)?;
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
    let run=app.run(&run_id).await?;
    if run["post_run_followup"]==true {
        if !crate::tool_execution::background_capable(&name) {
            return Ok(json!({"status":"denied","output":"Post-run follow-up permits vetted reads only"}));
        }
        return call_foreground(app,sid,run_id,generation,name,args,pending_id,policy_version,cancel).await;
    }
    if crate::background_tasks::owns(&name) {
        return crate::background_tasks::control(&app,&sid,&run_id,&name,&args,cancel).await;
    }
    if mode=="background" {
        if !crate::tool_execution::background_capable(&name) {
            return Ok(json!({"status":"denied","output":"Tool does not support background execution"}));
        }
        return crate::background_tasks::submit(&app,&sid,&run_id,&generation,&name,&args,&pending_id,policy_version,&settings).await;
    }
    call_foreground(app,sid,run_id,generation,name,args,pending_id,policy_version,cancel).await
}

pub(crate) async fn call_foreground(
    app:App,sid:String,run_id:String,generation:String,name:String,args:Value,pending_id:String,
    policy_version:u64,cancel:CancellationToken,
)->Result<Value> {
    let agent_id=session_agent_id(&app,&sid).await?;
    let visibility=session_agent_abilities(&app,&sid).await?;
    let settings=crate::tool_execution::resolve(&visibility,&name)?;
    let total_budget=loop_runtime::Config::read(&app.runtime_path)?.tools.max_parallel_calls;
    let _permit=crate::tool_execution::acquire_with_budget(app.store.current_user_id(),&agent_id,&name,&settings,&cancel,total_budget).await?;
    if loop_runtime::Config::read(&app.runtime_path)?.tools.max_parallel_calls!=total_budget
        || crate::tool_execution::resolve(&session_agent_abilities(&app,&sid).await?,&name)?!=settings {
        return Ok(json!({"status":"denied","output":"Tool execution limits changed while queued; no operation dispatched"}));
    }
    if cancel.is_cancelled() || app.shutoff().await? {anyhow::bail!("Tool cancelled before execution");}
    {
        let _guard=app.gate.lock().await;
        let background=crate::background_tasks::dispatch_authorized(&app,&sid,&run_id,&generation,&pending_id,&name,&args).await?;
        let authorized=match background {
            Some(authorized)=>authorized,
            None=>{
                let run=app.run(&run_id).await?;
                let pending=if run["pending"]["id"]==pending_id {Some(run["pending"].clone())} else {
                    run["batch"].as_array().and_then(|items|items.iter().find(|item|item["pending"]["id"]==pending_id)).map(|item|item["pending"].clone())
                };
                if let Some(pending)=pending {
                    let (policy,mode_revision,visibility,state)=ability_controls(&app,&sid).await?;
                    let access=app.store.live_access(app.store.current_user_id(),&agent_id).await?;
                    let mut mode=session_mode(&app,&sid).await?;
                    if run["post_run_followup"]==true {mode["mode"]=json!("read_only");mode["execution_mode"]=json!("ask");}
                    let effective=live_tool_decision(&app,&access,&name,&policy,&visibility,&state,&run["loaded_abilities"],mode["mode"]=="read_only").for_execution_mode(&mode);
                    let mut expected_args=pending["arguments"].clone();
                    if let Some(object)=expected_args.as_object_mut() {object.remove("_execution_mode");}
                    run["session_id"]==sid && run["generation"]==generation && run["status"]=="running"
                        && pending["state"]=="executing" && expected_args==args
                        && pending["policy_version"]==policy["version"]
                        && pending["mode_revision"]==json!(mode_revision)
                        && pending["visibility_revision"]==visibility["revision"]
                        && pending["ability_state_revision"]==state["revision"]
                        && pending["access_revision"]==json!(access.revision.clone())
                        && access.admitted() && app.run_model_authorized(&run).await
                        && effective.permission!=access::Permission::Deny
                        && (effective.permission!=access::Permission::Ask || pending["human_approved"]==true)
                        && (run["post_run_followup"]!=true || crate::tool_execution::background_capable(&name))
                } else {false}
            },
        };
        if !authorized {return Ok(json!({"status":"denied","output":"Tool authorization changed while queued; no operation dispatched"}));}
    }
    let mut scoped=app;
    scoped.store=scoped.store.for_agent(&agent_id).await?;
    scoped.session_operation(&sid,cancel.clone(),
        call_raw(scoped.clone(),sid.clone(),run_id,generation,name,args,pending_id,policy_version,cancel)).await
}

pub(crate) async fn call_raw(
    app:App,sid:String,run_id:String,generation:String,name:String,args:Value,pending_id:String,
    policy_version:u64,cancel:CancellationToken,
)->Result<Value> {
    if name=="promotion_propose" {
        propose_promotion(&app,&sid,&pending_id,&run_id,&args,policy_version,cancel).await
    } else if matches!(name.as_str(),"prompt_slot_read"|"prompt_slot_write") {
        execute_prompt_slot(&app,&name,&args).await
    } else if name=="state_inspect" {
        match serde_json::from_value::<InspectArgs>(args) {
            Ok(query) if validate_inspect_args(&query).is_ok() =>
                inspect_state(&app,&sid,&query).await
                    .map(|output|json!({"status":"success","output":output})),
            _=>Ok(json!({"status":"denied","output":"Invalid state inspection arguments"})),
        }
    } else if tools::core_mode::owns(&name) {
        tools::core_mode::execute(&app,&sid,&run_id,&generation,&pending_id,&name,&args,cancel).await
    } else if tools::administrator::owns(&name) {
        tools::administrator::execute(&app,&sid,&run_id,&name,&args,&pending_id,policy_version,cancel).await
    } else if tools::core_task::owns(&name) {
        tools::core_task::execute(&app,&sid,&run_id,&pending_id,&name,&args,cancel).await
    } else if tools::core_self_edit::owns(&name) {
        tools::core_self_edit::execute(&app,&sid,&run_id,&name,&args,&pending_id,cancel).await
    } else if tools::core_tool_search::owns(&name) {
        tools::core_tool_search::execute(&app,&sid,&run_id,&name,&args,cancel).await
    } else if tools::core_search::owns(&name) {
        tools::core_search::execute(&app,&sid,&run_id,&name,&args,cancel).await
    } else if tools::core_compaction::owns(&name) {
        tools::core_compaction::execute(&app,&sid,&run_id,&name,&args,cancel).await
    } else if tools::core_recall::owns(&name) {
        tools::core_recall::execute(&app,&sid,&run_id,&name,&args,cancel).await
    } else if tools::core_prompt::owns(&name) {
        tools::core_prompt::execute(&app,&sid,&run_id,&name,&args,cancel).await
    } else if tools::core_skills::owns(&name) {
        // Skill reads recheck current caller, agent and bundle validity; no activation.
        tools::core_skills::execute(&app,&sid,&run_id,&name,&args,cancel).await
    } else if tools::core_session::owns(&name) {
        // The authenticated runtime supplies the session; arguments cannot choose one.
        tools::core_session::execute(&app,&sid,&name,&args,cancel).await
    } else if tools::core_utilities::owns(&name) {
        // Native utilities remain behind the same live Rust authorization as worker calls.
        tools::core_utilities::execute(&name,&args,cancel).await
    } else if tools::web_access::owns(&name) {
        tools::web_access::execute(&name,&args,cancel).await
    } else if tools::core_decision::owns(&name) {
        // The compiled adapter resolves this session's assigned Decision model and credential.
        tools::core_decision::execute(&app,&sid,&run_id,&args,cancel).await
    } else if tools::core_directory_decision::owns(&name) {
        // Rust checks file_list authority before the worker returns names to Jev.
        tools::core_directory_decision::execute(&app,&sid,&run_id,&args,cancel).await
    } else {
        tools::execute(&name,&args,&sid,&app.tool_socket,cancel).await
    }
}
