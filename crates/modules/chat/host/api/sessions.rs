//! Chat-owned session HTTP handlers: search, list, create, metadata, mode selection, read cursor and lifecycle.
use super::*;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionSearchArgs {
    q: String, status: Option<String>, limit: Option<usize>, continuation: Option<String>,
}
pub(crate) async fn session_search_api(State(app): State<App>, Query(args): Query<SessionSearchArgs>) -> Api {
    session_search_scoped(app,args,None).await
}
pub(crate) async fn session_search_scoped(app:App,args:SessionSearchArgs,agent:Option<&str>)->Api {
    let mut page = app.store.search_private_sessions_scoped(&args.q, args.status.as_deref().unwrap_or("active"), args.limit.unwrap_or(20), args.continuation.as_deref(),agent).await
        .map_err(|error| if error.downcast_ref::<crate::store::search::InvalidSearch>().is_some() { bad("Invalid or expired search parameters/continuation") } else { ApiError::from(error) })?;
    let mut results = Vec::new();
    let mut admitted = std::collections::HashMap::new();
    for hit in page["results"].as_array().unwrap() {
        let agent = hit["agent_id"].as_str().unwrap_or("shared_default");
        let allowed = if let Some(allowed) = admitted.get(agent) { *allowed } else {
            let allowed = app.store.live_access(app.store.current_user_id(), agent).await.map(|a| a.admitted()).unwrap_or(false);
            admitted.insert(agent.to_owned(), allowed); allowed
        };
        if allowed { results.push(hit.clone()); }
    }
    page["results"] = json!(results);
    page["server_time"] = json!(now());
    Ok(Json(page))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionListArgs { pub status: Option<String> }
pub(crate) async fn sessions(State(app): State<App>, Query(args): Query<SessionListArgs>) -> Api {
    let recycled = match args.status.as_deref().unwrap_or("active") {
        "active" => false, "recycled" => true, _ => return Err(bad("Invalid session status")),
    };
    let mut list = app.store.ordered_sessions_status(recycled).await?;
    let actor=crate::shared_chat::actor(&app);
    let mut allowed=Vec::new();
    for row in list {let agent=row["agent_id"].as_str().unwrap_or("shared_default");
        if app.store.live_access(actor,agent).await.is_ok_and(|a|a.admitted()){allowed.push(row);}
    }
    let mut list=allowed;
    for entry in app.store.shared_sessions_for(actor).await? {
        let Some(sid)=entry["session_id"].as_str() else{continue};
        if list.iter().any(|row|row["id"]==sid){continue;}
        let Ok(scoped)=crate::shared_chat::scope(app.clone(),sid).await else{continue};
        if scoped.store.get(&key("session",sid)).await?.is_some_and(|s|s["module_silent"]==true){continue;}
        let mut summary=scoped.store.shared_read_summary(sid,actor).await?;
        if (summary["status"]=="recycled")!=recycled{continue;}
        summary["shared"]=json!(true);summary["owner_id"]=entry["owner_id"].clone();
        list.push(summary);
    }
    list.sort_by(|a,b|b["pinned"].as_bool().cmp(&a["pinned"].as_bool())
        .then_with(||b["last_active_at"].as_i64().cmp(&a["last_active_at"].as_i64()))
        .then_with(||a["id"].as_str().cmp(&b["id"].as_str())));
    Ok(Json(json!({"sessions":list,"server_time":now()})))
}
pub(crate) async fn patch_session_api(State(app): State<App>, Path(id): Path<String>,
    Json(body): Json<crate::store::session::SessionPatch>) -> Api {
    use crate::store::session::SessionPatchOutcome;
    match app.store.patch_session(&id, &body).await? {
        SessionPatchOutcome::Saved(value) => Ok(Json(value)),
        SessionPatchOutcome::NotFound => Err(ApiError(StatusCode::NOT_FOUND, "Session not found".into())),
        SessionPatchOutcome::Conflict(message) => Err(conflict(message)),
        SessionPatchOutcome::Invalid(message) => Err(bad(message)),
    }
}
pub(crate) async fn create_session(State(app): State<App>, Json(body): Json<Value>) -> Api {
    let request_id = required(&body, "client_request_id", 128)?;
    let supplied = match body.get("title") {
        None | Some(Value::Null) => None,
        Some(Value::String(title)) => Some(title.trim()),
        Some(_) => return Err(bad("Invalid session title")),
    }.filter(|title| !title.is_empty());
    let title = supplied.unwrap_or("New chat");
    if title.len() > 200 || title.chars().any(char::is_control) {
        return Err(bad("Invalid session title"));
    }
    let _guard = app.gate.lock().await;
    let receipt_key = format!("receipt:session:{request_id}");
    if let Some(receipt) = app.store.get(&receipt_key).await? {
        if receipt["digest"] != digest(&body) {
            return Err(conflict("Request identifier reused with different input"));
        }
        finish_session_creation(&app.store,&receipt["result"]).await?;
        return Ok(Json(receipt["result"].clone()));
    }
    let prepared=prepare_session(&app,&body).await?;
    let mut docs=prepared.docs;
    docs.push((receipt_key,json!({"digest":digest(&body),"result":prepared.result})));
    app.store.atomic_docs_event(docs,prepared.result["id"].as_str().unwrap(),"session_created",&prepared.result).await?;
    finish_session_creation(&app.store,&prepared.result).await?;
    Ok(Json(prepared.result))
}
/// Finalize the app-directory projection on fresh creation and receipt replay.
/// The owner-plane commit is already durable: never create a second session to recover it.
async fn finish_session_creation(store:&Store,result:&Value)->std::result::Result<(),ApiError> {
    let sid=result["id"].as_str().ok_or_else(||anyhow::anyhow!("Session creation receipt has no session ID"))?;
    let agent=result["agent_id"].as_str().ok_or_else(||anyhow::anyhow!("Session creation receipt has no agent ID"))?;
    let mut retry_busy=true;
    loop {
        match store.register_shared_session(store.current_user_id(),sid,agent).await {
            Ok(())=>return Ok(()),
            Err(error)=>{
                let code=error.downcast_ref::<sqlx::Error>().and_then(|e|e.as_database_error())
                    .and_then(|e|e.code()).map(|v|v.into_owned());
                let busy=code.as_deref().and_then(|v|v.parse::<u32>().ok())
                    .is_some_and(|v|matches!(v&255,5|6));
                if busy&&retry_busy {retry_busy=false;tokio::time::sleep(std::time::Duration::from_millis(20)).await;continue;}
                // Only fixed classifications reach logs; SQL text and session content stay private.
                tracing::warn!(operation="session_directory_registration",error_kind=if busy {"sqlite_busy"}else if code.is_some(){"sqlite_database"}else{"storage"},"Saved session directory finalization incomplete");
                diagnostics::backend_error();
                return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE,
                    "Session saved, but directory registration is unavailable. Retry with the same client_request_id.".into()));
            }
        }
    }
}
pub(crate) struct PreparedSession {
    pub docs: Vec<(String,Value)>,
    pub value: Value,
    pub result: Value,
}
pub(super) async fn prepare_session(app:&App,body:&Value) -> std::result::Result<PreparedSession,ApiError> {
    prepare_session_id(app,body,None).await
}
pub(crate) async fn prepare_session_id(app:&App,body:&Value,id:Option<String>) -> std::result::Result<PreparedSession,ApiError> {
    let supplied = match body.get("title") {
        None | Some(Value::Null) => None,
        Some(Value::String(title)) => Some(title.trim()),
        Some(_) => return Err(bad("Invalid session title")),
    }.filter(|title| !title.is_empty());
    let title = supplied.unwrap_or("New chat");
    if title.len() > 200 || title.chars().any(char::is_control) {
        return Err(bad("Invalid session title"));
    }
    let agent_id = body.get("agent_id").map(|v| v.as_str().ok_or_else(|| bad("Invalid agent ID")))
        .transpose()?.unwrap_or("shared_default");
    let access=app.store.live_access(app.store.current_user_id(),agent_id).await
        .map_err(|_|ApiError(StatusCode::FORBIDDEN,"Agent unavailable".into()))?;
    if !access.admitted() { return Err(ApiError(StatusCode::FORBIDDEN,"Agent unavailable".into())); }
    if app.store.list("session:").await?.iter().filter(|s|s["status"]!="deleted").count() >= access.max_sessions() as usize {
        return Err(conflict("Session limit reached"));
    }
    let agent_store=app.store.for_agent(agent_id).await
        .map_err(|_|bad("Agent not found"))?;
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM agents WHERE id=?")
        .bind(agent_id).fetch_optional(&agent_store.agent_pool).await.map_err(anyhow::Error::from)?;
    if exists.is_none() { return Err(bad("Agent not found")); }
    let id = id.unwrap_or_else(uuid);
    let initial_active=crate::shared_chat::policy(&app.store,agent_id).await?.initial_active;
    let privacy=agent_store.access_repository().member_access(agent_id).await?.conversation_privacy;
    if let Some(expected)=body.get("expected_conversation_privacy") {
        let expected:crate::access::membership::ConversationPrivacy=serde_json::from_value(expected.clone()).map_err(|_|bad("Invalid privacy expectation"))?;
        if expected!=privacy {return Err(conflict("Conversation privacy changed; review the notice and retry"));}
    }
    app.store.record_agent_visit(app.store.current_user_id(),agent_id).await?;
    let value = json!({"conversation_privacy":privacy,"participation":{"active":initial_active,"revision":0,"generation":0},"id":id,"agent_id":agent_id,"title":title,"checkpoint_input":[],"checkpoint_through":0,"created_at":now(),
        "auto_naming":{"version":1,"stage":if supplied.is_some() {"locked"} else {"awaiting_message"}}});
    let result = json!({"id":id,"agent_id":agent_id,"title":title});
    let mode = {
                    let config=crate::agents_api::modes_config(&app.store,agent_id).await?;
                    let chat=crate::chat_config::preferences(app,agent_id).await?;
                    let default=chat.default_mode.as_deref().unwrap_or(config["default_execution_mode"].as_str().unwrap());
                    let entry=webagent_agents::modes::resolve(&config,default).map_err(|_|bad("Unknown chat default execution mode"))?;
                    json!({"mode":entry["permission_policy"],"execution_mode":entry["id"],"revision":0})
    };
    Ok(PreparedSession { docs:vec![(key("session",&id),value.clone()),(key("session_mode",&id),mode)],value,result })
}
pub(crate) async fn snapshot(State(app): State<App>, Path(id): Path<String>) -> Api {
    let _guard = app.gate.lock().await;
    let _session = app
        .store
        .get(&key("session", &id))
        .await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Session not found".into()))?;
    if crate::shared_projection::observer(&app) {app.store.shared_observe_audit(&id,crate::shared_chat::actor(&app)).await?;}
    let (mut messages, last_seq) = app.store.chat_snapshot(&id).await?;
    uploads::message_files(&app,&mut messages).await?;
    crate::shared_projection::messages(&app,&mut messages).await?;
    crate::manager::decorate(&app,&mut messages).await?;
    let usage=crate::usage::public(&app.store.get(&format!("session_usage:{id}")).await?.unwrap_or(Value::Null));
    let runs = public_runs(&app,app.store.recent_runs(Some(&id)).await?).await?;
    let components = app.store.base_chat_components(&id).await?;
    let component_revision=app.store.base_component_revision(&id).await?;
    let app_view=tools::core_app_control::projection(&app,&id).await?;
    let queue=crate::followup_queue::public(&app.store.followup_queue(&id).await?);
    // Informational only: a session whose agent record is unreadable keeps its snapshot.
    let tools_notice=app.store.capability_levels(_session["agent_id"].as_str().unwrap_or("shared_default")).await.ok().and_then(|levels|levels.tools_notice());
    Ok(Json(
        json!({"access":{"observer":crate::shared_projection::observer(&app),"operator_reply":crate::shared_projection::observer(&app)&&_session["conversation_privacy"]=="operator_reviewable"&&_session["operator_joined"][crate::shared_chat::actor(&app)]==true,
        "can_send":!crate::shared_projection::observer(&app)||(_session["conversation_privacy"]=="operator_reviewable"&&_session["operator_joined"][crate::shared_chat::actor(&app)]==true),"can_control":!crate::shared_projection::observer(&app),"can_manage_queue":crate::shared_chat::actor(&app)==app.store.current_user_id()},"usage":usage,"session":app.store.shared_read_summary(&id,crate::shared_chat::actor(&app)).await?,
        "conversation_privacy":_session["conversation_privacy"],"messages":messages,"runs":runs,"components":components,"component_revision":component_revision,"app_view":app_view,"last_seq":last_seq,"queue":queue["queue"],"queue_state":queue["queue_state"],"tools_notice":tools_notice}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionModeBody {
    pub(crate) mode: String,
    #[serde(default)] pub(crate) expected_revision: Option<u64>,
    #[serde(default)] pub(crate) confirm_write: bool,
}
pub(crate) async fn session_mode_get(State(app): State<App>, Path(id): Path<String>) -> Api {
    if app.store.get(&key("session", &id)).await?.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "Session not found".into()));
    }
    Ok(Json(session_mode(&app, &id).await?))
}
pub(crate) async fn session_mode_set(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(body): Json<SessionModeBody>,
) -> Api {
    change_session_mode_authorized(&app,&id,&body.mode,body.expected_revision,None,body.confirm_write).await
}
#[derive(Clone, Copy)]
pub(crate) struct ModeCall<'a> { pub run_id: &'a str, pub generation: &'a str, pub pending_id: &'a str }


