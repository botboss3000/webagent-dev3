//! Message admission: the chat send endpoint and the shared preparation that turns a
//! request into a durable user message and its run. Panel answers, follow-ups, direct
//! messages and extension sends all admit through `prepare_message`/`send_message`.
use super::chat::{command, current_workflow_policy};
use super::{
    assembled_agent_instructions, attachments, bad, conflict, digest, json, key, memory,
    model_selection, now, provider, public_run, required, session_agent_id, status, terminal,
    uploads, uuid, workflow, ApiError, App, Json, Path, State, StatusCode, Value,
};

/// Fields only the server sets: a panel answer (`/components/{cid}/answer`) and a
/// compaction-only run (`/compact` on an idle chat).
const SERVER_FIELDS: [&str; 4] = [
    "component_id",
    "component_answer",
    "component_revision",
    "compaction_only",
];

pub(crate) async fn send(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> std::result::Result<(StatusCode, Json<Value>), ApiError> {
    if SERVER_FIELDS.iter().any(|field| body.get(field).is_some()) {
        return Err(bad("Answer panels through their answer endpoint"));
    }
    send_message(app, id, body).await
}

/// Admit one user message: queued behind live work, or as a new run. `body` may carry the
/// server-set fields of `SERVER_FIELDS`.
pub(crate) async fn send_message(
    app: App,
    id: String,
    body: Value,
) -> std::result::Result<(StatusCode, Json<Value>), ApiError> {
    let text = body["text"]
        .as_str()
        .ok_or_else(|| bad("text is required"))?;
    if text.len() > 65536 {
        return Err(bad("text has invalid length"));
    }
    let request_id = required(&body, "client_message_id", 128)?;
    let generated = SERVER_FIELDS.iter().any(|field| body.get(field).is_some());
    if !generated {
        if let Some(stopped) = typed_stop(&app, &id, &body, text, request_id).await? {
            return Ok((StatusCode::ACCEPTED, Json(stopped)));
        }
    }
    let _guard = app.gate.lock().await;
    let agent_id = session_agent_id(&app, &id).await?;
    let receipt_key = format!(
        "receipt:message:{id}:{}:{request_id}",
        crate::shared_chat::actor(&app)
    );
    let legacy_receipt = if crate::shared_chat::actor(&app) == app.store.current_user_id() {
        app.store
            .get(&format!("receipt:message:{id}:{request_id}"))
            .await?
    } else {
        None
    };
    if app.store.storage_full().await? {
        return Err(ApiError(
            StatusCode::INSUFFICIENT_STORAGE,
            crate::api::storage_cleanup::STORAGE_FULL.into(),
        ));
    }
    if let Some(receipt) = app.store.get(&receipt_key).await?.or(legacy_receipt) {
        if receipt["digest"] != digest(&body) {
            return Err(conflict("Message identifier reused with different input"));
        }
        let mut result = receipt["result"].clone();
        if let Some(review_id) = result["review_id"].as_str() {
            if let Some(review) = app
                .store
                .get(&format!("manager_review:{review_id}"))
                .await?
            {
                result["admission"] = crate::manager::public(&review);
                result["run_id"] = result["admission"]["run_id"].clone();
            }
        }
        return Ok((StatusCode::ACCEPTED, Json(result)));
    }
    let participation = app.store.participation_state(&id).await?;
    let legacy = participation["active"] == true
        && !crate::shared_chat::uses_shared_queue(&app, &id).await?;
    if legacy {
        // Conditions that block every run refuse the message now rather than queue it
        // silently behind a hold the user cannot see.
        if app.shutoff().await? {
            return Err(conflict("Global shutoff is active"));
        }
        if !app
            .store
            .live_access(app.store.current_user_id(), &agent_id)
            .await?
            .admitted()
        {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                "Agent access unavailable".into(),
            ));
        }
        supersede_stopped_runs(&app, &id, request_id).await?;
    }
    if legacy && crate::followup_queue::should_queue(&app, &id).await? {
        let result = crate::followup_queue::submit(&app, &id, &body).await?;
        return Ok((StatusCode::ACCEPTED, Json(result)));
    }
    let prepared = match prepare_message(&app, &id, &body, None).await {
        Ok(prepared) => prepared,
        Err(error)
            if legacy
                && matches!(
                    error.1.as_str(),
                    "Resolve the existing run first" | "Resolve the pending message review first"
                ) =>
        {
            return Ok((
                StatusCode::ACCEPTED,
                Json(crate::followup_queue::submit(&app, &id, &body).await?),
            ));
        }
        Err(error) => return Err(error),
    };
    let mut docs = prepared.docs;
    docs.push((
        receipt_key,
        json!({"digest":digest(&body),"result":prepared.result}),
    ));
    let result = app
        .store
        .admit_shared_message(docs, &id, prepared.kind, &prepared.message)
        .await
        .map_err(admission_error)?;
    record_admitted_turn(&app, &agent_id, &prepared.message["run_id"]).await;
    crate::wake::notify(&app).await;
    Ok((StatusCode::ACCEPTED, Json(result)))
}
/// True when a typed message is only a stop phrase such as "stop", "pause" or "hold on".
fn stop_phrase(text: &str) -> bool {
    let text = text
        .trim()
        .trim_end_matches(['.', '!'])
        .to_ascii_lowercase();
    let text = text.strip_prefix("please ").unwrap_or(&text);
    matches!(
        text,
        "stop"
            | "stop it"
            | "stop that"
            | "stop now"
            | "stop please"
            | "pause"
            | "hold on"
            | "wait"
            | "wait a moment"
            | "wait a second"
            | "cancel"
    )
}
/// A bare stop phrase typed while work is in flight stops that run without recording
/// a message or calling a model. Its message receipt makes retries return the same stop.
async fn typed_stop(
    app: &App,
    sid: &str,
    body: &Value,
    text: &str,
    request_id: &str,
) -> std::result::Result<Option<Value>, ApiError> {
    let attached = body["attachment_ids"]
        .as_array()
        .is_some_and(|ids| !ids.is_empty());
    let receipt_key = format!(
        "receipt:message:{sid}:{}:{request_id}",
        crate::shared_chat::actor(app)
    );
    if attached || !stop_phrase(text) || app.store.get(&receipt_key).await?.is_some() {
        return Ok(None);
    }
    let running = app.store.active_runs().await?.into_iter().find(|run| {
        run["session_id"] == sid
            && matches!(
                status(run),
                "queued" | "running" | "waiting_approval" | "waiting_gate" | "waiting_external"
            )
    });
    let Some(run_id) = running.as_ref().and_then(|run| run["id"].as_str()) else {
        return Ok(None);
    };
    let Json(run) = command(
        State(app.clone()),
        Path((run_id.to_owned(), "cancel".to_owned())),
        Json(json!({"client_request_id":format!("typed-stop-{request_id}")})),
    )
    .await?;
    let result = json!({"run_id":null,"stopped_run":run});
    app.store
        .put(
            &receipt_key,
            &json!({"digest":digest(body),"result":result}),
        )
        .await?;
    Ok(Some(result))
}
/// A new message replaces paused or stopped work in its conversation, and is the user's
/// resume for a held queue once no work remains, so the message (and the queue behind
/// it) is delivered instead of waiting. Uncertain work still needs explicit recovery.
async fn supersede_stopped_runs(
    app: &App,
    sid: &str,
    request_id: &str,
) -> std::result::Result<(), ApiError> {
    for view in app.store.active_runs().await? {
        if view["session_id"] != sid
            || !matches!(status(&view), "paused" | "stopped")
            || view["uncertain"] == true
        {
            continue;
        }
        let Some(run_id) = view["id"].as_str() else {
            continue;
        };
        let mut run = app.run(run_id).await?;
        if !matches!(status(&run), "paused" | "stopped") || run["uncertain"] == true {
            continue;
        }
        run["status"] = json!("cancelled");
        run["component_pause"] = Value::Null;
        run["superseded_by_message"] = json!(true);
        let result = public_run(&run);
        app.store
            .atomic_docs_event(vec![(key("run", run_id), run)], sid, "run_status", &result)
            .await?;
    }
    let queue = app.store.followup_queue(sid).await?;
    let busy = app
        .store
        .active_runs()
        .await?
        .iter()
        .any(|r| r["session_id"] == sid);
    if queue["held"] == true && !busy {
        let revision = queue["revision"].as_u64().unwrap_or(0);
        let request = format!("supersede:{request_id}");
        let body = json!({"client_request_id":request,"expected_revision":revision});
        app.store
            .followup_choice(sid, None, revision, &request, &body)
            .await
            .map_err(|error| conflict(&error.to_string()))?;
    }
    Ok(())
}
pub(crate) struct PreparedMessage {
    pub docs: Vec<(String, Value)>,
    pub kind: &'static str,
    pub message: Value,
    pub result: Value,
}
pub(super) fn admission_error(error: anyhow::Error) -> ApiError {
    if error.to_string() == "Participation revision changed" {
        conflict("Participation revision changed; resend against the current agent state")
    } else if error.to_string() == "Account run limit reached" {
        conflict("Account run limit reached")
    } else if error.to_string() == "Session already has an active run" {
        conflict("Resolve the existing run first")
    } else {
        ApiError::from(error)
    }
}
fn message_run_id(app: &App, sid: &str, body: &Value) -> String {
    let Some(request) = body["client_message_id"].as_str() else {
        return uuid();
    };
    use sha2::Digest;
    let hash=sha2::Sha256::digest(serde_json::to_vec(&json!({"actor":crate::shared_chat::actor(app),"owner":app.store.current_user_id(),"session":sid,"request":request,"body":digest(body)})).expect("JSON values always serialize"));
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    uuid::Uuid::from_bytes(bytes).to_string()
}
pub(crate) async fn prepare_message(
    app: &App,
    id: &str,
    body: &Value,
    supplied_session: Option<&Value>,
) -> std::result::Result<PreparedMessage, ApiError> {
    prepare_message_inner(app, id, body, supplied_session, false).await
}
pub(crate) async fn prepare_followup(
    app: &App,
    id: &str,
    body: &Value,
) -> std::result::Result<PreparedMessage, ApiError> {
    prepare_message_inner(app, id, body, None, true).await
}
async fn prepare_message_inner(
    app: &App,
    id: &str,
    body: &Value,
    supplied_session: Option<&Value>,
    followup: bool,
) -> std::result::Result<PreparedMessage, ApiError> {
    let text = body["text"]
        .as_str()
        .ok_or_else(|| bad("text is required"))?;
    if app.shutoff().await? {
        return Err(conflict("Global shutoff is active"));
    }
    let session = match supplied_session {
        Some(value) => value.clone(),
        None => app
            .store
            .get(&key("session", id))
            .await?
            .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Session not found".into()))?,
    };
    if supplied_session.is_none() && session["status"] != "active" {
        return Err(ApiError(StatusCode::NOT_FOUND, "Session not found".into()));
    }
    let agent_id = session["agent_id"]
        .as_str()
        .unwrap_or("shared_default")
        .to_owned();
    let access =
        crate::shared_chat::live_access(&app.store, crate::shared_chat::actor(app), id, &agent_id)
            .await
            .map_err(|_| ApiError(StatusCode::FORBIDDEN, "Agent unavailable".into()))?;
    if !access.admitted() {
        return Err(ApiError(StatusCode::FORBIDDEN, "Agent unavailable".into()));
    }
    crate::api::agent_members::enforce_turn_limits(
        app,
        crate::shared_chat::actor(app),
        &agent_id,
        &access.capability_level,
    )
    .await?;
    let participation = app.store.participation_state(id).await.or_else(|error| {
        if let Some(session) = supplied_session {
            Ok(session
                .get("participation")
                .cloned()
                .unwrap_or_else(|| json!({"active":true,"revision":0,"generation":0})))
        } else {
            Err(error)
        }
    })?;
    if participation["active"] != true {
        let run_id = message_run_id(app, id, body);
        let mut prepared = attachments::prepare(body.get("attachments"), id, &run_id)
            .map_err(|_| bad("Invalid attachments"))?;
        uploads::prepare_uploads(
            app,
            id,
            &agent_id,
            &run_id,
            body.get("attachment_ids"),
            &mut prepared,
        )
        .await?;
        if text.trim().is_empty() && prepared.metadata.is_empty() {
            return Err(bad("Message text or attachments required"));
        }
        let message = json!({"role":"user","content":text,"run_id":run_id,"author_id":crate::shared_chat::actor(app),"participation_generation":participation["generation"].as_u64().unwrap_or(0)});
        let mut docs = prepared.records;
        docs.push((
            format!("message_attachments:{run_id}"),
            json!(prepared.metadata),
        ));
        return Ok(PreparedMessage {
            docs,
            kind: "human_message",
            message,
            result: json!({"run_id":null,"agent_active":false,"attachments":prepared.metadata}),
        });
    }
    let active = app.store.active_runs().await?;
    let reviews = app.store.list("manager_pending:").await?;
    let session_busy = active
        .iter()
        .any(|run| run["session_id"] == id && !terminal(status(run)))
        || app
            .store
            .get(&format!("manager_pending:{id}"))
            .await?
            .is_some()
        || session["message_queue"]
            .as_array()
            .is_some_and(|queue| !queue.is_empty());
    // Follow-ups queue behind this conversation; independent admissions retain the existing ceilings.
    if !followup && !session_busy {
        if active.len() + reviews.len() >= app.runtime.read().await.scheduler.max_unresolved_runs {
            return Err(conflict("Run queue is full"));
        }
    }
    // Review screens visitors; the agent's owner and members skip it unless it also
    // reviews staff.
    let staff = app
        .store
        .live_access(crate::shared_chat::actor(app), &agent_id)
        .await
        .is_ok_and(|access| {
            matches!(
                access.relationship,
                crate::access::Relationship::Owner | crate::access::Relationship::Member
            )
        });
    let staff_exempt = staff
        && crate::manager::config::incoming(&app.store, &agent_id)
            .await
            .is_ok_and(|settings| !settings.review_staff);
    // A compaction-only run carries no request for the agent, so review has nothing to judge.
    let manager = if staff_exempt || body["compaction_only"] == true {
        None
    } else {
        crate::manager::prepare(&app, &agent_id)
            .await
            .map_err(|_| {
                conflict(
                    "Incoming-message review requires valid criteria and an authorized System model",
                )
            })?
    };
    let workflow_policy = current_workflow_policy(&app).await?;
    let input: Vec<Value> = if crate::shared_chat::actor(app) != app.store.current_user_id()
        && supplied_session.is_none()
    {
        app.store.shared_model_context(id).await?
    } else {
        Vec::new()
    };
    let run_id = message_run_id(app, id, body);
    // Attachment text is saved with this message, but remains untrusted user data
    // when the loop later hydrates context for the model.
    let mut prepared = attachments::prepare(body.get("attachments"), &id, &run_id)
        .map_err(|_| bad("Invalid or unsupported attachments"))?;
    uploads::prepare_uploads(
        &app,
        &id,
        &agent_id,
        &run_id,
        body.get("attachment_ids"),
        &mut prepared,
    )
    .await?;
    if text.trim().is_empty() && prepared.metadata.is_empty() {
        return Err(bad("Message text or attachments required"));
    }
    let mut message = json!({"role":"user","content":text,"run_id":run_id,"author_id":crate::shared_chat::actor(app),"participation_generation":participation["generation"].as_u64().unwrap_or(0)});
    if !prepared.metadata.is_empty() {
        message["context_content"] = json!(prepared.context_content(text)?);
    }
    // PANEL ANSWER: the message and the answered panel commit together, so a panel
    // has at most one answer and the reply names the call that drew it.
    let mut panel_doc = None;
    if let Some(panel) = body.get("component_id").and_then(Value::as_str) {
        let revision = body["component_revision"]
            .as_u64()
            .ok_or_else(|| bad("component_revision required"))?;
        let (key, document) = app
            .store
            .base_component_answered(id, panel, revision, &body["component_answer"], &run_id)
            .await
            .map_err(|error| conflict(&error.to_string()))?;
        let origin = document["origin"].clone();
        let call = origin["call_id"].as_str().unwrap_or("unknown");
        let content = message["context_content"]
            .as_str()
            .unwrap_or(text)
            .to_owned();
        message["context_content"] = json!(format!(
            "{content}\n\n[Answer to chat panel {panel}, shown by tool call {call}]"
        ));
        message["component_id"] = json!(panel);
        message["component_origin"] = origin;
        panel_doc = Some((key, document));
    }
    if body["compaction_only"] == true {
        let content = message["context_content"]
            .as_str()
            .unwrap_or(text)
            .to_owned();
        message["context_content"] = json!(format!(
            "{content}\n\n[The user compacted earlier context. No reply is needed.]"
        ));
    }
    // Panel outcomes that sent no message (a dismissal, Keep) reach the agent here.
    let notes_key = format!("panel_notes:{id}");
    let panel_notes = app
        .store
        .get(&notes_key)
        .await?
        .and_then(|record| record["notes"].as_array().cloned())
        .filter(|notes| !notes.is_empty());
    if let Some(notes) = &panel_notes {
        let content = message["context_content"]
            .as_str()
            .unwrap_or(text)
            .to_owned();
        let notes: Vec<&str> = notes.iter().filter_map(Value::as_str).collect();
        message["context_content"] = json!(format!("{content}\n\n{}", notes.join("\n")));
    }
    // A total stop leaves one note so the agent confirms before restarting stopped work.
    let stop_key = format!("session_stopped:{id}");
    let stop_record = app
        .store
        .get(&stop_key)
        .await?
        .filter(|r| r["delivered"] != true);
    if let Some(record) = &stop_record {
        let content = message["context_content"]
            .as_str()
            .unwrap_or(text)
            .to_owned();
        let note = super::session_stop::stopped_note(record);
        message["context_content"] = json!(format!("{content}\n\n{note}"));
    }
    let stop_delivery = stop_record.map(|mut record| {
        record["delivered"] = json!(true);
        (stop_key, record)
    });
    let agent_store = app.store.for_agent(&agent_id).await?;
    let catalog = app.store.model_catalog().await?;
    // The requested entry is a call-scoped override; it never appoints Standard.
    if body.get("model_role").is_some() && body["model_role"] != "standard" {
        return Err(bad("Chat supports only the Standard role"));
    }
    let saved_model = app
        .store
        .get(&format!("session_chat_controls:{id}"))
        .await?;
    let selected = body.get("model_entry_id").or_else(|| {
        saved_model
            .as_ref()
            .and_then(|saved| saved.get("entry_id"))
            .filter(|entry| !entry.is_null())
    });
    let mut plan = Some(
        match selected {
            Some(Value::String(entry)) => catalog.resolve_custom(&agent_id, entry),
            Some(_) => return Err(bad("Invalid custom model entry")),
            None => catalog.resolve_role(&agent_id, model_selection::ModelRole::Standard),
        }
        .map_err(|_| conflict("Standard model required, or selected model is unavailable"))?,
    );
    // Pin System separately now; compaction cannot borrow Standard or a later assignment.
    let mut system_plan = if catalog.agent_models.get(&agent_id).is_some_and(|models| {
        models
            .roles
            .contains_key(&model_selection::ModelRole::System)
    }) {
        Some(
            catalog
                .resolve_role(&agent_id, model_selection::ModelRole::System)
                .map_err(|_| conflict("System model assignment is invalid or unauthorized"))?,
        )
    } else {
        None
    };
    // Preserve the public forbidden response for live BYO entitlement denial.
    for candidate in plan
        .iter()
        .chain(system_plan.iter())
        .flat_map(|p| &p.candidates)
    {
        if candidate.platform.is_none() && !access.model_allowed(&candidate.upstream_model) {
            return Err(ApiError(StatusCode::FORBIDDEN, "Model unavailable".into()));
        }
    }
    for plan in plan.iter_mut().chain(system_plan.iter_mut()) {
        app.pin_model_plan(plan)
            .await
            .map_err(|_| conflict("Assigned model is no longer entitled or configured"))?;
    }
    let selection = match &plan {
        Some(plan) => provider::ProviderSelection {
            provider: plan.candidates[0].transport.provider_id(),
            model: plan.candidates[0].upstream_model.clone(),
        },
        None => {
            let mut agent_app = app.clone();
            agent_app.store = agent_store.clone();
            agent_app.selection().await?
        }
    };
    if plan
        .as_ref()
        .is_none_or(|p| p.candidates[0].platform.is_none())
        && !access.model_allowed(&selection.model)
    {
        return Err(ApiError(StatusCode::FORBIDDEN, "Model unavailable".into()));
    }
    let credential_ref = plan
        .as_ref()
        .map(|p| p.candidates[0].credential_ref.as_str())
        .unwrap_or("provider_tokens");
    if plan
        .as_ref()
        .is_none_or(|p| p.candidates[0].platform.is_none())
        && app.store.get(credential_ref).await?.is_none()
    {
        return Err(conflict("Assigned provider connection is not authorized"));
    }
    let instructions = assembled_agent_instructions(&agent_store, &agent_id).await?;
    let mut run = json!({"agent_id":agent_id,"selection_plan":plan,"failover_state":model_selection::FailoverState::default(),"instructions":instructions,"provider_selection":selection.value(),"id":run_id,"session_id":id,"status":"queued","phase":"model","context_hydration_pending":true,"input":input,"calls":[],"call_index":0,"loaded_abilities":[],"ability_catalog_version":app.abilities.version(),
        "turns":0,"attempt":0,"pending":null,"generation":"","uncertain":false,"error":null,"created_at":now(),"updated_at":now()});
    run["requester_user_id"] = json!(crate::shared_chat::actor(app));
    if body["compaction_only"] == true {
        run["compaction_only"] = json!(true);
    }
    run["participation_generation"] = participation["generation"].clone();
    // Both role cursors survive recovery without changing one another on failover.
    run["system_selection_plan"] = json!(system_plan);
    // Role changes on the Models page after this revision apply at the next model call.
    run["model_catalog_revision"] = json!(catalog.revision);
    run["system_failover_state"] = json!(model_selection::FailoverState::default());
    run["attachments"] = json!(prepared.metadata.clone());
    run["chat_model_revision"] = json!(saved_model
        .as_ref()
        .and_then(|v| v["revision"].as_u64())
        .unwrap_or(0));
    // The conversation's effort belongs to its saved entry, not a per-message override.
    run["chat_effort"] = if body.get("model_entry_id").is_none() {
        saved_model
            .as_ref()
            .map(|v| v["effort"].clone())
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    // Freeze memory at admission; later edits cannot rewrite queued context.
    run["memory_context"] = if crate::shared_chat::actor(app) == app.store.current_user_id() {
        json!(memory::snapshot(
            app.store.list(&format!("memory:{id}:")).await?
        )?)
    } else {
        Value::Null
    };
    workflow::record(
        &mut run,
        &workflow_policy,
        &workflow::Input::Admission {
            request_digest: digest(&body),
        },
        workflow::Decision::Pass,
    )?;
    workflow::record(
        &mut run,
        &workflow_policy,
        &workflow::Input::AttachmentPreparation {
            attachment_count: prepared.metadata.len(),
        },
        workflow::Decision::Pass,
    )?;
    if let Some(manager) = manager {
        let record = json!({"id":run_id,"session_id":id,"agent_id":agent_id,
            "status":"pending_review","generation":"","deadline":0,"verdict":null,"error":null,
            "settings":manager["settings"],"settings_revision":manager["settings_revision"],"selection_plan":manager["selection_plan"],
            "failover_state":manager["failover_state"],"message":message,"run":run,
            "created_at":now(),"updated_at":now()});
        let result = json!({"run_id":null,"review_id":run_id,"admission":crate::manager::public(&record),"attachments":prepared.metadata});
        let mut docs = prepared.records;
        docs.push((
            format!("message_attachments:{run_id}"),
            json!(prepared.metadata),
        ));
        docs.push((format!("manager_review:{run_id}"), record));
        docs.push((format!("manager_pending:{id}"), json!({"id":run_id})));
        docs.extend(stop_delivery);
        docs.extend(panel_doc);
        if panel_notes.is_some() {
            docs.push((notes_key, json!({"notes":[]})));
        }
        return Ok(PreparedMessage {
            docs,
            kind: "manager_submission",
            message,
            result,
        });
    }
    let result = json!({"run_id":run_id,"attachments":prepared.metadata});
    let mut docs = prepared.records;
    docs.push((
        format!("message_attachments:{run_id}"),
        json!(prepared.metadata),
    ));
    docs.push((key("run", &run_id), run));
    docs.extend(stop_delivery);
    docs.extend(panel_doc);
    if panel_notes.is_some() {
        docs.push((notes_key, json!({"notes":[]})));
    }
    Ok(PreparedMessage {
        docs,
        kind: "message",
        message,
        result,
    })
}

/// Logs an admitted user message for per-level limits. The message is already
/// durable, so a logging failure is reported rather than failing the send.
pub(crate) async fn record_admitted_turn(app: &App, agent: &str, message_id: &Value) {
    let Some(message_id) = message_id.as_str() else {
        return;
    };
    if let Err(error) = super::agent_members::record_turn(app, agent, message_id).await {
        tracing::warn!("Message limit log unavailable: {error:#}");
    }
}
