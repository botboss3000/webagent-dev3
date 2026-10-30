//! Two-stage chat stop. A run cancel stops only the conversation; a session stop also
//! ends the background work it leaves running and records it for the agent.
use super::*;

const ACTIVE: [&str; 5] = [
    "queued",
    "running",
    "waiting_approval",
    "waiting_gate",
    "waiting_external",
];

fn active(run: &Value) -> bool {
    ACTIVE.contains(&status(run))
}

/// Active runs in sub-agent conversations that `sid` spawned.
async fn child_runs(app: &App, sid: &str) -> Result<Vec<Value>> {
    let mut children = Vec::new();
    for receipt in app
        .store
        .list(&format!("receipt:agent_message:{sid}:"))
        .await?
    {
        if receipt["result"]["session_created"] == true {
            if let Some(id) = receipt["result"]["session_id"].as_str() {
                children.push(id.to_owned());
            }
        }
    }
    Ok(app
        .store
        .active_runs()
        .await?
        .into_iter()
        .filter(|run| active(run) && children.iter().any(|id| run["session_id"] == id.as_str()))
        .collect())
}

/// Open terminals and preview tunnels the worker holds for `sid`; unknown counts as none.
async fn worker_resources(app: &App, sid: &str) -> Value {
    if !app.tool_socket.exists() {
        return json!({});
    }
    let empty = json!({});
    let call = crate::tools::execute_trusted(
        "administrator_resources",
        &empty,
        sid,
        &app.tool_socket,
        None,
        CancellationToken::new(),
    );
    match tokio::time::timeout(Duration::from_secs(2), call).await {
        Ok(Ok(value)) if value["status"] == "success" => value["output"].clone(),
        _ => json!({}),
    }
}

/// Background work that survives a conversation stop, as `{kind,id,label}` rows.
pub(crate) async fn background_processes(app: &App, sid: &str) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    for task in app.store.background_scan(Some(sid)).await? {
        if matches!(task["status"].as_str(), Some("queued" | "running")) {
            let name = task["name"].as_str().unwrap_or("task");
            rows.push(json!({"kind":"background_task","id":task["id"],"label":format!("Background {name}")}));
        }
    }
    for run in child_runs(app, sid).await? {
        rows.push(
            json!({"kind":"sub_agent","id":run["session_id"],"label":"Sub-agent conversation"}),
        );
    }
    let host = crate::tools::administrator::Host(app);
    let jobs = webagent_administration_host::watch::session_jobs(&host, sid)
        .await
        .unwrap_or(0);
    for n in 0..jobs {
        rows.push(json!({"kind":"build","id":format!("build-{n}"),"label":"Build or host job"}));
    }
    if app
        .store
        .get(&format!("administrator_browser:{sid}"))
        .await?
        .is_some()
    {
        rows.push(json!({"kind":"browser","id":"browser","label":"Development browser login"}));
    }
    let worker = worker_resources(app, sid).await;
    for (kind, label) in [("terminals", "Terminal"), ("tunnels", "Preview tunnel")] {
        for n in 0..worker[kind].as_u64().unwrap_or(0) {
            rows.push(json!({"kind":kind,"id":format!("{kind}-{n}"),"label":label}));
        }
    }
    for automation in crate::modules::session_automations(app, sid).await? {
        let prompt: String = automation["prompt"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(60)
            .collect();
        rows.push(json!({"kind":"automation","id":automation["id"],"label":format!("Automation: {prompt}")}));
    }
    Ok(rows)
}

/// Stop everything `sid` runs: its runs, sub-agent runs, background tasks, automations and
/// persistent resources. Records what was stopped so the next message tells the agent.
pub(crate) async fn stop_session_work(
    app: &App,
    sid: &str,
    request: &str,
) -> Result<Vec<Value>, ApiError> {
    let processes = background_processes(app, sid).await?;
    let mut runs: Vec<Value> = app
        .store
        .active_runs()
        .await?
        .into_iter()
        .filter(|run| active(run) && run["session_id"] == sid)
        .collect();
    runs.extend(child_runs(app, sid).await?);
    for run in runs {
        let Some(rid) = run["id"].as_str() else {
            continue;
        };
        // A run that finished meanwhile reports a conflict; nothing remains to stop.
        let _ = crate::command(
            State(app.clone()),
            Path((rid.to_owned(), "cancel".into())),
            Json(json!({"client_request_id":format!("session-stop:{request}:{rid}")})),
        )
        .await;
    }
    for mut task in app.store.background_scan(Some(sid)).await? {
        if matches!(task["status"].as_str(), Some("queued" | "running")) {
            let revision = task["revision"].as_u64().unwrap_or(0);
            task["status"] = json!("cancelled");
            // A lost race means the task already moved on; its executor rechecks authority.
            app.store.background_cas(&task, Some(revision)).await?;
        }
    }
    crate::modules::pause_session_automations(app, sid).await?;
    crate::phase3_api::quiesce_session_resources(app, sid).await?;
    // `session_id` lets permanent deletion remove this record with the conversation.
    let record =
        json!({"session_id":sid,"processes":processes,"stopped_at":now(),"delivered":false});
    app.store
        .put(&format!("session_stopped:{sid}"), &record)
        .await?;
    app.store
        .event(sid, "session_stopped", &json!({"count":processes.len()}))
        .await?;
    crate::wake::notify(app).await;
    Ok(processes)
}

/// One-time model note for the first message after a total stop.
pub(crate) fn stopped_note(record: &Value) -> String {
    let labels: Vec<&str> = record["processes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|process| process["label"].as_str())
        .collect();
    let stopped = if labels.is_empty() {
        "no background work".to_owned()
    } else {
        labels.join("; ")
    };
    format!(
        "[Host note: the user fully stopped this conversation and its background work. \
         Stopped: {stopped}. Do not restart any of it on your own. If the request needs \
         any of it, list what was stopped and ask the user which to restart.]"
    )
}

pub(crate) async fn background(State(app): State<App>, Path(sid): Path<String>) -> Api {
    if app.store.get(&key("session", &sid)).await?.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "Session not found".into()));
    }
    let processes = background_processes(&app, &sid).await?;
    Ok(Json(json!({"count":processes.len(),"processes":processes})))
}

pub(crate) async fn stop(
    State(app): State<App>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    let request = required(&body, "client_request_id", 128)?;
    let stopped = stop_session_work(&app, &sid, request).await?;
    Ok(Json(json!({"stopped":stopped,"count":0})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopped_note_lists_work_and_asks_before_restart() {
        let record = json!({"processes":[{"label":"Build or host job"},{"label":"Terminal"}]});
        let note = stopped_note(&record);
        assert!(note.contains("Stopped: Build or host job; Terminal."));
        assert!(note.contains("ask the user which to restart"));
        assert!(stopped_note(&json!({})).contains("Stopped: no background work."));
    }
}
