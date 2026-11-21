//! HTTP handlers for chat operations.
use super::{
    access, anyhow, append_tool, bad, conflict, digest, json, key, loop_runtime, marker, memory,
    model_selection, now, parallel_batch, public_run, required, session_ability_state,
    session_agent_abilities, session_agent_id, session_mode, status, terminal, tools, uploads,
    user_apps, workflow, Api, ApiError, App, Duration, Event, HashMap, HeaderMap, Infallible, Json,
    KeepAlive, Path, Query, Result, Sse, State, StatusCode, SystemTime, Uuid, Value, UNIX_EPOCH,
};
use webagent_accounts_host::AccountsStore;
pub(crate) async fn workflow_policy_get(State(app): State<App>) -> Api {
    let policy = current_workflow_policy(&app).await?;
    Ok(Json(json!({"policy":policy,"revision":policy.revision()?})))
}
pub(crate) async fn workflow_policy_set(State(app): State<App>, Json(body): Json<Value>) -> Api {
    let _guard = app.gate.lock().await;
    let policy = workflow::Policy::resolve(Some(&body))
        .map_err(|_| bad("Invalid workflow policy, unsupported module, or ceiling exceeded"))?;
    let mut snapshot = app.runtime.write().await;
    let mut next = loop_runtime::Config::read(&app.runtime_path)?;
    next.apply_policy(&policy);
    crate::loop_runtime::save(&app, &next).await?;
    *snapshot = next;
    Ok(Json(json!({"policy":policy,"revision":policy.revision()?})))
}
pub(crate) async fn current_workflow_policy(app: &App) -> Result<workflow::Policy> {
    loop_runtime::Config::read(&app.runtime_path)?.policy()
}