pub(crate) async fn change_session_mode(
    app: &App, id: &str, mode: &str, expected_revision: Option<u64>, tool: Option<ModeCall<'_>>,
) -> Api {
    change_session_mode_authorized(app,id,mode,expected_revision,tool,true).await
}
async fn change_session_mode_authorized(
    app: &App, id: &str, mode: &str, expected_revision: Option<u64>, tool: Option<ModeCall<'_>>,
    human_confirmed: bool,
) -> Api {
    let _guard = app.gate.lock().await;
    if app.store.get(&key("session", &id)).await?.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "Session not found".into()));
    }
    let current = session_mode(&app, &id).await?;
    let agent=session_agent_id(app,id).await?;
    let config=crate::agents_api::modes_config(&app.store,&agent).await?;
    let named=if matches!(mode,"write"|"read_only") {None} else {
        Some(webagent_agents::modes::resolve(&config,mode).map_err(|_|bad("Unknown execution mode"))?)
    };
    let target_policy=named.as_ref().map(|m|m["permission_policy"].as_str().unwrap()).unwrap_or(mode);
    // The agent's effective chat.json may waive only the browser confirmation for Auto.
    // Revision fencing and model-call authorization remain mandatory.
    let confirm_auto=if tool.is_none() && named.as_ref().is_some_and(|entry|entry["id"]=="auto") {
        crate::chat_config::preferences(app,&agent).await?.confirm_auto_mode
    } else { true };
    if tool.is_none() && current["mode"]=="read_only" && target_policy=="write"
        && (expected_revision.is_none() || (confirm_auto && !human_confirmed)) {
        return Err(ApiError(StatusCode::FORBIDDEN,"Confirm write-capable mode with the current revision".into()));
    }
    let receipt = tool.map(|call| {
        let key = format!("receipt:session_mode:{id}:{}",call.pending_id);
        let hash = digest(&json!([id,mode,expected_revision,call.run_id,call.generation,call.pending_id]));
        (key,hash)
    });
    if let Some((receipt_key,hash)) = &receipt {
        if let Some(saved) = app.store.get(receipt_key).await? {
            if saved["request_hash"]!=*hash { return Err(conflict("Mode call receipt mismatch")); }
            if mode=="read_only" {phase3_api::quiesce_session_resources(app,id).await?;}
            return Ok(Json(saved["result"].clone()));
        }
    }
    if let Some(call) = tool {
        if expected_revision.is_none()
            || uuid::Uuid::parse_str(call.run_id).is_err()
            || uuid::Uuid::parse_str(call.pending_id).is_err()
            || call.generation.is_empty() {
            return Err(bad("Invalid mode call identity or revision"));
        }
        let agent = session_agent_id(app,id).await?;
        let run = app.run(call.run_id).await?;
        let (policy,mode_revision,visibility,state)=ability_controls(app,id).await?;
        let access=crate::shared_chat::live_access(&app.store,crate::shared_chat::actor(app),id,&agent).await?;
        if run["session_id"]!=id || run["agent_id"]!=agent
            || run["generation"]!=call.generation
            || run["status"]!="running" || run["phase"]!="tools"
            || run["pending"]["id"]!=call.pending_id
            || run["pending"]["name"]!="set_execution_mode"
            || run["pending"]["state"]!="executing"
            || run["pending"]["policy_version"]!=policy["version"]
            || run["pending"]["mode_revision"]!=json!(mode_revision)
            || run["pending"]["visibility_revision"]!=visibility["revision"]
            || run["pending"]["ability_state_revision"]!=state["revision"]
            || run["pending"]["access_revision"]!=json!(access.revision.clone())
            || !access.admitted()
            || live_tool_decision(app,&access,"set_execution_mode",&policy,&visibility,
                &state,&run["loaded_abilities"],current["mode"]=="read_only")
                .permission==access::Permission::Deny {
            return Err(conflict("Mode call authorization changed"));
        }
        if current["mode"]=="read_only" && target_policy=="write"
            && run["pending"]["human_approved"]!=true {
            return Err(ApiError(StatusCode::FORBIDDEN,"Write mode requires approval".into()));
        }
    }
    if expected_revision.is_some_and(|revision| current["revision"].as_u64()!=Some(revision)) {
        return Err(conflict("Session mode revision changed"));
    }
    if current["mode"] == mode && named.is_none() && current["execution_mode"].is_null() {
        if mode=="read_only" {phase3_api::quiesce_session_resources(&app,&id).await?;}
        return Ok(Json(current));
    }
    let mut next = json!({"mode":target_policy,"revision":current["revision"].as_u64().unwrap()+1});
    if let Some(entry)=named {next["execution_mode"]=entry["id"].clone();}
    let mut docs = vec![(key("session_mode", &id), next.clone())];
    if let Some((receipt_key,hash)) = receipt {
        docs.push((receipt_key,json!({"request_hash":hash,"result":next})));
    }

    for view in app.store.active_runs().await? {
        if view["session_id"] != id || view["status"] != "waiting_approval" {
            continue;
        }
        let mut run = app.run(view["id"].as_str().unwrap()).await?;
        run["pending"]["state"] = json!("requested");
        run["pending"]["decision"] = Value::Null;
        run["status"] = json!("queued");
        run["updated_at"] = json!(now());
        docs.push((key("run", run["id"].as_str().unwrap()), run));
    }
    app.store
        .atomic_docs_event(docs, &id, "session_mode_changed", &next)
        .await?;
    phase3_api::quiesce_session_resources(&app,&id).await?;
    Ok(Json(next))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionReadBody { pub through_seq: u64 }
