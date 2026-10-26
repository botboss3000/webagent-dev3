//! Session-scoped tool and inspection handlers owned outside Chat: state inspection, prompt slots, ability controls,
//! source review and background-task metadata. Conversation lifecycle handlers live in the Chat module.
use super::*;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InspectArgs {
    resource: String,
    after: Option<i64>,
    limit: Option<i64>,
}
pub(crate) fn validate_inspect_args(args: &InspectArgs) -> std::result::Result<(), ApiError> {
    match args.resource.as_str() {
        "catalog" | "session" if args.after.is_none() && args.limit.is_none() => Ok(()),
        "chat" if args.after.unwrap_or(0) >= 0 && (1..=50).contains(&args.limit.unwrap_or(20)) => {
            Ok(())
        }
        "runs" if args.after.is_none() && (1..=50).contains(&args.limit.unwrap_or(20)) => Ok(()),
        _ => Err(bad("Invalid state inspection resource or bounds")),
    }
}
pub(crate) fn preview(value: &Value, max_chars: usize) -> Value {
    let raw = value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string());
    let length = raw.chars().count();
    let text: String = raw.chars().take(max_chars).collect();
    json!({"text":text,"truncated":length > max_chars})
}
pub(crate) async fn inspect_state(app: &App, sid: &str, args: &InspectArgs) -> Result<Value> {
    match args.resource.as_str() {
        "catalog" => {
            app.store.session_summary(sid).await?;
            Ok(json!({"resources":[
                {"name":"session","description":"Current session title, creation time and chat cursor"},
                {"name":"chat","description":"Paginated current-session message previews","after":"nonnegative chat sequence","limit":"1-50"},
                {"name":"runs","description":"Recent current-session run status summaries","limit":"1-50"}
            ]}))
        }
        "session" => app.store.session_summary(sid).await,
        "chat" => {
            let mut page = app
                .store
                .chat_page(sid, args.after.unwrap_or(0), args.limit.unwrap_or(20))
                .await?;
            if let Some(messages) = page["messages"].as_array_mut() {
                for message in messages {
                    let content = preview(&message["content"], 512);
                    message.as_object_mut().unwrap().remove("content");
                    message["content_preview"] = content;
                }
            }
            Ok(page)
        }
        "runs" => {
            let mut runs = app
                .store
                .run_summaries(sid, args.limit.unwrap_or(20))
                .await?;
            for run in &mut runs {
                if !run["error"].is_null() {
                    let error = preview(&run["error"], 512);
                    run["error"] = error;
                }
            }
            Ok(json!({"runs":runs}))
        }
        _ => Err(anyhow!("Invalid state inspection resource")),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PromptReadArgs {
    action: String,
    slot_name: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PromptWriteArgs {
    action: String,
    slot_name: String,
    expected_revision: i64,
    content: Option<String>,
    order_index: Option<i64>,
    lock: Option<bool>,
    merge_mode: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PromptSlotPutBody {
    content: String,
    expected_revision: i64,
    order_index: Option<i64>,
    lock: Option<bool>,
    merge_mode: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PromptSlotDeleteBody {
    expected_revision: i64,
}
pub(crate) fn valid_slot_name(name: &str) -> bool {
    !name.is_empty() && name == name.trim() && name.len() <= 128 && name != "__skills__"
}
pub(crate) fn valid_prompt_read(args: &PromptReadArgs) -> bool {
    match args.action.as_str() {
        "list" => args.slot_name.is_none(),
        "get" => args.slot_name.as_deref().is_some_and(valid_slot_name),
        _ => false,
    }
}
pub(crate) fn valid_prompt_write(args: &PromptWriteArgs) -> bool {
    if !valid_slot_name(&args.slot_name)
        || args.expected_revision < 0
        || args
            .order_index
            .is_some_and(|order| !(0..=1_000_000).contains(&order))
        || args
            .merge_mode
            .as_deref()
            .is_some_and(|mode| !matches!(mode, "replace" | "append"))
    {
        return false;
    }
    match args.action.as_str() {
        "put" => args
            .content
            .as_ref()
            .is_some_and(|content| content.len() <= 16 * 1024),
        "delete" => {
            args.expected_revision > 0
                && args.content.is_none()
                && args.order_index.is_none()
                && args.lock.is_none()
                && args.merge_mode.is_none()
        }
        _ => false,
    }
}
pub(crate) fn prompt_slot_error(error: anyhow::Error) -> ApiError {
    let message = error.to_string();
    if message == "Prompt slot revision mismatch" || message == "Prompt slot not found" {
        conflict("Prompt slot changed; reload it and retry")
    } else {
        error.into()
    }
}
pub(crate) async fn execute_prompt_slot(app: &App, name: &str, args: &Value) -> Result<Value> {
    execute_prompt_slot_receipted(app,name,args,None).await
}
pub(crate) async fn execute_prompt_slot_receipted(app:&App,name:&str,args:&Value,pending:Option<&str>)->Result<Value> {
    let receipt_key=pending.map(|id|format!("agent:{}:action_prompt:{id}",app.store.current_agent_id()));
    let hash=digest(args);
    let receipt=receipt_key.as_deref().map(|key|(key,hash.as_str()));
    // A tool grant controls use; editing agent configuration also requires tenant authority.
    if name=="prompt_slot_write" && !app.store.agent_authority(crate::shared_chat::actor(app),app.store.current_agent_id()).await?
        ["can_manage"].as_bool().unwrap_or(false) {
        return Ok(json!({"status":"denied","output":"Agent administration required to edit prompts"}));
    }
    if name == "prompt_slot_read" {
        let Ok(query) = serde_json::from_value::<PromptReadArgs>(args.clone()) else {
            return Ok(json!({"status":"denied","output":"Invalid prompt slot read arguments"}));
        };
        if !valid_prompt_read(&query) {
            return Ok(json!({"status":"denied","output":"Invalid prompt slot read arguments"}));
        }
        let output = if query.action == "list" {
            json!({"slots":app.store.list_prompt_slots().await?})
        } else {
            json!({"slot":app.store.get_prompt_slot(query.slot_name.as_deref().unwrap()).await?})
        };
        Ok(json!({"status":"success","output":output}))
    } else {
        let Ok(command) = serde_json::from_value::<PromptWriteArgs>(args.clone()) else {
            return Ok(json!({"status":"denied","output":"Invalid prompt slot write arguments"}));
        };
        if !valid_prompt_write(&command) {
            return Ok(json!({"status":"denied","output":"Invalid prompt slot write arguments"}));
        }
        let result = if command.action == "put" {
            app.store
                .upsert_prompt_slot_receipted(
                    &command.slot_name,
                    command.content.as_deref().unwrap(),
                    command.order_index,
                    command.lock,
                    command.merge_mode.as_deref(),
                    command.expected_revision,
                    receipt,
                )
                .await
                .map(|slot| json!({"slot":slot}))
        } else {
            app.store
                .delete_prompt_slot_receipted(&command.slot_name, command.expected_revision,receipt)
                .await
                .map(|deleted| json!({"deleted":deleted,"slot_name":command.slot_name}))
        };
        match result {
            Ok(output) => Ok(json!({"status":"success","output":output})),
            Err(error)
                if matches!(
                    error.to_string().as_str(),
                    "Prompt slot revision mismatch" | "Prompt slot not found"
                ) =>
            {
                Ok(
                    json!({"status":"denied","output":"Prompt slot changed; read current revision and retry"}),
                )
            }
            Err(error) => Err(error),
        }
    }
}
pub(crate) async fn prompt_slots_api(State(app): State<App>, Path(id): Path<String>) -> Api {
    let app=admin_agent_view(&app,&id).await?;
    Ok(Json(json!({"slots":app.store.list_prompt_slots().await?})))
}
pub(crate) async fn prompt_slot_api(State(app): State<App>, Path((id, name)): Path<(String, String)>) -> Api {
    if !valid_slot_name(&name) {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "Prompt slot not found".into(),
        ));
    }
    let app=admin_agent_view(&app,&id).await?;
    let slot = app
        .store
        .get_prompt_slot(&name)
        .await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Prompt slot not found".into()))?;
    Ok(Json(json!({"slot":slot})))
}
pub(crate) async fn prompt_slot_put_api(
    State(app): State<App>,
    Path((id, name)): Path<(String, String)>,
    Json(body): Json<PromptSlotPutBody>,
) -> Api {
    let app=admin_agent_view(&app,&id).await?;
    let command = PromptWriteArgs {
        action: "put".into(),
        slot_name: name,
        content: Some(body.content),
        expected_revision: body.expected_revision,
        order_index: body.order_index,
        lock: body.lock,
        merge_mode: body.merge_mode,
    };
    if !valid_prompt_write(&command) {
        return Err(bad("Invalid prompt slot or revision"));
    }
    let slot = app
        .store
        .upsert_prompt_slot(
            &command.slot_name,
            command.content.as_deref().unwrap(),
            command.order_index,
            command.lock,
            command.merge_mode.as_deref(),
            command.expected_revision,
        )
        .await
        .map_err(prompt_slot_error)?;
    Ok(Json(json!({"slot":slot})))
}
pub(crate) async fn prompt_slot_delete_api(
    State(app): State<App>,
    Path((id, name)): Path<(String, String)>,
    Json(body): Json<PromptSlotDeleteBody>,
) -> Api {
    let app=admin_agent_view(&app,&id).await?;
    let command = PromptWriteArgs {
        action: "delete".into(),
        slot_name: name,
        content: None,
        expected_revision: body.expected_revision,
        order_index: None,
        lock: None,
        merge_mode: None,
    };
    if !valid_prompt_write(&command) {
        return Err(bad("Invalid prompt slot or revision"));
    }
    let deleted = app
        .store
        .delete_prompt_slot(&command.slot_name, command.expected_revision)
        .await
        .map_err(prompt_slot_error)?;
    Ok(Json(json!({"deleted":deleted})))
}
pub(crate) async fn state_inspect_api(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(args): Query<InspectArgs>,
) -> Api {
    validate_inspect_args(&args)?;
    if app.store.get(&key("session", &id)).await?.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "Session not found".into()));
    }
    let policy = app
        .store
        .get("policy")
        .await?
        .unwrap_or_else(policy_default);
    if policy_decision(&policy, "state_inspect") != "allow" {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "State inspection is unavailable under current policy".into(),
        ));
    }
    Ok(Json(inspect_state(&app, &id, &args).await?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionAbilityBody {
    pub(crate) state: String,
    pub(crate) expected_revision: u64,
}
pub(crate) async fn session_ability_set(
    State(app): State<App>,
    Path((id, ability_id)): Path<(String, String)>,
    Json(body): Json<SessionAbilityBody>,
) -> Api {
    if !app.abilities.known(&ability_id) {
        return Err(bad("Unknown ability ID"));
    }
    if !matches!(body.state.as_str(), "active" | "suppressed" | "default") {
        return Err(bad("Invalid session ability state"));
    }
    let _guard = app.gate.lock().await;
    if app.store.get(&key("session", &id)).await?.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "Session not found".into()));
    }
    let mut state = session_ability_state(&app, &id).await?;
    if state["revision"] != body.expected_revision {
        return Err(conflict("Session ability revision changed"));
    }
    let old_active = state["active"].as_array().unwrap();
    let old_suppressed = state["suppressed"].as_array().unwrap();
    let desired = body.state.as_str();
    let unchanged = match desired {
        "active" => {
            old_active
                .iter()
                .any(|v| v.as_str() == Some(ability_id.as_str()))
                && !old_suppressed
                    .iter()
                    .any(|v| v.as_str() == Some(ability_id.as_str()))
        }
        "suppressed" => {
            old_suppressed
                .iter()
                .any(|v| v.as_str() == Some(ability_id.as_str()))
                && !old_active
                    .iter()
                    .any(|v| v.as_str() == Some(ability_id.as_str()))
        }
        _ => {
            !old_active
                .iter()
                .any(|v| v.as_str() == Some(ability_id.as_str()))
                && !old_suppressed
                    .iter()
                    .any(|v| v.as_str() == Some(ability_id.as_str()))
        }
    };
    if unchanged {
        if desired=="suppressed" {phase3_api::quiesce_session_resources(&app,&id).await?;}
        return Ok(Json(state));
    }
    state["active"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v.as_str() != Some(ability_id.as_str()));
    state["suppressed"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v.as_str() != Some(ability_id.as_str()));
    if desired != "default" {
        state[if desired == "active" {
            "active"
        } else {
            "suppressed"
        }]
        .as_array_mut()
        .unwrap()
        .push(json!(ability_id));
    }
    state["revision"] = json!(state["revision"].as_u64().unwrap() + 1);
    let mut docs = vec![(key("session_ability_state", &id), state.clone())];
    for view in app.store.active_runs().await? {
        if view["session_id"] != id || view["status"] != "waiting_approval" {
            continue;
        }
        let run = invalidate_waiting_run(app.run(view["id"].as_str().unwrap()).await?);
        docs.push((key("run", run["id"].as_str().unwrap()), run));
    }
    app.store
        .atomic_docs_event(docs, &id, "session_abilities_changed", &state)
        .await?;
    phase3_api::quiesce_session_resources(&app,&id).await?;
    Ok(Json(state))
}
pub(crate) async fn session_abilities(State(app): State<App>, Path(id): Path<String>) -> Api {
    if app.store.get(&key("session", &id)).await?.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "Session not found".into()));
    }
    let (policy, _) = effective_policy(&app, &id).await?;
    let visibility = session_agent_abilities(&app, &id).await?;
    let state = session_ability_state(&app, &id).await?;
    let mode=session_mode(&app,&id).await?;
    let agent_id=session_agent_id(&app,&id).await?;
    let access=crate::shared_chat::live_access(&app.store,crate::shared_chat::actor(&app),&id,&agent_id).await?;
    let mut abilities=app.abilities.catalog_with(&policy,&visibility,&state);
    abilities.retain_mut(|entry|filter_ability_entry(&app,&access,entry,
        &policy,&visibility,&state,&json!([]),mode["mode"]=="read_only"));
    Ok(Json(json!({"mode":mode,
        "catalog_version":app.abilities.version(),
        "visibility_revision":visibility["revision"],"state":state,
        "abilities":abilities})))
}
pub(crate) async fn source_review(State(app): State<App>, Path(id): Path<String>) -> Api {
    let _guard = app.gate.lock().await;
    if app.store.get(&key("session", &id)).await?.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "Session not found".into()));
    }
    let policy = app
        .store
        .get("policy")
        .await?
        .unwrap_or_else(policy_default);
    if policy_decision(&policy, "source_review") != "allow" {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Source review is unavailable under current policy".into(),
        ));
    }
    if app.shutoff().await? {
        return Err(conflict("Global shutoff is active"));
    }
    drop(_guard);
    let result = tools::administrator::workspace_host(
        &app,
        &id,
        "source_review",
        &json!({}),
        None,
        CancellationToken::new(),
    )
    .await?;
    Ok(Json(result))
}