pub(super) use super::message_admission::admission_error;
pub(crate) use super::message_admission::{
    prepare_followup, prepare_message, record_admitted_turn, send, send_message, PreparedMessage,
};
// Memory writes require a completed run in this same session. The API owns
// the decision; neither model text nor a Python tool can save memory directly.
pub(crate) async fn memory_list(State(app): State<App>, Path(sid): Path<String>) -> Api {
    let notes = app.store.list(&format!("memory:{sid}:")).await?;
    Ok(Json(json!({"memories":notes})))
}
pub(crate) async fn memory_save(
    State(app): State<App>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> std::result::Result<(StatusCode, Json<Value>), ApiError> {
    let source = required(&body, "source_run_id", 64)?;
    let id = required(&body, "client_memory_id", 64)?;
    let text = required(&body, "text", 1024)?;
    let _guard = app.gate.lock().await;
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| bad("System clock unavailable"))?
        .as_millis() as u64;
    let record = memory::record(&sid, source, id, text, created_at)
        .map_err(|_| bad("Invalid memory input"))?;
    let run = app
        .run(source)
        .await
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "Source run not found".into()))?;
    if run["session_id"] != sid {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "Source run not found".into(),
        ));
    }
    if status(&run) != "completed" {
        return Err(conflict("Source run must be completed"));
    }
    let key = format!("memory:{sid}:{id}");
    if let Some(existing) = app.store.get(&key).await? {
        if existing["text"] != text || existing["source_run_id"] != source {
            return Err(conflict("Memory identifier reused with different input"));
        }
        return Ok((StatusCode::OK, Json(existing)));
    }
    if app.store.list(&format!("memory:{sid}:")).await?.len() >= memory::MAX_COUNT {
        return Err(conflict("Session memory limit reached"));
    }
    app.store.put(&key, &record).await?;
    Ok((StatusCode::CREATED, Json(record)))
}
pub(crate) async fn memory_delete(
    State(app): State<App>,
    Path((sid, id)): Path<(String, String)>,
) -> Api {
    if Uuid::parse_str(&id).is_err() {
        return Err(ApiError(StatusCode::NOT_FOUND, "Memory not found".into()));
    }
    let key = format!("memory:{sid}:{id}");
    let record = app
        .store
        .get(&key)
        .await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Memory not found".into()))?;
    if record["session_id"] != sid {
        return Err(ApiError(StatusCode::NOT_FOUND, "Memory not found".into()));
    }
    app.store.delete(&key).await?;
    Ok(Json(json!({"deleted":true})))
}
/// How often an open event stream re-runs its full session, access and login check and
/// refreshes the session summary when nothing new arrives. New events always get a fresh
/// check before they are sent.
const STREAM_RECHECK_SECONDS: u64 = 3;
/// After this long without new events or summary changes the stream probes more slowly.
const STREAM_QUIET_SECONDS: u64 = 30;
const STREAM_BUSY_PROBE: Duration = Duration::from_millis(200);
const STREAM_QUIET_PROBE: Duration = Duration::from_secs(1);
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
    let opening = app
        .store
        .get(&key("session", &id))
        .await?
        .filter(|s| s["status"] == "active")
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Session not found".into()))?;
    let slot = crate::request_limits::stream_slot(auth_doc["_user_id"].as_str().unwrap_or(""))
        .ok_or_else(|| {
            ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many open live views; close some tabs".into(),
            )
        })?;
    let execution_epoch = opening["execution_epoch"].as_u64().unwrap_or(0);
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
    let user_id = auth_doc["_user_id"].as_str().unwrap_or("admin").to_owned();
    let generation = auth_doc["generation"].clone();
    let agent_id = session_agent_id(&app, &id).await?;
    let access_revision = crate::shared_chat::live_access(&app.store, &user_id, &id, &agent_id)
        .await
        .map_err(|_| ApiError(StatusCode::FORBIDDEN, "Agent unavailable".into()))?
        .revision;
    let resolved = crate::shared_chat::resolve(&app, &id, &user_id)
        .await
        .map_err(|_| ApiError(StatusCode::FORBIDDEN, "Session unavailable".into()))?;
    if !resolved.can_write() {
        app.store.shared_observe_audit(&id, &user_id).await?;
    }
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
            user_id,
            generation,
            agent_id,
            access_revision,
            None::<String>,
            0u64,
            now(),
        ),
        move |(
            app,
            id,
            mut cursor,
            mut queue,
            start,
            auth_key,
            expires,
            mut checked,
            user_id,
            generation,
            agent_id,
            access_revision,
            mut metadata_revision,
            mut metadata_checked,
            mut active_at,
        )| async move {
            loop {
                if now() - start > 3600 || app.stop.is_cancelled() || now() >= expires {
                    return None;
                }
                // The full check (session, access, login, account) runs every
                // STREAM_RECHECK_SECONDS and always before newly fetched events are sent;
                // the 200 ms probe between checks reads only the event log.
                if now() >= checked.saturating_add(STREAM_RECHECK_SECONDS) {
                    if !app
                        .store
                        .get(&key("session", &id))
                        .await
                        .is_ok_and(|session| {
                            session.is_some_and(|s| {
                                s["status"] == "active"
                                    && s["execution_epoch"].as_u64().unwrap_or(0) == execution_epoch
                            })
                        })
                    {
                        return None;
                    }
                    if crate::shared_chat::resolve(&app, &id, &user_id)
                        .await
                        .is_err()
                    {
                        return None;
                    }
                    let doc = app.store.get(&auth_key).await.ok().flatten();
                    let account = app.store.account_by_id(&user_id).await.ok().flatten();
                    let access =
                        crate::shared_chat::live_access(&app.store, &user_id, &id, &agent_id)
                            .await
                            .ok();
                    let valid = doc.as_ref().is_some_and(|doc| {
                        doc["generation"] == generation
                            && doc["expires"].as_u64().unwrap_or(0) > now()
                    }) && (if let Some(doc) =
                        doc.as_ref().filter(|d| d.get("agent_id").is_some())
                    {
                        super::embed::credential_valid(&app, doc).await
                    } else {
                        true
                    }) && account.as_ref().is_some_and(|account| {
                        account["generation"] == generation
                            && account["is_approved"] == true
                            && access::Principal::from_account(account)
                                .is_ok_and(|principal| principal.active())
                    }) && access.as_ref().is_some_and(|access| {
                        (access.admitted()
                            || app.chat_access.as_ref().is_some_and(|a| !a.can_write()))
                            && access.revision == access_revision
                    });
                    if !valid {
                        return None;
                    }
                    checked = now();
                }
                if !queue.is_empty() {
                    let item = queue.remove(0);
                    cursor = item["seq"].as_i64().unwrap_or(cursor);
                    let event = Event::default()
                        .id(cursor.to_string())
                        .data(item.to_string());
                    return Some((
                        Ok(event),
                        (
                            app,
                            id,
                            cursor,
                            queue,
                            start,
                            auth_key,
                            expires,
                            checked,
                            user_id,
                            generation,
                            agent_id,
                            access_revision,
                            metadata_revision,
                            metadata_checked,
                            active_at,
                        ),
                    ));
                }
                queue = match app.store.chat_events(&id, cursor).await {
                    Ok(mut v) => {
                        if uploads::message_files(&app, &mut v).await.is_err()
                            || crate::manager::decorate(&app, &mut v).await.is_err()
                            || crate::shared_projection::messages(&app, &mut v)
                                .await
                                .is_err()
                        {
                            return None;
                        }
                        v
                    }
                    Err(_) => return None,
                };
                if !queue.is_empty() {
                    // Re-run the full check before sending, then refresh the summary soon after.
                    checked = 0;
                    metadata_checked = 0;
                    active_at = now();
                    continue;
                }
                if now() >= metadata_checked.saturating_add(STREAM_RECHECK_SECONDS) {
                    metadata_checked = now();
                    let summary = app.store.session_read_summary(&id).await.ok()?;
                    let participation = app.store.participation_state(&id).await.ok()?;
                    let queue_state =
                        crate::followup_queue::public(&app.store.followup_queue(&id).await.ok()?);
                    // Private notes revisions refresh the dedicated element without adding transcript messages.
                    let notes_revision = if opening_main_owner(&app, &id, &user_id).await {
                        app.store
                            .for_agent(&agent_id)
                            .await
                            .ok()?
                            .main_chat_state()
                            .await
                            .ok()?["revision"]
                            .clone()
                    } else {
                        Value::Null
                    };
                    let component_revision = app.store.base_component_revision(&id).await.ok()?;
                    let app_view = tools::core_app_control::projection(&app, &id).await.ok()?;
                    let app_view_revision = app_view["revision"].as_u64().unwrap_or(0);
                    // Flag fields are part of the digest so a settled run or a dismissal is
                    // pushed to the open session's dropdown row.
                    let revision = digest(&json!([
                        component_revision,
                        app_view_revision,
                        summary["metadata_revision"],
                        summary["flag"],
                        summary["flag_run_id"],
                        summary["flag_dismissed"],
                        summary["notification"],
                        participation["revision"],
                        participation["generation"],
                        queue_state["queue_state"]["revision"],
                        notes_revision
                    ]));
                    if metadata_revision.as_ref() != Some(&revision) {
                        if now() > checked {
                            // A changed summary is sent only after a full check this second.
                            checked = 0;
                            metadata_checked = 0;
                            continue;
                        }
                        metadata_revision = Some(revision);
                        active_at = now();
                        // Metadata events deliberately carry no SSE ID: event-log
                        // sequence numbers are not public chat sequence numbers.
                        let event = Event::default().event("session_updated")
                            .data(json!({"component_revision":component_revision,"app_view_revision":app_view_revision,"session":summary,"participation":participation,"queue":queue_state["queue"],"queue_state":queue_state["queue_state"],"main_notes_revision":notes_revision,"can_manage_queue":crate::shared_chat::actor(&app)==app.store.current_user_id()}).to_string());
                        return Some((
                            Ok(event),
                            (
                                app,
                                id,
                                cursor,
                                queue,
                                start,
                                auth_key,
                                expires,
                                checked,
                                user_id,
                                generation,
                                agent_id,
                                access_revision,
                                metadata_revision,
                                metadata_checked,
                                active_at,
                            ),
                        ));
                    }
                }
                if queue.is_empty() {
                    // Probe quickly while the conversation is busy, slowly once it is quiet.
                    let quiet = now().saturating_sub(active_at) >= STREAM_QUIET_SECONDS;
                    tokio::time::sleep(if quiet {
                        STREAM_QUIET_PROBE
                    } else {
                        STREAM_BUSY_PROBE
                    })
                    .await;
                }
            }
        },
    );
    // The slot lives as long as the stream, so a closed tab frees it.
    let stream = futures_util::StreamExt::map(stream, move |event| {
        let _slot = &slot;
        event
    });
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
                    let Some(id) = view["id"].as_str() else {
                        continue;
                    };
                    let mut run = user_app.run(id).await?;
                    run["status"] = json!("stopped");
                    run["error"] = json!("Stopped by global shutoff");
                    let single = run["pending"]["state"] == "executing"
                        && !tools::is_read(run["pending"]["name"].as_str().unwrap_or(""))
                        && run["pending"]["external_job"]["kind"] != "v3_build";
                    if single || parallel_batch::executing_write(&run) {
                        // Only high-risk parallel writes stay uncertain; see mark_uncertain_writes.
                        let review = parallel_batch::mark_uncertain_writes(&mut run);
                        run["uncertain"] = json!(single || review);
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
    let stored = app
        .run(&id)
        .await
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "Run not found".into()))?;
    let requester = stored
        .pointer("/pending/authorization/owner_user_id")
        .and_then(Value::as_str)
        .or_else(|| stored["requester_user_id"].as_str())
        .unwrap_or(app.store.current_user_id());
    if matches!(action.as_str(), "resume" | "recovery")
        && requester != crate::shared_chat::actor(&app)
    {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Only the requesting user can reconcile or resume their run".into(),
        ));
    }
    let receipt_key = format!(
        "receipt:run:{id}:{action}:{}:{req}",
        crate::shared_chat::actor(&app)
    );
    if let Some(r) = app.store.get(&receipt_key).await? {
        if r["digest"] != digest(&body) {
            return Err(conflict("Identifier reused"));
        }
        let mut views = vec![r["result"].clone()];
        crate::shared_projection::runs(&app, &mut views).await?;
        return Ok(Json(views.remove(0)));
    }
    let mut run = app
        .run(&id)
        .await
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "Run not found".into()))?;
    if let Some(expected) = body.get("expected_run_revision") {
        if expected.as_u64().is_none() || expected != &run["run_revision"] {
            return Err(conflict("Run changed; no control applied"));
        }
    }
    let generation_live = app.store.participation_allows_run(&run).await?;
    if !generation_live
        && (action == "resume" || (action == "recovery" && body["decision"] == "retry"))
    {
        return Err(conflict(
            "Participation changed; send fresh instructions rather than restarting withdrawn work",
        ));
    }
    match action.as_str() {
        "cancel" => {
            // Storage fences finished runs; report the race instead of a storage failure.
            if terminal(status(&run)) {
                return Err(conflict("Run already finished"));
            }
            run["component_pause"] = Value::Null;
            if let Some(token) = app.active.lock().await.get(&id) {
                token.cancel();
            }
            if !terminal(status(&run)) {
                if run["pending"]["external_job"]["kind"] == "v3_build" {
                    run["status"] = json!("cancelled");
                } else if (run["pending"]["state"] == "executing"
                    && !tools::is_read(run["pending"]["name"].as_str().unwrap_or("")))
                    || (parallel_batch::executing_write(&run)
                        && parallel_batch::mark_uncertain_writes(&mut run))
                {
                    parallel_batch::mark_uncertain_writes(&mut run);
                    run["status"] = json!("recovery");
                    run["uncertain"] = json!(true);
                } else {
                    run["status"] = json!("cancelled");
                }
                run["error"] = json!("Cancelled by user");
                // Stop only this conversation; background work runs until a session stop.
                run["stopped_by"] = json!("user");
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
                && run["provider_recovery"]["safe_to_resume"] == true
            {
                let field = if run["provider_recovery"]["system"] == true {
                    "system_failover_state"
                } else {
                    "failover_state"
                };
                let mut state: model_selection::FailoverState =
                    serde_json::from_value(run[field].clone())
                        .map_err(|_| conflict("Provider recovery state is invalid"))?;
                if state.response_started || state.uncertain_tool_effect {
                    return Err(conflict("Provider response cannot be safely replayed"));
                }
                // Keep the pinned candidate and failover history. Only a human
                // resume grants another bounded attempt on this connection.
                state.retries = 0;
                run[field] = serde_json::to_value(state).map_err(anyhow::Error::from)?;
                run["provider_recovery"] = Value::Null;
                // The per-call attempt limit counts the spent retries too; resume starts it again.
                run["attempt"] = json!(0);
            }
            run["provider_retry_at"] = Value::Null;
            run["component_pause"] = Value::Null;
            let external = run["pending"]["external_job"]["kind"] == "v3_build";
            run["status"] = json!(if external {
                "waiting_external"
            } else {
                "queued"
            });
            run["error"] = Value::Null;
            run["attempt"] = json!(0);
            run["turns"] = json!(0);
            if run["pending"]["state"] == "executing" && !external {
                run["pending"]["state"] = json!("requested");
            }
            if let Some(items) = run["batch"].as_array_mut() {
                for item in items {
                    if item["state"] == "executing"
                        && tools::is_read(item["pending"]["name"].as_str().unwrap_or(""))
                    {
                        item["state"] = json!("requested");
                    }
                }
            }
        }
        "recovery" => {
            if run["uncertain"] != true {
                return Err(conflict("Run does not require recovery review"));
            }
            // A stop or restart can leave parallel calls recorded as executing. Sort them
            // now: reads run again, low-risk writes settle as outcome unknown, and only
            // high-risk writes stay for review.
            let mut settled_all = false;
            if run["batch"].is_array() && !app.active.lock().await.contains_key(&id) {
                if let Some(items) = run["batch"].as_array_mut() {
                    for item in items {
                        if item["state"] == "executing"
                            && tools::is_read(item["pending"]["name"].as_str().unwrap_or(""))
                        {
                            item["state"] = json!("requested");
                        }
                    }
                }
                settled_all = !parallel_batch::mark_uncertain_writes(&mut run)
                    && body["decision"] != "abandon";
            }
            if settled_all {
                if app.shutoff().await? {
                    return Err(conflict("Clear global shutoff first"));
                }
                run["uncertain"] = json!(false);
                run["status"] = json!("queued");
                run["pending"] = Value::Null;
                run["error"] = Value::Null;
            } else if run["batch"].is_array() {
                // RECOVERY-PER-CALL: the reviewer names the uncertain call when
                // more than one write was in flight. No other call is retried.
                let uncertain = run["batch"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .filter(|(_, item)| item["state"] == "uncertain")
                    .map(|(idx, _)| idx)
                    .collect::<Vec<_>>();
                // Abandoning ends the whole run, so it names no call.
                let idx = if body["decision"] == "abandon" {
                    0
                } else if let Some(call_id) = body["call_id"].as_str() {
                    uncertain
                        .iter()
                        .copied()
                        .find(|idx| run["batch"][*idx]["pending"]["call_id"] == call_id)
                        .ok_or_else(|| bad("Unknown uncertain call ID"))?
                } else if uncertain.len() == 1 {
                    uncertain[0]
                } else {
                    return Err(bad("Specify call_id for uncertain parallel writes"));
                };
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
                        // A retry is a new authorization attempt with a new ID.
                        run["batch"][idx]["state"] = json!("new");
                        run["batch"][idx]["pending"] = Value::Null;
                        run["batch"][idx]["result"] = Value::Null;
                    }
                    Some("accept") => {
                        let output = required(&body, "output", 1048576)?;
                        run["batch"][idx]["result"] =
                            json!({"status":"reconciled","output":output});
                        run["batch"][idx]["state"] = json!("done");
                    }
                    _ => return Err(bad("Choose retry, accept, or abandon")),
                }
                if body["decision"] != "abandon" {
                    run["uncertain"] = json!(parallel_batch::uncertain_count(&run) > 0);
                    run["status"] = json!(if run["uncertain"] == true {
                        "recovery"
                    } else if app.shutoff().await? {
                        "stopped"
                    } else {
                        "queued"
                    });
                }
                run["pending"] = Value::Null;
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
                }
            }
        }
        _ => return Err(ApiError(StatusCode::NOT_FOUND, "Unknown run action".into())),
    }
    if action == "recovery" && !generation_live && !terminal(status(&run)) {
        // Reconciliation records known outcomes but never revives withdrawn work.
        run["status"] = json!(if run["uncertain"] == true {
            "recovery"
        } else {
            "stopped"
        });
    }
    let result = public_run(&run);
    let resolved = action == "recovery" && status(&run) != "recovery";
    app.store
        .atomic_docs_event(
            vec![
                (key("run", &id), run),
                (receipt_key, json!({"digest":digest(&body),"result":result})),
            ],
            result["session_id"]
                .as_str()
                .expect("decision results carry their session"),
            "run_status",
            &result,
        )
        .await?;
    if resolved {
        // The reviewer's decision resolves the uncertain write that held the queue.
        if let Some(sid) = result["session_id"].as_str() {
            app.store
                .followup_release_hold(sid, crate::followup_queue::RECOVERY_HOLD)
                .await?;
        }
    }
    let mut views = vec![result];
    crate::shared_projection::runs(&app, &mut views).await?;
    Ok(Json(views.remove(0)))
}
pub(crate) async fn approval(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    let req = required(&body, "client_request_id", 128)?;
    let decision = required(&body, "decision", 20)?;
    if !matches!(decision, "approve" | "deny") {
        return Err(bad("Invalid approval decision"));
    }
    let _guard = app.gate.lock().await;
    let receipt_key = format!(
        "receipt:approval:{id}:{}:{req}",
        crate::shared_chat::actor(&app)
    );
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
    let mut run = app
        .run(
            view["id"]
                .as_str()
                .ok_or_else(|| anyhow!("Run without an ID"))?,
        )
        .await?;
    if status(&run) != "waiting_approval" {
        return Err(conflict("Approval is no longer pending"));
    }
    let sid = run["session_id"]
        .as_str()
        .ok_or_else(|| anyhow!("Run without a session"))?;
    if !webagent_actions::outcome::may_confirm_new_intent(
        &run,
        app.store.current_user_id(),
        crate::shared_chat::actor(&app),
    ) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Only the requesting user may confirm an additional matching action".into(),
        ));
    }
    if let Some(owner) = run["pending"]["authorization"]["owner_user_id"].as_str() {
        if owner != crate::shared_chat::actor(&app) {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                "Only the credential owner may authorize this tool".into(),
            ));
        }
    }
    if !crate::credential_authorization::valid(&app.store, &run, &run["pending"]).await? {
        run["pending"]["state"] = json!("cancelled");
        run["pending"]["decision"] = Value::Null;
        run["pending"]["human_approved"] = json!(false);
        run["status"] = json!("stopped");
        app.save(&mut run).await?;
        return Err(conflict("Credential authorization changed"));
    }
    if !app.store.participation_allows_run(&run).await? {
        return Err(conflict("Agent is inactive or participation changed"));
    }
    if crate::session_messaging::owns(run["pending"]["name"].as_str().unwrap_or("")) {
        if crate::shared_chat::actor(&app) != app.store.current_user_id() {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                "Only the private owner can approve messaging".into(),
            ));
        }
        let name = run["pending"]["name"]
            .as_str()
            .ok_or_else(|| anyhow!("Pending call without a name"))?;
        if decision == "approve" {
            let fresh =
                crate::session_messaging::binding(&app, &run, name, &run["pending"]["arguments"])
                    .await
                    .map_err(|_| conflict("Messaging destination changed"))?;
            if fresh != run["pending"]["messaging_binding"] {
                return Err(conflict(
                    "Messaging destination changed; request a fresh approval",
                ));
            }
        }
        if body
            .get("remember_messaging_route")
            .is_some_and(|v| !v.is_boolean())
        {
            return Err(bad("Invalid remember route choice"));
        }
        if body["remember_messaging_route"] == true {
            if decision != "approve" || run["pending"]["messaging_can_remember"] != true {
                return Err(bad("This approval cannot grant a route"));
            }
        }
    } else if body.get("remember_messaging_route").is_some() {
        return Err(bad("Messaging route choice is not applicable"));
    }
    // "Continue in Ask mode" on an Auto request approves the call with Ask as its target:
    // the mode tool then enters Ask instead, so the agent continues with approvals on.
    let to_ask = match body.get("alternate_mode") {
        None => false,
        Some(mode) if mode == "ask" => {
            if decision != "approve"
                || run["pending"]["name"] != "set_execution_mode"
                || run["pending"]["arguments"]["mode"] != "auto"
            {
                return Err(bad(
                    "Alternate mode applies only when approving a switch to Auto",
                ));
            }
            true
        }
        Some(_) => return Err(bad("Invalid alternate mode")),
    };
    let agent_id = run["agent_id"].as_str().unwrap_or("shared_default");
    let access = crate::shared_chat::live_access(
        &app.store,
        run["requester_user_id"]
            .as_str()
            .unwrap_or(app.store.current_user_id()),
        sid,
        agent_id,
    )
    .await
    .map_err(|_| conflict("Access changed; approval is invalid"))?;
    if !access.admitted() || !app.run_model_authorized(&run).await {
        run["status"] = json!("stopped");
        run["pending"]["decision"] = Value::Null;
        app.save(&mut run).await?;
        return Err(conflict("Access changed; approval is invalid"));
    }
    let mode_revision = session_mode(&app, sid).await?["revision"]
        .as_u64()
        .unwrap_or(0);
    let visibility = session_agent_abilities(&app, sid).await?;
    let ability_state = session_ability_state(&app, sid).await?;
    if run["pending"]["access_revision"] != json!(access.revision)
        || run["pending"]["mode_revision"].as_u64().unwrap_or(0) != mode_revision
        || run["pending"]["visibility_revision"].as_u64().unwrap_or(0)
            != visibility["revision"].as_u64().unwrap_or(0)
        || run["pending"]["ability_state_revision"]
            .as_u64()
            .unwrap_or(0)
            != ability_state["revision"].as_u64().unwrap_or(0)
    {
        if !run["pending"]["authorization"].is_null() {
            run["pending"]["state"] = json!("cancelled");
            run["pending"]["decision"] = Value::Null;
            run["pending"]["human_approved"] = json!(false);
            run["status"] = json!("stopped");
            app.save(&mut run).await?;
            return Err(conflict(
                "Policy or session mode changed; owner authorization cancelled",
            ));
        }
        // BATCH-APPROVAL-REVOCATION: a changed policy invalidates only this
        // call. Its next pass gets a new pending ID and fresh revisions.
        let stale_id = run["pending"]["id"].clone();
        if let Some(items) = run["batch"].as_array_mut() {
            for item in items {
                if item["pending"]["id"] == stale_id {
                    item["state"] = json!("new");
                    item["pending"] = Value::Null;
                    item["result"] = Value::Null;
                    break;
                }
            }
            run["pending"] = Value::Null;
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
    if crate::session_messaging::owns(run["pending"]["name"].as_str().unwrap_or("")) {
        run["pending"]["remember_messaging_route"] =
            json!(body["remember_messaging_route"] == true);
    }
    run["pending"]["decision"] = json!(decision);
    if to_ask {
        run["pending"]["alternate_mode"] = json!("ask");
    }
    run["status"] = json!(if app.shutoff().await? {
        "stopped"
    } else {
        "queued"
    });
    let result = public_run(&run);
    app.store
        .atomic_docs_event(
            vec![
                (
                    key(
                        "run",
                        run["id"].as_str().expect("stored runs carry their ID"),
                    ),
                    run.clone(),
                ),
                (receipt_key, json!({"digest":digest(&body),"result":result})),
            ],
            run["session_id"]
                .as_str()
                .expect("stored runs carry their session"),
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
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let rid = crate::runtime_tests::queued(&app, &sid).await;
        let mut run = app.run(&rid).await.unwrap();
        run["requester_user_id"] = json!("alice");
        run["status"] = json!("paused");
        run["pending"] = json!({"id":"private-panel","state":"waiting","arguments":{"private":"owner-only"},"authorization":{"owner_user_id":"alice"}});
        app.store.put(&key("run", &rid), &run).await.unwrap();
        let mut bob = app.clone();
        bob.chat_actor = Some("bob".into());
        let error = command(
            State(bob.clone()),
            Path((rid.clone(), "resume".into())),
            Json(json!({"client_request_id":"resume"})),
        )
        .await
        .unwrap_err();
        assert_eq!(error.0, StatusCode::FORBIDDEN);
        for _ in 0..2 {
            let Json(result) = command(
                State(bob.clone()),
                Path((rid.clone(), "cancel".into())),
                Json(json!({"client_request_id":"stop"})),
            )
            .await
            .unwrap();
            assert_eq!(result["can_decide"], false);
            assert!(result["pending"].is_null());
            assert!(!result.to_string().contains("owner-only"));
        }
    }
}

// Shared observers must not learn even the existence or revision of private Main Notes.
async fn opening_main_owner(app: &App, sid: &str, user: &str) -> bool {
    user == app.store.current_user_id()
        && app
            .store
            .get(&key("session", sid))
            .await
            .ok()
            .flatten()
            .is_some_and(|s| s["main_chat"] == true)
}