pub(crate) async fn session_read_api(State(app): State<App>, Path(id): Path<String>, Json(body): Json<SessionReadBody>) -> Api {
    use crate::store::session::SessionPatchOutcome;
    match app.store.acknowledge_session_read(&id, body.through_seq).await? {
        SessionPatchOutcome::Saved(value) => Ok(Json(value)),
        SessionPatchOutcome::NotFound => Err(ApiError(StatusCode::NOT_FOUND,"Session not found".into())),
        SessionPatchOutcome::Invalid(message) => Err(bad(message)),
        SessionPatchOutcome::Conflict(message) => Err(conflict(message)),
    }
}

pub(crate) async fn recycle_session_api(State(app): State<App>, Path(id): Path<String>, Json(body): Json<crate::store::session::SessionLifecycle>) -> Api {
    lifecycle_session_api(app,id,body,false).await
}
pub(crate) async fn purge_session_api(State(app):State<App>,Path(id):Path<String>,Json(body):Json<crate::store::session::SessionLifecycle>)->Api {
    use crate::store::session::SessionPatchOutcome;
    let _guard=app.gate.lock().await;
    let validation=crate::store::session::SessionPatch {client_request_id:body.client_request_id.clone(),
        expected_revision:body.expected_revision,title:None,pinned:Some(false)};
    validation.validate().map_err(bad)?;
    if let Some(session)=app.store.get(&key("session",&id)).await?.filter(|s|s["status"]=="recycled") {
        if session["metadata_revision"].as_u64()!=Some(body.expected_revision) {
            return Err(conflict("Session metadata revision changed"));
        }
        crate::api::voice::quiesce(&app,&id).await;
        crate::phase3_api::quiesce_session_resources(&app,&id).await?;
    }
    match app.store.purge_session(&id,&body).await? {
        SessionPatchOutcome::Saved(value)=>Ok(Json(value)),
        SessionPatchOutcome::NotFound=>Err(ApiError(StatusCode::NOT_FOUND,"Session not found".into())),
        SessionPatchOutcome::Conflict(message)=>Err(conflict(message)),
        SessionPatchOutcome::Invalid(message)=>Err(bad(message)),
    }
}
pub(crate) async fn restore_session_api(State(app): State<App>, Path(id): Path<String>, Json(body): Json<crate::store::session::SessionLifecycle>) -> Api {
    lifecycle_session_api(app,id,body,true).await
}
async fn lifecycle_session_api(app: App, id: String, body: crate::store::session::SessionLifecycle, restore: bool) -> Api {
    use crate::store::session::SessionPatchOutcome;
    let _guard=app.gate.lock().await;
    match app.store.lifecycle_session(&id,&body,restore).await? {
        SessionPatchOutcome::Saved(value) => {
            if !restore && app.store.get(&key("session",&id)).await?.is_some_and(|s|s["status"]=="recycled") {
                crate::api::voice::quiesce(&app,&id).await;
                crate::phase3_api::quiesce_session_resources(&app,&id).await?;
            }
            Ok(Json(value))
        },
        SessionPatchOutcome::NotFound => Err(ApiError(StatusCode::NOT_FOUND,"Session not found".into())),
        SessionPatchOutcome::Conflict(message) => Err(conflict(message)),
        SessionPatchOutcome::Invalid(message) => Err(bad(message)),
    }
}