/// Read only caller-owned task milestones; no arguments, results or claim credentials cross this API.
pub(crate) async fn background_task_metadata_api(State(app):State<App>,Path((sid,id)):Path<(String,String)>)->Api {
 if id.len()!=67 || !id.starts_with("bg_") || !id[3..].bytes().all(|b|b.is_ascii_hexdigit()){return Err(bad("Invalid task ID"));}
 let missing=||ApiError(StatusCode::NOT_FOUND,"Task not found".into());
 let task=app.store.background_get(&id).await?.ok_or_else(missing)?;
 if task["session_id"]!=sid || task["user_id"]!=app.store.current_user_id(){return Err(missing());}
 use webagent_background_contracts::{TaskSummary,State};
 let state=match task["status"].as_str(){Some("queued")=>State::Queued,Some("running")=>State::Running,Some("completed")=>State::Completed,Some("cancelled")=>State::Cancelled,Some("failed")=>State::Failed,_=>return Err(missing())};
 let mut summary=serde_json::to_value(TaskSummary{version:1,task_id:id,session_id:sid,
  run_id:task["run_id"].as_str().ok_or_else(missing)?.into(),status:state,
  created_at_ms:task["created_at_ms"].as_u64(),claimed_at_ms:task["claimed_at_ms"].as_u64(),completed_at_ms:task["completed_at_ms"].as_u64()}).map_err(anyhow::Error::from)?;
 if let Some(value)=task.get("execution_metadata") {
  let metadata:webagent_context_contracts::execution::Metadata=serde_json::from_value(value.clone()).map_err(anyhow::Error::from)?;metadata.validate()?;
  summary["execution_metadata"]=value.clone();
 }
 Ok(Json(summary))
}

