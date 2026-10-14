//! HTTP handlers for chat operations.
use super::*;
pub(crate) async fn workflow_policy_get(State(app): State<App>) -> Api {
    let policy = current_workflow_policy(&app).await?;
    Ok(Json(json!({"policy":policy,"revision":policy.revision()?})))
}
pub(crate) async fn workflow_policy_set(State(app): State<App>, Json(body): Json<Value>) -> Api {
    let _guard = app.gate.lock().await;
    let policy = workflow::Policy::resolve(Some(&body))
        .map_err(|_| bad("Invalid workflow policy, unsupported module, or ceiling exceeded"))?;
    let mut snapshot = app.runtime.write().await;
    let mut next = loop_runtime::Config::read(&app.runtime_path)?; next.apply_policy(&policy);
    next.persist(&app.runtime_path)?;
    *snapshot = next;
    Ok(Json(json!({"policy":policy,"revision":policy.revision()?})))
}
pub(crate) async fn current_workflow_policy(app: &App) -> Result<workflow::Policy> {
    loop_runtime::Config::read(&app.runtime_path)?.policy()
}

pub(crate) async fn send(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> std::result::Result<(StatusCode, Json<Value>), ApiError> {
    let text = body["text"].as_str().ok_or_else(||bad("text is required"))?;
    if text.len() > 65536 { return Err(bad("text has invalid length")); }
    let request_id = required(&body, "client_message_id", 128)?;
    let _guard = app.gate.lock().await;
    session_agent_id(&app,&id).await?;
    let receipt_key = format!("receipt:message:{id}:{}:{request_id}",crate::shared_chat::actor(&app));
    let legacy_receipt=if crate::shared_chat::actor(&app)==app.store.current_user_id(){app.store.get(&format!("receipt:message:{id}:{request_id}")).await?}else{None};
    if let Some(receipt) = app.store.get(&receipt_key).await?.or(legacy_receipt) {
        if receipt["digest"] != digest(&body) {
            return Err(conflict("Message identifier reused with different input"));
        }
        let mut result=receipt["result"].clone();
        if let Some(review_id)=result["review_id"].as_str() {
            if let Some(review)=app.store.get(&format!("manager_review:{review_id}")).await? {
                result["admission"]=crate::manager::public(&review);
                result["run_id"]=result["admission"]["run_id"].clone();
            }
        }
        return Ok((StatusCode::ACCEPTED, Json(result)));
    }
    let participation=app.store.participation_state(&id).await?;
    let legacy=participation["active"]==true && !crate::shared_chat::uses_shared_queue(&app,&id).await?;
    if legacy && crate::followup_queue::should_queue(&app,&id).await? {
        let result=crate::followup_queue::submit(&app,&id,&body).await?;
        return Ok((StatusCode::ACCEPTED,Json(result)));
    }
    let prepared=match prepare_message(&app,&id,&body,None).await {
        Ok(prepared)=>prepared,
        Err(error) if legacy && matches!(error.1.as_str(),"Resolve the existing run first"|"Resolve the pending message review first")=>{
            return Ok((StatusCode::ACCEPTED,Json(crate::followup_queue::submit(&app,&id,&body).await?)));
        },
        Err(error)=>return Err(error),
    };
    let mut docs=prepared.docs;
    docs.push((receipt_key,json!({"digest":digest(&body),"result":prepared.result})));
    let result=app.store.admit_shared_message(docs,&id,prepared.kind,&prepared.message).await.map_err(admission_error)?;
    crate::wake::notify(&app).await;
    Ok((StatusCode::ACCEPTED,Json(result)))
}
pub(crate) struct PreparedMessage {
    pub docs:Vec<(String,Value)>,
    pub kind: &'static str,
    pub message:Value,
    pub result:Value,
}
pub(super) fn admission_error(error:anyhow::Error)->ApiError {
    if error.to_string()=="Participation revision changed" {conflict("Participation revision changed; resend against the current agent state")}
    else if error.to_string()=="Account run limit reached" {conflict("Account run limit reached")}
    else if error.to_string()=="Session already has an active run" {conflict("Resolve the existing run first")}
    else {ApiError::from(error)}
}
fn message_run_id(app:&App,sid:&str,body:&Value)->String {
    let Some(request)=body["client_message_id"].as_str() else {return uuid()};
    use sha2::Digest;
    let hash=sha2::Sha256::digest(serde_json::to_vec(&json!({"actor":crate::shared_chat::actor(app),"owner":app.store.current_user_id(),"session":sid,"request":request,"body":digest(body)})).unwrap());
    let mut bytes=[0u8;16];bytes.copy_from_slice(&hash[..16]);
    uuid::Uuid::from_bytes(bytes).to_string()
}
pub(crate) async fn prepare_message(app:&App,id:&str,body:&Value,supplied_session:Option<&Value>)
    ->std::result::Result<PreparedMessage,ApiError> {
    prepare_message_inner(app,id,body,supplied_session,false).await
}
pub(crate) async fn prepare_followup(app:&App,id:&str,body:&Value) ->std::result::Result<PreparedMessage,ApiError> {
    prepare_message_inner(app,id,body,None,true).await
}
async fn prepare_message_inner(app:&App,id:&str,body:&Value,supplied_session:Option<&Value>,followup:bool)
    ->std::result::Result<PreparedMessage,ApiError> {
    let text=body["text"].as_str().ok_or_else(||bad("text is required"))?;
    if app.shutoff().await? {
        return Err(conflict("Global shutoff is active"));
    }
    let session = match supplied_session {
        Some(value)=>value.clone(),
        None=>app.store.get(&key("session",id)).await?
            .ok_or_else(||ApiError(StatusCode::NOT_FOUND,"Session not found".into()))?,
    };
    if supplied_session.is_none() && session["status"] != "active" { return Err(ApiError(StatusCode::NOT_FOUND,"Session not found".into())); }
    let agent_id=session["agent_id"].as_str().unwrap_or("shared_default").to_owned();
    let access=crate::shared_chat::live_access(&app.store,crate::shared_chat::actor(app),id,&agent_id).await
        .map_err(|_|ApiError(StatusCode::FORBIDDEN,"Agent unavailable".into()))?;
    if !access.admitted() { return Err(ApiError(StatusCode::FORBIDDEN,"Agent unavailable".into())); }
    let participation=app.store.participation_state(id).await.or_else(|error| if let Some(session)=supplied_session {Ok(session.get("participation").cloned().unwrap_or_else(||json!({"active":true,"revision":0,"generation":0})))} else {Err(error)})?;
    if participation["active"]!=true {
        let run_id=message_run_id(app,id,body);
        let mut prepared=attachments::prepare(body.get("attachments"),id,&run_id).map_err(|_|bad("Invalid attachments"))?;
        uploads::prepare_uploads(app,id,&run_id,body.get("attachment_ids"),&mut prepared).await?;
        if text.trim().is_empty() && prepared.metadata.is_empty(){return Err(bad("Message text or attachments required"));}
        let message=json!({"role":"user","content":text,"run_id":run_id,"author_id":crate::shared_chat::actor(app),"participation_generation":participation["generation"].as_u64().unwrap_or(0)});
        let mut docs=prepared.records;docs.push((format!("message_attachments:{run_id}"),json!(prepared.metadata)));
        return Ok(PreparedMessage{docs,kind:"human_message",message,result:json!({"run_id":null,"agent_active":false,"attachments":prepared.metadata})});
    }
    let active=app.store.active_runs().await?;
    let reviews=app.store.list("manager_pending:").await?;
    let session_busy=active.iter().any(|run|run["session_id"]==id && !terminal(status(run)))
        || app.store.get(&format!("manager_pending:{id}")).await?.is_some()
        || session["message_queue"].as_array().is_some_and(|queue|!queue.is_empty());
    // Follow-ups queue behind this conversation; independent admissions retain the existing ceilings.
    if !followup && !session_busy {
        if active.len()+reviews.len()>=app.runtime.read().await.scheduler.max_unresolved_runs {return Err(conflict("Run queue is full"));}
    }
    let manager=crate::manager::prepare(&app,&agent_id).await
        .map_err(|_|conflict("Incoming-message review requires valid criteria and an authorized System model"))?;
    let workflow_policy = current_workflow_policy(&app).await?;
    let input: Vec<Value> = if crate::shared_chat::actor(app)!=app.store.current_user_id() && supplied_session.is_none(){app.store.shared_model_context(id).await?}else{Vec::new()};
    let run_id = message_run_id(app,id,body);
    // Attachment text is saved with this message, but remains untrusted user data
    // when the loop later hydrates context for the model.
    let mut prepared = attachments::prepare(body.get("attachments"), &id, &run_id)
        .map_err(|_| bad("Invalid or unsupported attachments"))?;
    uploads::prepare_uploads(&app,&id,&run_id,body.get("attachment_ids"),&mut prepared).await?;
    if text.trim().is_empty() && prepared.metadata.is_empty() {
        return Err(bad("Message text or attachments required"));
    }
    let mut message = json!({"role":"user","content":text,"run_id":run_id,"author_id":crate::shared_chat::actor(app),"participation_generation":participation["generation"].as_u64().unwrap_or(0)});
    if !prepared.metadata.is_empty() {
        message["context_content"] = json!(prepared.context_content(text)?);
    }
    let agent_store=app.store.for_agent(&agent_id).await?;
    let catalog = app.store.model_catalog().await?;
    // The requested entry is a call-scoped override; it never appoints Standard.
    if body.get("model_role").is_some() && body["model_role"] != "standard" {
        return Err(bad("Chat supports only the Standard role"));
    }
    let saved_model = app.store.get(&format!("session_chat_controls:{id}")).await?;
    let selected = body.get("model_entry_id").or_else(|| saved_model.as_ref()
        .and_then(|saved| saved.get("entry_id")).filter(|entry| !entry.is_null()));
    let mut plan = Some(match selected {
        Some(Value::String(entry)) => catalog.resolve_custom(&agent_id,entry),
        Some(_) => return Err(bad("Invalid custom model entry")),
        None => catalog.resolve_role(&agent_id,model_selection::ModelRole::Standard),
    }.map_err(|_|conflict("Standard model required, or selected model is unavailable"))?);
    // Pin System separately now; compaction cannot borrow Standard or a later assignment.
    let mut system_plan = if catalog.agent_models.get(&agent_id)
        .is_some_and(|models|models.roles.contains_key(&model_selection::ModelRole::System)) {
        Some(catalog.resolve_role(&agent_id,model_selection::ModelRole::System)
            .map_err(|_|conflict("System model assignment is invalid or unauthorized"))?)
    } else {None};
    // Preserve the public forbidden response for live BYO entitlement denial.
    for candidate in plan.iter().chain(system_plan.iter()).flat_map(|p| &p.candidates) {
        if candidate.platform.is_none() && !access.model_allowed(&candidate.upstream_model) {
            return Err(ApiError(StatusCode::FORBIDDEN,"Model unavailable".into()));
        }
    }
    for plan in plan.iter_mut().chain(system_plan.iter_mut()) {
        app.pin_model_plan(plan).await.map_err(|_|conflict("Assigned model is no longer entitled or configured"))?;
    }
    let selection = match &plan {
        Some(plan) => provider::ProviderSelection { limits:plan.candidates[0].limits.clone(), provider: plan.candidates[0].transport.provider_id(),
            model: plan.candidates[0].upstream_model.clone() },
        None => {
            let mut agent_app=app.clone();
            agent_app.store=agent_store.clone();
            agent_app.selection().await?
        },
    };
    if plan.as_ref().is_none_or(|p|p.candidates[0].platform.is_none()) && !access.model_allowed(&selection.model) {
        return Err(ApiError(StatusCode::FORBIDDEN,"Model unavailable".into()));
    }
    let credential_ref = plan.as_ref().map(|p| p.candidates[0].credential_ref.as_str()).unwrap_or("provider_tokens");
    if plan.as_ref().is_none_or(|p|p.candidates[0].platform.is_none()) && app.store.get(credential_ref).await?.is_none() {
        return Err(conflict("Assigned provider connection is not authorized"));
    }
    let instructions = assembled_agent_instructions(&agent_store, &agent_id).await?;
    let mut run = json!({"agent_id":agent_id,"selection_plan":plan,"failover_state":model_selection::FailoverState::default(),"instructions":instructions,"provider_selection":selection.value(),"id":run_id,"session_id":id,"status":"queued","phase":"model","context_hydration_pending":true,"input":input,"calls":[],"call_index":0,"loaded_abilities":[],"ability_catalog_version":app.abilities.version(),
        "turns":0,"attempt":0,"pending":null,"generation":"","uncertain":false,"error":null,"created_at":now(),"updated_at":now()});
    run["requester_user_id"]=json!(crate::shared_chat::actor(app));
    run["participation_generation"]=participation["generation"].clone();
    // Both role cursors survive recovery without changing one another on failover.
    run["system_selection_plan"] = json!(system_plan);
    run["system_failover_state"] = json!(model_selection::FailoverState::default());
    run["attachments"] = json!(prepared.metadata.clone());
    run["chat_model_revision"] = json!(saved_model.as_ref().and_then(|v|v["revision"].as_u64()).unwrap_or(0));
    // Freeze memory at admission; later edits cannot rewrite queued context.
    run["memory_context"] = if crate::shared_chat::actor(app)==app.store.current_user_id(){json!(memory::snapshot(app.store.list(&format!("memory:{id}:")).await?)?)}else{Value::Null};
    workflow::record(&mut run, &workflow_policy,
        &workflow::Input::Admission { request_digest:digest(&body) }, workflow::Decision::Pass)?;
    workflow::record(&mut run, &workflow_policy,
        &workflow::Input::AttachmentPreparation { attachment_count:prepared.metadata.len() }, workflow::Decision::Pass)?;
    if let Some(manager)=manager {
        let record=json!({"id":run_id,"session_id":id,"agent_id":agent_id,
            "status":"pending_review","generation":"","deadline":0,"verdict":null,"error":null,
            "settings":manager["settings"],"settings_revision":manager["settings_revision"],"selection_plan":manager["selection_plan"],
            "failover_state":manager["failover_state"],"message":message,"run":run,
            "created_at":now(),"updated_at":now()});
        let result=json!({"run_id":null,"review_id":run_id,"admission":crate::manager::public(&record),"attachments":prepared.metadata});
        let mut docs=prepared.records;
        docs.push((format!("message_attachments:{run_id}"),json!(prepared.metadata)));
        docs.push((format!("manager_review:{run_id}"),record));
        docs.push((format!("manager_pending:{id}"),json!({"id":run_id})));
        return Ok(PreparedMessage {docs,kind:"manager_submission",message,result});
    }
    let result = json!({"run_id":run_id,"attachments":prepared.metadata});
    let mut docs = prepared.records;
    docs.push((format!("message_attachments:{run_id}"),json!(prepared.metadata)));
    docs.push((key("run", &run_id), run));
    Ok(PreparedMessage {docs,kind:"message",message,result})
}
// Only the owning session can read this encrypted attachment record. The
// model-facing copy was already saved with the message at admission.
pub(crate) async fn attachment_get(State(app): State<App>, Path((sid, aid)): Path<(String, String)>) -> Api {
    if Uuid::parse_str(&aid).is_err() {
        return Err(ApiError(StatusCode::NOT_FOUND, "Attachment not found".into()));
    }
    let record = app.store.get(&key("attachment", &aid)).await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Attachment not found".into()))?;
    if record["session_id"] != sid || !matches!(record["schema_version"].as_u64(),Some(1|2)) || record["run_id"].is_null() {
        return Err(ApiError(StatusCode::NOT_FOUND,"Attachment not found".into()));
    }
    Ok(Json(json!({"id":record["id"],"run_id":record["run_id"],"name":record["name"],
        "media_type":record["media_type"],"bytes":record["bytes"],"sha256":record["sha256"],"text":record["text"],"described_by":record["described_by"]})))
}
// Memory writes require a completed run in this same session. The API owns
// the decision; neither model text nor a Python tool can save memory directly.
pub(crate) async fn memory_list(State(app): State<App>, Path(sid): Path<String>) -> Api {
    let notes=app.store.list(&format!("memory:{sid}:")).await?;
    Ok(Json(json!({"memories":notes})))
}
pub(crate) async fn memory_save(State(app): State<App>, Path(sid): Path<String>,
    Json(body): Json<Value>) -> std::result::Result<(StatusCode, Json<Value>), ApiError> {
    let source=required(&body,"source_run_id",64)?;
    let id=required(&body,"client_memory_id",64)?;
    let text=required(&body,"text",1024)?;
    let _guard=app.gate.lock().await;
    let created_at=SystemTime::now().duration_since(UNIX_EPOCH)
        .map_err(|_|bad("System clock unavailable"))?.as_millis() as u64;
    let record=memory::record(&sid,source,id,text,created_at)
        .map_err(|_|bad("Invalid memory input"))?;
    let run=app.run(source).await
        .map_err(|_|ApiError(StatusCode::NOT_FOUND,"Source run not found".into()))?;
    if run["session_id"]!=sid {return Err(ApiError(StatusCode::NOT_FOUND,"Source run not found".into()));}
    if status(&run)!="completed" {return Err(conflict("Source run must be completed"));}
    let key=format!("memory:{sid}:{id}");
    if let Some(existing)=app.store.get(&key).await? {
        if existing["text"]!=text || existing["source_run_id"]!=source {
            return Err(conflict("Memory identifier reused with different input"));
        }
        return Ok((StatusCode::OK,Json(existing)));
    }
    if app.store.list(&format!("memory:{sid}:")).await?.len()>=memory::MAX_COUNT {
        return Err(conflict("Session memory limit reached"));
    }
    app.store.put(&key,&record).await?;
    Ok((StatusCode::CREATED,Json(record)))
}
pub(crate) async fn memory_delete(State(app): State<App>, Path((sid,id)): Path<(String,String)>) -> Api {
    if Uuid::parse_str(&id).is_err() {
        return Err(ApiError(StatusCode::NOT_FOUND,"Memory not found".into()));
    }
    let key=format!("memory:{sid}:{id}");
    let record=app.store.get(&key).await?
        .ok_or_else(||ApiError(StatusCode::NOT_FOUND,"Memory not found".into()))?;
    if record["session_id"]!=sid {return Err(ApiError(StatusCode::NOT_FOUND,"Memory not found".into()));}
    app.store.delete(&key).await?;
    Ok(Json(json!({"deleted":true})))
}
pub(crate) async fn events(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    axum::Extension(auth_doc): axum::Extension<Value>,
    Query(params): Query<HashMap<String, String>>,
) -> std::result::Result<
    Sse<impl futures_util::Stream<Item = std::result::Result<Event, Infallible>>>,
    ApiError,
> {
    let opening=app.store.get(&key("session", &id)).await?
        .filter(|s|s["status"]=="active")
        .ok_or_else(||ApiError(StatusCode::NOT_FOUND,"Session not found".into()))?;
    let execution_epoch=opening["execution_epoch"].as_u64().unwrap_or(0);
    let after = params
        .get("after")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0)
        .max(0)
        .max(
            headers
                .get("last-event-id")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0),
        );
    let auth_key = auth_doc["_auth_key"].as_str().unwrap_or("").to_owned();
    let expires = auth_doc["expires"].as_u64().unwrap_or(0);
    let user_id=auth_doc["_user_id"].as_str().unwrap_or("admin").to_owned();
    let generation=auth_doc["generation"].clone();
    let agent_id=session_agent_id(&app,&id).await?;
    let access_revision=crate::shared_chat::live_access(&app.store,&user_id,&id,&agent_id).await
        .map_err(|_|ApiError(StatusCode::FORBIDDEN,"Agent unavailable".into()))?.revision;
    let resolved=crate::shared_chat::resolve(&app,&id,&user_id).await.map_err(|_|ApiError(StatusCode::FORBIDDEN,"Session unavailable".into()))?;
    if !resolved.can_write(){app.store.shared_observe_audit(&id,&user_id).await?;}
    let stream = futures_util::stream::unfold(
        (
            app,
            id,
            after,
            Vec::<Value>::new(),
            now(),
            auth_key,
            expires,
            0u64,
            user_id, generation, agent_id, access_revision, None::<String>, 0u64,
        ),
        move |(app, id, mut cursor, mut queue, start, auth_key, expires, mut checked,
            user_id, generation, agent_id, access_revision, mut metadata_revision, mut metadata_checked)| async move {
            loop {
                if !app.store.get(&key("session",&id)).await.is_ok_and(|session|session.is_some_and(|s|
                    s["status"]=="active" && s["execution_epoch"].as_u64().unwrap_or(0)==execution_epoch)) { return None; }
                if now() - start > 3600 || app.stop.is_cancelled() || now() >= expires {
                    return None;
                }
                if crate::shared_chat::resolve(&app,&id,&user_id).await.is_err() {return None;}
                if now() > checked {
                    let doc=app.store.get(&auth_key).await.ok().flatten();
                    let account=app.store.account_by_id(&user_id).await.ok().flatten();
                    let access=crate::shared_chat::live_access(&app.store,&user_id,&id,&agent_id).await.ok();
                    let valid=doc.as_ref().is_some_and(|doc|doc["generation"]==generation
                            && doc["expires"].as_u64().unwrap_or(0)>now())
                        && (if let Some(doc)=doc.as_ref().filter(|d|d.get("agent_id").is_some()) {super::embed::credential_valid(&app,doc).await} else {true})
                        && account.as_ref().is_some_and(|account|
                            account["generation"]==generation
                            && account["is_approved"]==true
                            && access::Principal::from_account(account)
                                .is_ok_and(|principal|principal.active()))
                        && access.as_ref().is_some_and(|access|
                            (access.admitted() || app.chat_access.as_ref().is_some_and(|a|!a.can_write())) && access.revision==access_revision);
                    if !valid {
                        return None;
                    }
                    checked = now();
                }
                if !queue.is_empty() {
                    let item = queue.remove(0);
                    cursor = item["seq"].as_i64().unwrap();
                    let event = Event::default()
                        .id(cursor.to_string())
                        .data(item.to_string());
                    return Some((
                        Ok(event),
                        (app, id, cursor, queue, start, auth_key, expires, checked,
                            user_id, generation, agent_id, access_revision, metadata_revision, metadata_checked),
                    ));
                }
                queue = match app.store.chat_events(&id, cursor).await {
                    Ok(mut v) => {if uploads::message_files(&app,&mut v).await.is_err() || crate::manager::decorate(&app,&mut v).await.is_err() || crate::shared_projection::messages(&app,&mut v).await.is_err(){return None;} v},
                    Err(_) => return None,
                };
                if queue.is_empty() && now() > metadata_checked {
                    metadata_checked = now();
                    let summary = app.store.shared_read_summary(&id,&user_id).await.ok()?;
                    let participation=app.store.participation_state(&id).await.ok()?;
                    let queue_state=crate::followup_queue::public(&app.store.followup_queue(&id).await.ok()?);
                    let revision=digest(&json!([summary["metadata_revision"],participation["revision"],participation["generation"],queue_state["queue_state"]["revision"]]));
                    if metadata_revision.as_ref() != Some(&revision) {
                        metadata_revision = Some(revision);
                        // Metadata events deliberately carry no SSE ID: event-log
                        // sequence numbers are not public chat sequence numbers.
                        let event = Event::default().event("session_updated")
                            .data(json!({"session":summary,"participation":participation,"queue":queue_state["queue"],"queue_state":queue_state["queue_state"],"can_manage_queue":crate::shared_chat::actor(&app)==app.store.current_user_id()}).to_string());
                        return Some((Ok(event), (app, id, cursor, queue, start, auth_key,
                            expires, checked, user_id, generation, agent_id, access_revision,
                            metadata_revision, metadata_checked)));
                    }
                }
                if queue.is_empty() {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        },
    );
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}
pub(crate) async fn control(State(app): State<App>, Json(body): Json<Value>) -> Api {
    let req = required(&body, "client_request_id", 128)?;
    let shutoff = body["shutoff"]
        .as_bool()
        .ok_or_else(|| bad("shutoff must be boolean"))?;
    let _guard = app.gate.lock().await;
    let receipt_key = format!("receipt:control:{req}");
    if let Some(r) = app.store.get(&receipt_key).await? {
        if r["digest"] != digest(&body) {
            return Err(conflict("Identifier reused"));
        }
        return Ok(Json(r["result"].clone()));
    }
    // Fail closed: enabling marker precedes commit, disabling follows commit.
    if shutoff {
        marker(&app, true)?;
    }
    let result = json!({"shutoff":shutoff});
    app.store
        .atomic_docs_event(
            vec![
                ("control".into(), result.clone()),
                (receipt_key, json!({"digest":digest(&body),"result":result})),
            ],
            "",
            "control_changed",
            &result,
        )
        .await?;
    if !shutoff {
        marker(&app, false)?;
    } else {
        for token in app.active.lock().await.values() {
            token.cancel();
        }
        for user_app in user_apps(&app).await? {
            for view in user_app.store.active_runs().await? {
                if matches!(status(&view), "queued" | "running" | "waiting_external") {
                    let mut run = user_app.run(view["id"].as_str().unwrap()).await?;
                    run["status"] = json!("stopped");
                    run["error"] = json!("Stopped by global shutoff");
                    if (run["pending"]["state"] == "executing"
                        && !tools::is_read(run["pending"]["name"].as_str().unwrap_or(""))
                        && run["pending"]["external_job"]["kind"]!="v3_build")
                        || parallel_batch::executing_write(&run)
                    {
                        run["uncertain"] = json!(true);
                        parallel_batch::mark_uncertain_writes(&mut run);
                    }
                    user_app.save_override(&mut run).await?;
                }
            }
        }
    }
    Ok(Json(result))
}
pub(crate) async fn command(
    State(app): State<App>,
    Path((id, action)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Api {
    let req = required(&body, "client_request_id", 128)?;
    let _guard = app.gate.lock().await;
    let stored=app.run(&id).await.map_err(|_|ApiError(StatusCode::NOT_FOUND,"Run not found".into()))?;
    let requester=stored.pointer("/pending/authorization/owner_user_id").and_then(Value::as_str)
        .or_else(||stored["requester_user_id"].as_str()).unwrap_or(app.store.current_user_id());
    if matches!(action.as_str(),"resume"|"recovery") && requester!=crate::shared_chat::actor(&app) {
        return Err(ApiError(StatusCode::FORBIDDEN,"Only the requesting user can reconcile or resume their run".into()));
    }
    let receipt_key = format!("receipt:run:{id}:{action}:{}:{req}",crate::shared_chat::actor(&app));
    if let Some(r) = app.store.get(&receipt_key).await? {
        if r["digest"] != digest(&body) {
            return Err(conflict("Identifier reused"));
        }
        if matches!(action.as_str(),"cancel"|"pause") {
            if let Some(sid)=r["result"]["session_id"].as_str() {phase3_api::quiesce_session_resources(&app,sid).await?;}
        }
        let mut views=vec![r["result"].clone()];crate::shared_projection::runs(&app,&mut views).await?;
        return Ok(Json(views.remove(0)));
    }
    let mut run = app
        .run(&id)
        .await
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "Run not found".into()))?;
    if let Some(expected)=body.get("expected_run_revision") {
        if expected.as_u64().is_none() || expected!=&run["run_revision"] {return Err(conflict("Run changed; no control applied"));}
    }
    let generation_live=app.store.participation_allows_run(&run).await?;
    if !generation_live && (action=="resume" || (action=="recovery" && body["decision"]=="retry")) {
        return Err(conflict("Participation changed; send fresh instructions rather than restarting withdrawn work"));
    }
    match action.as_str() {
        "cancel" | "pause" => {
            run["component_pause"]=Value::Null;
            if let Some(token) = app.active.lock().await.get(&id) {
                token.cancel();
            }
            if !terminal(status(&run)) {
                if run["pending"]["external_job"]["kind"]=="v3_build" {
                    run["status"]=json!(if action=="pause" {"paused"} else {"cancelled"});
                } else if (run["pending"]["state"] == "executing"
                    && !tools::is_read(run["pending"]["name"].as_str().unwrap_or("")))
                    || parallel_batch::executing_write(&run)
                {
                    run["status"] = json!("recovery");
                    run["uncertain"] = json!(true);
                    parallel_batch::mark_uncertain_writes(&mut run);
                } else {
                    run["status"] = json!(if action=="pause"{"paused"}else{"cancelled"});
                }
                run["error"] = json!("Cancelled by user");
            }
        }
        "resume" => {
            if app.shutoff().await? {
                return Err(conflict("Clear global shutoff first"));
            }
            if run["uncertain"] == true {
                return Err(conflict("Review the uncertain tool outcome first"));
            }
            if !matches!(status(&run), "stopped" | "paused" | "failed") {
                return Err(conflict("Run cannot be resumed"));
            }
            if app.store.active_runs().await?.iter().any(|r| {
                r["id"] != id && r["session_id"] == run["session_id"] && !terminal(status(r))
            }) {
                return Err(conflict("Another run owns this conversation"));
            }
            if run["provider_recovery"]["version"] == 1
                && run["provider_recovery"]["safe_to_resume"] == true {
                let field = if run["provider_recovery"]["system"] == true {
                    "system_failover_state"
                } else { "failover_state" };
                let mut state: model_selection::FailoverState = serde_json::from_value(run[field].clone())
                    .map_err(|_| conflict("Provider recovery state is invalid"))?;
                if state.response_started || state.uncertain_tool_effect {
                    return Err(conflict("Provider response cannot be safely replayed"));
                }
                // Keep the pinned candidate and failover history. Only a human
                // resume grants another bounded attempt on this connection.
                state.retries = 0;
                run[field] = serde_json::to_value(state).map_err(anyhow::Error::from)?;
                run["provider_recovery"] = Value::Null;
            }
            run["provider_retry_at"] = Value::Null;
            run["component_pause"]=Value::Null;
            let external=run["pending"]["external_job"]["kind"]=="v3_build";
            run["status"] = json!(if external {"waiting_external"}else{"queued"});
            run["error"] = Value::Null;
            run["attempt"] = json!(0);
            run["turns"] = json!(0);
            if run["pending"]["state"] == "executing" && !external {
                run["pending"]["state"] = json!("requested");
            }
            if let Some(items)=run["batch"].as_array_mut() {
                for item in items { if item["state"]=="executing" && tools::is_read(item["pending"]["name"].as_str().unwrap_or("")) {item["state"]=json!("requested");} }
            }
        }
        "recovery" => {
            if run["uncertain"] != true {
                return Err(conflict("Run does not require recovery review"));
            }
            if run["batch"].is_array() {
                // RECOVERY-PER-CALL: the reviewer names the uncertain call when
                // more than one write was in flight. No other call is retried.
                let uncertain=run["batch"].as_array().unwrap().iter().enumerate()
                    .filter(|(_,item)|item["state"]=="uncertain").map(|(idx,_)|idx)
                    .collect::<Vec<_>>();
                let idx=if let Some(call_id)=body["call_id"].as_str() {
                    uncertain.iter().copied().find(|idx|run["batch"][*idx]["pending"]["call_id"]==call_id)
                        .ok_or_else(||bad("Unknown uncertain call ID"))?
                } else if uncertain.len()==1 {uncertain[0]}
                    else {return Err(bad("Specify call_id for uncertain parallel writes"));};
                match body["decision"].as_str() {
                    Some("abandon")=>{
                        run["status"]=json!("cancelled");
                        run["uncertain"]=json!(false);
                        run["recovery_abandoned"]=json!(true);
                    },
                    Some("retry")=>{
                        if app.shutoff().await? {return Err(conflict("Clear global shutoff first"));}
                        // A retry is a new authorization attempt with a new ID.
                        run["batch"][idx]["state"]=json!("new");
                        run["batch"][idx]["pending"]=Value::Null;
                        run["batch"][idx]["result"]=Value::Null;
                    },
                    Some("accept")=>{
                        let output=required(&body,"output",1048576)?;
                        run["batch"][idx]["result"]=json!({"status":"reconciled","output":output});
                        run["batch"][idx]["state"]=json!("done");
                    },
                    _=>return Err(bad("Choose retry, accept, or abandon")),
                }
                if body["decision"]!="abandon" {
                    run["uncertain"]=json!(parallel_batch::uncertain_count(&run)>0);
                    run["status"]=json!(if run["uncertain"]==true {"recovery"}
                        else if app.shutoff().await? {"stopped"} else {"queued"});
                }
                run["pending"]=Value::Null;
            } else {
            match body["decision"].as_str() {
                Some("abandon") => {
                    run["status"] = json!("cancelled");
                    run["uncertain"] = json!(false);
                    run["recovery_abandoned"] = json!(true);
                }
                Some("retry") => {
                    if app.shutoff().await? {
                        return Err(conflict("Clear global shutoff first"));
                    }
                    run["pending"]["state"] = json!("requested");
                    run["pending"]["decision"] = Value::Null;
                    run["status"] = json!("queued");
                    run["uncertain"] = json!(false);
                }
                Some("accept") => {
                    let output = required(&body, "output", 1048576)?;
                    append_tool(&mut run, &json!({"status":"reconciled","output":output}))?;
                    run["status"] = json!(if app.shutoff().await? {
                        "stopped"
                    } else {
                        "queued"
                    });
                    run["uncertain"] = json!(false);
                }
                _ => return Err(bad("Choose retry, accept, or abandon")),

            }}
        }
        _ => return Err(ApiError(StatusCode::NOT_FOUND, "Unknown run action".into())),
    }
    if action=="recovery" && !generation_live && !terminal(status(&run)) {
        // Reconciliation records known outcomes but never revives withdrawn work.
        run["status"]=json!(if run["uncertain"]==true{"recovery"}else{"stopped"});
    }
    let result = public_run(&run);
    app.store
        .atomic_docs_event(
            vec![
                (key("run", &id), run),
                (receipt_key, json!({"digest":digest(&body),"result":result})),
            ],
            result["session_id"].as_str().unwrap(),
            "run_status",
            &result,
        )
        .await?;
    if matches!(action.as_str(),"cancel"|"pause") {phase3_api::quiesce_session_resources(&app,result["session_id"].as_str().unwrap()).await?;}
    let mut views=vec![result];crate::shared_projection::runs(&app,&mut views).await?;
    Ok(Json(views.remove(0)))
}
pub(crate) async fn approval(State(app): State<App>, Path(id): Path<String>, Json(body): Json<Value>) -> Api {
    let req = required(&body, "client_request_id", 128)?;
    let decision = required(&body, "decision", 20)?;
    if !matches!(decision, "approve" | "deny") {
        return Err(bad("Invalid approval decision"));
    }
    let _guard = app.gate.lock().await;
    let receipt_key = format!("receipt:approval:{id}:{}:{req}",crate::shared_chat::actor(&app));
    if let Some(r) = app.store.get(&receipt_key).await? {
        if r["digest"] != digest(&body) {
            return Err(conflict("Identifier reused"));
        }
        return Ok(Json(r["result"].clone()));
    }
    let view = app
        .store
        .active_runs()
        .await?
        .into_iter()
        .find(|r| r["pending"]["id"] == id)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Approval not found".into()))?;
    let mut run = app.run(view["id"].as_str().unwrap()).await?;
    if status(&run) != "waiting_approval" {
        return Err(conflict("Approval is no longer pending"));
    }
    let sid = run["session_id"].as_str().unwrap();
    if run["pending"]["new_intent_confirmation"]==true && run["requester_user_id"].as_str().unwrap_or(app.store.current_user_id())!=crate::shared_chat::actor(&app) {
        return Err(ApiError(StatusCode::FORBIDDEN,"Only the requesting user may confirm an additional matching action".into()));
    }
    if let Some(owner)=run["pending"]["authorization"]["owner_user_id"].as_str() {
        if owner!=crate::shared_chat::actor(&app) {return Err(ApiError(StatusCode::FORBIDDEN,"Only the credential owner may authorize this tool".into()));}
    }
    if !crate::credential_authorization::valid(&app.store,&run,&run["pending"]).await? {
        run["pending"]["state"]=json!("cancelled");run["pending"]["decision"]=Value::Null;
        run["pending"]["human_approved"]=json!(false);run["status"]=json!("stopped");
        app.save(&mut run).await?;
        return Err(conflict("Credential authorization changed"));
    }
    if !app.store.participation_allows_run(&run).await? {return Err(conflict("Agent is inactive or participation changed"));}
    let agent_id=run["agent_id"].as_str().unwrap_or("shared_default");
    let access=crate::shared_chat::live_access(&app.store,run["requester_user_id"].as_str().unwrap_or(app.store.current_user_id()),sid,agent_id).await
        .map_err(|_|conflict("Access changed; approval is invalid"))?;
    if !access.admitted() || !app.run_model_authorized(&run).await {
        run["status"]=json!("stopped");
        run["pending"]["decision"]=Value::Null;
        app.save(&mut run).await?;
        return Err(conflict("Access changed; approval is invalid"));
    }
    let (policy, mode_revision) = effective_policy(&app, sid).await?;
    let visibility = session_agent_abilities(&app, sid).await?;
    let ability_state = session_ability_state(&app, sid).await?;
    if run["pending"]["policy_version"] != policy["version"]
        || run["pending"]["access_revision"] != json!(access.revision)
        || run["pending"]["mode_revision"].as_u64().unwrap_or(0) != mode_revision
        || run["pending"]["visibility_revision"].as_u64().unwrap_or(0)
            != visibility["revision"].as_u64().unwrap()
        || run["pending"]["ability_state_revision"]
            .as_u64()
            .unwrap_or(0)
            != ability_state["revision"].as_u64().unwrap()
    {
        if !run["pending"]["authorization"].is_null() {
            run["pending"]["state"]=json!("cancelled");run["pending"]["decision"]=Value::Null;
            run["pending"]["human_approved"]=json!(false);run["status"]=json!("stopped");
            app.save(&mut run).await?;
            return Err(conflict("Policy or session mode changed; owner authorization cancelled"));
        }
        // BATCH-APPROVAL-REVOCATION: a changed policy invalidates only this
        // call. Its next pass gets a new pending ID and fresh revisions.
        let stale_id=run["pending"]["id"].clone();
        if let Some(items)=run["batch"].as_array_mut() {
            for item in items { if item["pending"]["id"]==stale_id {
                item["state"]=json!("new"); item["pending"]=Value::Null;
                item["result"]=Value::Null; break;
            } }
            run["pending"]=Value::Null;
        } else {
            run["pending"]["state"] = json!("requested");
            run["pending"]["decision"] = Value::Null;
        }
        run["status"] = json!("queued");
        app.save(&mut run).await?;
        return Err(conflict(
            "Policy or session mode changed; a new gate decision is required",
        ));
    }
    run["pending"]["decision"] = json!(decision);
    run["status"] = json!(if app.shutoff().await? {
        "stopped"
    } else {
        "queued"
    });
    let result = public_run(&run);
    app.store
        .atomic_docs_event(
            vec![
                (key("run", run["id"].as_str().unwrap()), run.clone()),
                (receipt_key, json!({"digest":digest(&body),"result":result})),
            ],
            run["session_id"].as_str().unwrap(),
            "run_status",
            &result,
        )
        .await?;
    Ok(Json(result))
}

#[cfg(test)]
mod shared_command_projection_tests {
 use super::*;
 #[tokio::test]
 async fn another_participant_can_stop_but_cannot_resume_or_read_owner_approval() {
  let (_root,app)=crate::runtime_tests::fixture().await;
  let sid=crate::runtime_tests::conversation(&app).await;
  let rid=crate::runtime_tests::queued(&app,&sid).await;
  let mut run=app.run(&rid).await.unwrap();
  run["requester_user_id"]=json!("alice");run["status"]=json!("paused");
  run["pending"]=json!({"id":"private-panel","state":"waiting","arguments":{"private":"owner-only"},"authorization":{"owner_user_id":"alice"}});
  app.store.put(&key("run",&rid),&run).await.unwrap();
  let mut bob=app.clone();bob.chat_actor=Some("bob".into());
  let error=command(State(bob.clone()),Path((rid.clone(),"resume".into())),Json(json!({"client_request_id":"resume"}))).await.unwrap_err();
  assert_eq!(error.0,StatusCode::FORBIDDEN);
  for _ in 0..2 {
   let Json(result)=command(State(bob.clone()),Path((rid.clone(),"cancel".into())),Json(json!({"client_request_id":"stop"}))).await.unwrap();
   assert_eq!(result["can_decide"],false);
   assert!(result["pending"].is_null());
   assert!(!result.to_string().contains("owner-only"));
  }
 }
}