#[cfg(test)]
mod silent_session_tests {
    use super::*;
    #[tokio::test]
    async fn private_execution_does_not_create_visible_or_searchable_chat() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let visible = crate::runtime_tests::conversation(&app).await;
        let silent = crate::runtime_tests::conversation(&app).await;
        let mut doc = app
            .store
            .get(&key("session", &silent))
            .await
            .unwrap()
            .unwrap();
        doc["module_silent"] = json!(true);
        doc["title"] = json!("Needle private execution");
        app.store.put(&key("session", &silent), &doc).await.unwrap();
        let listed = sessions(State(app.clone()), Query(SessionListArgs { status: None }))
            .await
            .unwrap()
            .0;
        assert!(listed["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == visible));
        assert!(!listed["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == silent));
        let searched = app
            .store
            .search_private_sessions("needle", "active", 50, None)
            .await
            .unwrap();
        assert!(searched["results"].as_array().unwrap().is_empty());
        let stats = app
            .store
            .session_statistics_page(false, 0, 100)
            .await
            .unwrap();
        assert!(!stats.0.iter().any(|s| s["id"] == silent));
        assert!(app
            .store
            .get(&key("session", &silent))
            .await
            .unwrap()
            .is_some());
    }
}

#[cfg(test)]
mod session_creation_recovery_tests {
    use super::*;
    #[tokio::test]
    async fn saved_shared_default_creation_retries_directory_without_duplicate_session_or_event() {
        let (_root,app)=crate::runtime_tests::fixture().await;
        app.store.shared_schema().await.unwrap();
        // Fail only the app-plane finalization after the owner-plane commit succeeds.
        sqlx::query("CREATE TRIGGER fixture_directory_failure BEFORE INSERT ON shared_sessions BEGIN SELECT RAISE(ABORT,'fixture directory unavailable'); END")
            .execute(&app.store.app_pool).await.unwrap();
        let body=json!({"client_request_id":"directory-recovery","agent_id":"shared_default","title":"Preserved title"});
        let error=create_session(State(app.clone()),Json(body.clone())).await.unwrap_err();
        assert_eq!(error.0,StatusCode::SERVICE_UNAVAILABLE);
        assert!(error.1.contains("same client_request_id"));
        let receipt=app.store.get("receipt:session:directory-recovery").await.unwrap().unwrap();
        let original=receipt["result"].clone();let sid=original["id"].as_str().unwrap();
        assert_eq!(app.store.get(&key("session",sid)).await.unwrap().unwrap()["title"],"Preserved title");
        assert!(app.store.shared_directory(sid).await.unwrap().is_none());
        sqlx::query("DROP TRIGGER fixture_directory_failure").execute(&app.store.app_pool).await.unwrap();
        let retried=create_session(State(app.clone()),Json(body.clone())).await.unwrap().0;
        assert_eq!(retried,original);
        let directory=app.store.shared_directory(sid).await.unwrap().unwrap();
        assert_eq!(directory["owner_id"],app.store.current_user_id());assert_eq!(directory["agent_id"],"shared_default");
        assert_eq!(create_session(State(app.clone()),Json(body.clone())).await.unwrap().0,original);
        let events:i64=sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE session_id=? AND kind='session_created'").bind(sid).fetch_one(&app.store.pool).await.unwrap();
        assert_eq!(events,1);
        let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM docs WHERE key LIKE 'session:%'").fetch_one(&app.store.pool).await.unwrap();assert_eq!(count,1);
        let mut changed=body;changed["title"]=json!("Changed retry");
        assert_eq!(create_session(State(app.clone()),Json(changed)).await.unwrap_err().0,StatusCode::CONFLICT);
    }
    #[tokio::test]
    async fn saved_session_finalization_preserves_conflicting_directory_authority() {
        let (_root,app)=crate::runtime_tests::fixture().await;
        let body=json!({"client_request_id":"directory-conflict","title":"Original"});
        let created=create_session(State(app.clone()),Json(body.clone())).await.unwrap().0;let sid=created["id"].as_str().unwrap();
        sqlx::query("UPDATE shared_sessions SET owner_id='other-owner' WHERE session_id=?").bind(sid).execute(&app.store.app_pool).await.unwrap();
        assert_eq!(create_session(State(app.clone()),Json(body.clone())).await.unwrap_err().0,StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(app.store.shared_directory(sid).await.unwrap().unwrap()["owner_id"],"other-owner");
        assert_eq!(app.store.get("receipt:session:directory-conflict").await.unwrap().unwrap()["result"],created);
    }
}

#[cfg(test)]
mod privacy_notice_tests {
    use super::*;
    #[tokio::test]
    async fn session_creation_rejects_privacy_changes_since_the_notice() {
        let (_root,app)=crate::runtime_tests::fixture().await;
        let body=|mode:&str|json!({"client_request_id":"privacy-notice","expected_conversation_privacy":mode});
        assert_eq!(create_session(State(app.clone()),Json(body("operator_reviewable"))).await.unwrap_err().0,StatusCode::CONFLICT);
        let raw:Vec<u8>=sqlx::query_scalar("SELECT metadata FROM agents WHERE id='shared_default'").fetch_one(&app.store.agent_pool).await.unwrap();
        let mut metadata=app.store.unseal_agent(&raw).unwrap();metadata["member_access"]=json!({"conversation_privacy":"operator_reviewable"});
        sqlx::query("UPDATE agents SET metadata=? WHERE id='shared_default'").bind(app.store.seal_agent(&metadata).unwrap()).execute(&app.store.agent_pool).await.unwrap();
        assert_eq!(create_session(State(app.clone()),Json(body("private"))).await.unwrap_err().0,StatusCode::CONFLICT);
        let result=create_session(State(app.clone()),Json(body("operator_reviewable"))).await.unwrap().0;
        let saved=app.store.get(&format!("session:{}",result["id"].as_str().unwrap())).await.unwrap().unwrap();
        assert_eq!(saved["conversation_privacy"],"operator_reviewable");
    }
}