#[cfg(test)]mod background_metadata_tests {
 use super::*;
 /// Milestone reads reject another session and never return private task fields.
 #[tokio::test]async fn background_dispatch_metadata_is_content_free_and_session_fenced(){
  let (_root,app)=crate::runtime_tests::fixture().await;
  let sid=crate::runtime_tests::conversation(&app).await;let id=format!("bg_{}", "a".repeat(64));
  let task=json!({"id":id,"session_id":sid,"user_id":app.store.current_user_id(),"run_id":"run",
   "status":"completed","revision":0,"created_at_ms":1,"claimed_at_ms":2,"completed_at_ms":3,
   "arguments":{"private":"fixture-content"},"result":"fixture-content","claim_id":"fixture-claim",
   "execution_metadata":{"version":1,"rpc_calls":[{"peer":"tool-worker","id":"routing"}]}});
  assert!(app.store.background_cas(&task,None).await.unwrap());
  let value=background_task_metadata_api(State(app.clone()),Path((sid.clone(),id.clone()))).await.unwrap().0;
  assert_eq!(value["completed_at_ms"],3);assert_eq!(value.as_object().unwrap().len(),9);
  assert_eq!(value["execution_metadata"],task["execution_metadata"]);
  for key in ["arguments","result","claim_id","authorization"]{assert!(value.get(key).is_none());}
  assert_eq!(background_task_metadata_api(State(app),Path(("other".into(),id))).await.err().unwrap().0,StatusCode::NOT_FOUND);
 }
}
