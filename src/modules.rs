//! Optional administrator-installed packages. The host owns storage and admission;
//! package reducers never receive encryption keys, raw databases or approvals.
use crate::*;
use std::{
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path as FsPath},
    process::Stdio,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
const IDS: [&str; 2] = ["automation", "outbound_notifications"];
// A bounded sealed namespace plus a bounded read page may appear in one response.
const LIMIT: usize = 8 * 1024 * 1024;
pub(crate) fn package_ids() -> Vec<&'static str> {
    IDS.to_vec()
}
pub(crate) fn is_read(name: &str) -> bool {
    matches!(
        name,
        "list_event_sources"
            | "list_delivery_channels"
            | "list_automations"
            | "get_automation"
            | "list_event_subscriptions"
            | "get_automation_history"
            | "history"
            | "discover"
            | "status"
    )
}
fn package_lock(app: &App, id: &str, exclusive: bool) -> Result<File> {
    if !IDS.contains(&id) {
        return Err(anyhow!("Unknown module"));
    }
    let dir = app.store.root.join("modules");
    std::fs::create_dir_all(&dir)?;
    use std::os::unix::fs::OpenOptionsExt;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(dir.join(format!("package-{id}.lock")))?;
    if exclusive {
        file.try_lock_exclusive()
    } else {
        FileExt::try_lock_shared(&file)
    }
    .map_err(|_| anyhow!("Module lifecycle busy"))?;
    Ok(file)
}
fn host_lock(app: &App, id: &str) -> Result<File> {
    let dir = app.store.root.join("modules");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "{}-{}.lock",
        id,
        digest(&json!(app.store.current_user_id()))
    ));
    use std::os::unix::fs::OpenOptionsExt;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.try_lock_exclusive()
        .map_err(|_| anyhow!("Module busy"))?;
    Ok(file)
}
pub(crate) fn package_root(id: &str) -> Option<PathBuf> {
    IDS.contains(&id).then(|| {
        PathBuf::from(std::env::var("V3_MODULE_ROOT").unwrap_or_else(|_| {
            PathBuf::from(std::env::var("V3_STATE_ROOT").unwrap_or_else(|_| "/state".into()))
                .join("plugins/services")
                .to_string_lossy()
                .into_owned()
        }))
        .join(id)
    })
}
fn manifest(id: &str) -> Result<(PathBuf, Value)> {
    let root = package_root(id).ok_or_else(|| anyhow!("Unknown module"))?;
    if root.join(".disabled").exists() {
        return Err(anyhow!("Module disabled"));
    }
    let file = root.join("package.json");
    let meta = std::fs::symlink_metadata(&file)?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.permissions().mode() & 0o022 != 0 {
        return Err(anyhow!("Unsafe package manifest"));
    }
    let value: Value = serde_json::from_slice(&std::fs::read(file)?)?;
    if value["protocol"] != 1 || value["id"] != id {
        return Err(anyhow!("Unsupported module protocol"));
    }
    let name = value["executable"]
        .as_str()
        .ok_or_else(|| anyhow!("Module executable required"))?;
    if FsPath::new(name)
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(anyhow!("Invalid module executable"));
    }
    let executable = root.join(name);
    let binary = std::fs::symlink_metadata(&executable)?;
    if !binary.is_file()
        || binary.file_type().is_symlink()
        || binary.permissions().mode() & 0o022 != 0
        || binary.permissions().mode() & 0o111 == 0
        || binary.uid() != meta.uid()
    {
        return Err(anyhow!("Unsafe module executable"));
    }
    let canonical = executable.canonicalize()?;
    if !canonical.starts_with(root.canonicalize()?) {
        return Err(anyhow!("Module executable escaped package"));
    }
    Ok((canonical, value))
}
fn health_path(root: &FsPath, id: &str) -> PathBuf {
    root.join("modules").join(format!("health-{id}.json"))
}
fn health_current(health: &Value, gen: &str, at: u64) -> bool {
    health["healthy"] == true
        && health["generation"] == gen
        && health["checked_at"]
            .as_u64()
            .is_some_and(|checked| checked <= at && at.saturating_sub(checked) <= 15)
}
pub(crate) fn available(id: &str) -> bool {
    let Ok(gen) = generation(id) else {
        return false;
    };
    let root = PathBuf::from(std::env::var("V3_STATE_ROOT").unwrap_or_else(|_| "/state".into()));
    std::fs::read(health_path(&root, id))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|health| health_current(&health, &gen, now()))
}
fn mark_health(app: &App, id: &str, gen: &str, healthy: bool) -> Result<()> {
    let dir = app.store.root.join("modules");
    std::fs::create_dir_all(&dir)?;
    let file = health_path(&app.store.root, id);
    let temp = dir.join(format!(".health-{}.tmp", uuid()));
    use std::os::unix::fs::OpenOptionsExt;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    output.write_all(&serde_json::to_vec(
        &json!({"generation":gen,"healthy":healthy,"checked_at":now()}),
    )?)?;
    output.sync_all()?;
    std::fs::rename(temp, file)?;
    Ok(())
}
pub(crate) async fn binding_lease(app: &App, run: &Value) -> Result<Option<File>> {
    if run["module_binding"].is_null() {
        return Ok(None);
    }
    let id = run["module_binding"]["id"]
        .as_str()
        .ok_or_else(|| anyhow!("Invalid module binding"))?;
    let lease = package_lock(app, id, false)?;
    if !binding_valid(app, run).await {
        return Err(anyhow!("Module admission revoked"));
    }
    Ok(Some(lease))
}
fn generation(id: &str) -> Result<String> {
    let (path, manifest) = manifest(id)?;
    let meta = std::fs::metadata(path)?;
    Ok(digest(&json!([
        manifest["generation"],
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec()
    ])))
}
pub(crate) fn schemas() -> Vec<Value> {
    let mut schemas = Vec::new();
    for id in IDS {
        if available(id) {
            if let Some(root) = package_root(id) {
                if let Ok(bytes) = std::fs::read(root.join("tool-schemas.json")) {
                    if bytes.len() < 256 * 1024 {
                        if let Ok(items) = serde_json::from_slice::<Vec<Value>>(&bytes) {
                            schemas.extend(items.into_iter().map(|item| {
                                if item["function"].is_object() {
                                    let mut schema = item["function"].clone();
                                    schema["type"] = json!("function");
                                    schema
                                } else {
                                    item
                                }
                            }));
                        }
                    }
                }
            }
        }
    }
    schemas
}
pub(crate) fn owns_tool(name: &str) -> bool {
    schemas().iter().any(|schema| schema["name"] == name)
}
fn tool_package(name: &str) -> Option<&'static str> {
    IDS.into_iter().find(|id| {
        available(id)
            && package_root(id)
                .and_then(|root| std::fs::read(root.join("tool-schemas.json")).ok())
                .and_then(|bytes| serde_json::from_slice::<Vec<Value>>(&bytes).ok())
                .is_some_and(|items| {
                    items
                        .iter()
                        .any(|item| item["name"] == name || item["function"]["name"] == name)
                })
    })
}
async fn administrator(app: &App) -> bool {
    app.store
        .account_by_id(shared_chat::actor(app))
        .await
        .ok()
        .flatten()
        .and_then(|v| access::Principal::from_account(&v).ok())
        .is_some_and(|p| p.active() && p.administrator())
}
async fn reduce(id: &str, request: &Value) -> Result<Value> {
    let (executable, _) = manifest(id)?;
    let pinned = generation(id)?;
    let mut child = tokio::process::Command::new(executable)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut bytes = serde_json::to_vec(request)?;
    if bytes.len() > LIMIT {
        return Err(anyhow!("Module request limit"));
    }
    bytes.push(b'\n');
    let input = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("Module input unavailable"))?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("Module output unavailable"))?;
    let result = tokio::time::timeout(Duration::from_secs(20), async move {
        let mut input = input;
        input.write_all(&bytes).await?;
        input.shutdown().await?;
        let mut line = Vec::new();
        let reader = BufReader::new(output);
        use tokio::io::AsyncReadExt;
        let mut bounded = reader.take((LIMIT + 1) as u64);
        bounded.read_until(b'\n', &mut line).await?;
        if line.len() > LIMIT {
            return Err(anyhow!("Module response limit"));
        }
        let value: Value = serde_json::from_slice(&line)?;
        Ok::<_, anyhow::Error>(value)
    })
    .await
    .map_err(|_| anyhow!("Module timeout"))??;
    let _ = child.kill().await;
    let _ = child.wait().await;
    if generation(id)? != pinned {
        return Err(anyhow!("Module changed during request"));
    }
    if result["ok"] != true {
        return Err(anyhow!(safe_rejection(
            result["error"].as_str().unwrap_or("module_rejected")
        )));
    }
    if !result["state"].is_object() {
        return Err(anyhow!("Invalid module state"));
    }
    Ok(result)
}
fn safe_rejection(error: &str) -> &'static str {
    match error {
        "revision_conflict" | "revision conflict" => "revision_conflict",
        "human_activation_required" => "human_activation_required",
        "automation_capacity" | "event_queue_capacity" => "automation_capacity",
        "unsupported_runner" => "unsupported_runner",
        "unsupported_or_missing_destination" => "destination_required",
        "recurring_schedules_disabled" => "recurring_schedules_disabled",
        "event_subscriptions_disabled" => "event_subscriptions_disabled",
        "automation_busy" => "automation_busy",
        "inactive_occurrence" => "inactive_occurrence",
        "origin_occurrence_required" => "origin_occurrence_required",
        "not_found" | "occurrence_not_found" => "not_found",
        "unsupported_operation" | "unsupported operation" => "unsupported_operation",
        _ => "invalid_arguments_or_unavailable",
    }
}
fn request(
    app: &App,
    id: &str,
    op: &str,
    args: Value,
    state: Value,
    gen: &str,
    admin: bool,
    dispatch: bool,
    scope: &Value,
) -> Value {
    json!({"protocol":1,"operation":op,"args":args,"state":state,"now_ms":now()*1000,
        "context":{"user_id":app.store.current_user_id(),"agent_id":scope["agent_id"],"session_id":scope["session_id"],"settings":scope["settings"],"startup":scope["startup"],"origin_occurrence":scope["origin_occurrence"],"interactive_user":scope["interactive_user"],"generation":gen,"administrator":admin,"dispatch_authorized":dispatch,"module_id":id,"latest_event_seq":scope["latest_event_seq"],"event_scan_cursor":scope["event_scan_cursor"],"readable_agents":scope["readable_agents"]}})
}
// Caller input never supplies this read authority. Filter before package pagination.
fn scoped_read_operation(op: &str) -> bool {
    matches!(
        op,
        "list_automations"
            | "list_event_subscriptions"
            | "history"
            | "get_automation"
            | "get_automation_history"
    )
}
async fn readable_agents(app: &App, envelope: &Value, scope: &Value) -> Vec<String> {
    let candidates: std::collections::BTreeSet<String> = if scope["interactive_user"] == true {
        envelope["state"]["definitions"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(_, definition)| definition["agent_id"].as_str().map(str::to_owned))
            .collect()
    } else {
        scope["agent_id"]
            .as_str()
            .into_iter()
            .map(str::to_owned)
            .collect()
    };
    let mut allowed = Vec::new();
    for agent in candidates {
        if execution_scope(app, &agent, None, false).await.is_ok() {
            allowed.push(agent);
        }
    }
    allowed
}
fn apply_completion_ack(envelope: &mut Value, completed_oid: Option<&str>) {
    if let Some(oid) = completed_oid {
        if let Some(tracking) = envelope["admission_results"].as_object_mut() {
            tracking.remove(oid);
        }
    }
}
async fn perform(
    app: &App,
    id: &str,
    op: &str,
    args: Value,
    expected: Option<u64>,
    receipt: Option<(&str, &str)>,
    internal: bool,
    scope: Value,
) -> Result<Value> {
    let _package_lease = if internal {
        None
    } else {
        Some(package_lock(app, id, false)?)
    };
    let _lock = if internal {
        None
    } else {
        Some(host_lock(app, id)?)
    };
    if id == "outbound_notifications" && !administrator(app).await {
        return Err(anyhow!("Administrator required"));
    }
    let gen = generation(id)?;
    let installed = app.store.get(&format!("module_install:{id}")).await?;
    if !installed.is_some_and(|record| record["enabled"] == true && record["generation"] == gen) {
        return Err(anyhow!("Module installation unavailable"));
    }
    if !internal && !available(id) {
        return Err(anyhow!("Module health unavailable"));
    }
    let mut envelope = app.store.module_state(id).await?;
    let (_, descriptor) = manifest(id)?;
    if !internal
        && !descriptor["public_operations"]
            .as_array()
            .is_some_and(|ops| ops.iter().any(|v| v == op))
    {
        return Err(anyhow!("Module public operation unavailable"));
    }
    let readonly = descriptor["read_operations"]
        .as_array()
        .is_some_and(|ops| ops.iter().any(|v| v == op));
    let receipt = if readonly { None } else { receipt };
    let revision = envelope["revision"].as_u64().unwrap_or(0);
    if expected.is_some_and(|v| v != revision) {
        return Err(anyhow!("Module revision changed"));
    }
    if let Some((rid, input_digest)) = receipt {
        if let Some(saved) = envelope["receipts"][rid].as_object() {
            if saved.get("digest") == Some(&json!(input_digest)) {
                return Ok(saved.get("result").cloned().unwrap_or(Value::Null));
            }
            return Err(anyhow!("Module request identifier reused"));
        }
    }
    let admin = administrator(app).await;
    let mut scope = scope;
    if id == "automation" && !internal && readonly && scoped_read_operation(op) {
        scope["readable_agents"] = json!(readable_agents(app, &envelope, &scope).await);
    }
    if IDS.contains(&id) {
        scope["latest_event_seq"] = json!(app.store.notification_page(i64::MAX, 1).await?
            ["latest_seq"]
            .as_i64()
            .unwrap_or(0));
    }
    if envelope["generation"].is_string() && envelope["generation"] != gen {
        let reset = reduce(
            id,
            &request(
                app,
                id,
                "deactivate",
                json!({}),
                envelope["state"].clone(),
                &gen,
                admin,
                false,
                &scope,
            ),
        )
        .await?;
        envelope["state"] = reset["state"].clone();
        envelope["pending"] = json!([]);
        envelope["admission_results"] = json!({});
    }
    let completed_oid = if internal && op == "complete" {
        args["occurrence_id"].as_str().map(str::to_owned)
    } else {
        None
    };
    let result = reduce(
        id,
        &request(
            app,
            id,
            op,
            args,
            envelope["state"].clone(),
            &gen,
            admin,
            internal,
            &scope,
        ),
    )
    .await?;
    envelope["state"] = result["state"].clone();
    // Completion delivery and retry classification share this namespace CAS.
    // A crash cannot deliver one terminal failure twice and exhaust its retry budget.
    apply_completion_ack(&mut envelope, completed_oid.as_deref());

    envelope["generation"] = json!(gen);
    if !envelope["pending"].is_array() {
        envelope["pending"] = json!([]);
    }
    if let Some(admissions) = result["admissions"].as_array() {
        if admissions.len() > 128
            || envelope["pending"].as_array().unwrap().len() + admissions.len() > 256
        {
            return Err(anyhow!("Module admission queue full"));
        }
        for admission in admissions {
            let oid = admission["occurrence_id"]
                .as_str()
                .ok_or_else(|| anyhow!("Occurrence identity missing"))?;
            if oid.is_empty() || oid.len() > 256 {
                return Err(anyhow!("Invalid occurrence identity"));
            }
            if !envelope["pending"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["occurrence_id"] == oid)
            {
                envelope["pending"]
                    .as_array_mut()
                    .unwrap()
                    .push(admission.clone());
            }
        }
    }
    let unchanged = readonly
        && envelope["generation"] == gen
        && envelope["state"] == app.store.module_state(id).await?["state"];
    let public = json!({"result":result["result"],"revision":if unchanged{revision}else{revision+1},"available":true});
    if unchanged {
        return Ok(public);
    }
    if let Some((rid, input_digest)) = receipt {
        if !envelope["receipts"].is_object() {
            envelope["receipts"] = json!({});
        }
        // Receipts retain mutation idempotency and are bounded with explicit quota.
        if envelope["receipts"].as_object().unwrap().len() >= 4096 {
            return Err(anyhow!("Module receipt quota reached"));
        }
        envelope["receipts"][rid] = json!({"digest":input_digest,"result":public});
    }
    app.store.module_cas(id, revision, &envelope).await?;
    if !internal {
        wake::notify(app).await;
    }
    Ok(public)
}
pub(crate) async fn tool_call(
    app: &App,
    sid: &str,
    rid: &str,
    name: &str,
    args: &Value,
    pending_id: &str,
    cancel: CancellationToken,
) -> Result<Value> {
    if cancel.is_cancelled() {
        return Err(anyhow!("Cancelled"));
    }
    let id = tool_package(name).ok_or_else(|| anyhow!("Module tool unavailable"))?;
    let run_app = shared_chat::scope(app.clone(), sid)
        .await
        .map_err(|_| anyhow!("Session unavailable"))?;
    let mut scoped = run_app;
    scoped.store = scoped.store.for_user(shared_chat::actor(app)).await?;
    scoped.chat_actor = None;
    scoped.chat_access = None;
    let mut args = args.clone();
    if args.is_object() {
        args["origin_session_id"] = json!(sid);
    }
    let agent = session_agent_id(app, sid)
        .await
        .map_err(|_| anyhow!("Agent unavailable"))?;
    if let Some(target) = args["id"].as_str() {
        let state = scoped.store.module_state(id).await?;
        if let Some(owner) = state["state"]["definitions"][target]["agent_id"].as_str() {
            if owner != agent {
                return Err(anyhow!("Automation belongs to another agent"));
            }
        }
    }
    let mut scope = execution_scope(&scoped, &agent, Some(sid), false).await?;
    if name == "remember_automation_state" {
        let run = app.run(rid).await?;
        if run["module_binding"]["id"] != "automation"
            || run["requester_user_id"] != shared_chat::actor(app)
            || run["module_binding"]["occurrence_id"] != args["occurrence_id"]
            || run["module_binding"]["automation_id"] != args["id"]
        {
            return Err(anyhow!("Automation occurrence unavailable"));
        }
        scope["origin_occurrence"] = run["module_binding"]["occurrence_id"].clone();
    }
    perform(
        &scoped,
        id,
        name,
        args.clone(),
        None,
        Some((pending_id, &digest(&args))),
        false,
        scope,
    )
    .await
}
async fn execution_scope(
    app: &App,
    agent: &str,
    sid: Option<&str>,
    interactive: bool,
) -> Result<Value> {
    let access = app
        .store
        .live_access(app.store.current_user_id(), agent)
        .await?;
    if !access.admitted() || access.ability_permission("automation") == access::Permission::Deny {
        return Err(anyhow!("Automation ability unavailable"));
    }
    let visibility = app
        .store
        .for_agent(agent)
        .await?
        .agent_abilities_for(agent)
        .await?;
    if visibility["visibility"]["automation"] == "off" {
        return Err(anyhow!("Automation ability disabled"));
    }
    Ok(
        json!({"agent_id":agent,"session_id":sid,"settings":visibility["settings"]["automation"],"interactive_user":interactive}),
    )
}
pub(crate) async fn admin_list(State(app): State<App>) -> Api {
    if !administrator(&app).await {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Administrator required".into(),
        ));
    }
    let mut modules = Vec::new();
    for id in IDS {
        let installed = package_root(id).is_some_and(|root| root.join("package.json").is_file());
        let enabled = app
            .store
            .get(&format!("module_install:{id}"))
            .await?
            .is_some_and(|state| state["enabled"] == true);
        modules.push(json!({"id":id,"label":if id=="automation"{"Automation"}else{"Outbound Notifications"},"installed":installed,"available":available(id),"enabled":enabled,"protocol":1}));
    }
    Ok(Json(json!({"modules":modules})))
}
pub(crate) async fn status_api(State(app): State<App>, Path(id): Path<String>) -> Api {
    if package_root(&id).is_none() {
        return Err(bad("Unknown module"));
    }
    if id == "outbound_notifications" && !administrator(&app).await {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Administrator required".into(),
        ));
    }
    let state = app.store.module_state(&id).await?;
    Ok(Json(
        json!({"available":available(&id),"id":id,"revision":state["revision"],"protocol":1,"installed":package_root(&id).is_some_and(|root|root.join("package.json").is_file()),"healthy":available(&id),"event_gap":state["event_gap"]}),
    ))
}
pub(crate) async fn call_api(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    let op = required(&body, "action", 64)?;
    if matches!(
        op,
        "tick" | "deactivate" | "reserve_delivery" | "execute_delivery" | "admitted" | "complete"
    ) {
        return Err(bad("Internal operation"));
    }
    let rid = required(&body, "client_request_id", 128)?;
    let requested = body["agent_id"]
        .as_str()
        .or_else(|| body["args"]["agent_id"].as_str());
    let state = app.store.module_state(&id).await?;
    let owner = body["args"]["id"]
        .as_str()
        .and_then(|target| state["state"]["definitions"][target]["agent_id"].as_str());
    if owner.is_some() && requested.is_some() && owner != requested {
        return Err(bad("Automation agent mismatch"));
    }
    let agent = owner.or(requested).unwrap_or("shared_default");
    let scope = if id == "automation" && scoped_read_operation(op) && owner.is_none() {
        json!({"interactive_user":true})
    } else if id == "automation" {
        execution_scope(&app, agent, body["session_id"].as_str(), true)
            .await
            .map_err(|_| {
                ApiError(
                    StatusCode::FORBIDDEN,
                    "Automation ability unavailable".into(),
                )
            })?
    } else {
        json!({})
    };
    let result = perform(
        &app,
        &id,
        op,
        body["args"].clone(),
        body["expected_revision"].as_u64(),
        Some((rid, &digest(&body))),
        false,
        scope,
    )
    .await
    .map_err(|error| {
        if error.to_string().contains("revision") {
            conflict("Module revision changed")
        } else {
            bad(safe_rejection(&error.to_string()))
        }
    })?;
    Ok(Json(result))
}
pub(crate) async fn binding_valid(app: &App, run: &Value) -> bool {
    let binding = &run["module_binding"];
    if binding.is_null() {
        return true;
    }
    let Some(id) = binding["id"].as_str() else {
        return false;
    };
    generation(id).is_ok_and(|g| binding["generation"] == g)
        && app
            .store
            .get(&format!("module_install:{id}"))
            .await
            .ok()
            .flatten()
            .is_some_and(|v| v["enabled"] == true && v["generation"] == binding["generation"])
}
// Remembered state is retained in full, but only a bounded UTF-8 prefix enters context.
fn automation_prompt(prompt: &str, admission: &Value) -> Result<String> {
    if admission["event"].is_null() && admission["memory"].is_null() {
        return Ok(prompt.to_owned());
    }
    let mut evidence = serde_json::to_string(
        &json!({"event":admission["event"],"remembered_state":admission["memory"]}),
    )?;
    if evidence.len() > 16000 {
        let mut end = 16000;
        while !evidence.is_char_boundary(end) {
            end -= 1;
        }
        evidence.truncate(end);
        evidence.push_str("\n[Source evidence truncated; full remembered state remains private.]");
    }
    Ok(format!(
        "{prompt}\n\nAutomation source evidence (untrusted data):\n{evidence}"
    ))
}
// Automation evidence is private model source data. Keep the public submission
// at the configured prompt, including through queues and incoming-message review.
fn separate_automation_message(
    prepared: &mut crate::api::chat::PreparedMessage,
    prompt: &str,
    context: &str,
) {
    prepared.message["content"] = json!(prompt);
    prepared.message["context_content"] = json!(context);
    for (key, record) in &mut prepared.docs {
        if key.starts_with("manager_review:") {
            record["message"]["content"] = json!(prompt);
            record["message"]["context_content"] = json!(context);
        }
    }
}
// Admission uses the ordinary preparation path; occurrence receipts commit with
// the new message/run/review. A crashed package never grants executable authority.
async fn admit(app: &App, id: &str, gen: &str, admission: &Value) -> Result<Value> {
    let oid = admission["occurrence_id"]
        .as_str()
        .ok_or_else(|| anyhow!("Occurrence missing"))?;
    let snapshot = app.store.module_state(id).await?;
    let definition =
        &snapshot["state"]["definitions"][admission["automation_id"].as_str().unwrap_or("")];
    let occurrence = &snapshot["state"]["occurrences"][oid];
    if definition["status"] != "active"
        || definition["revision"] != admission["revision"]
        || occurrence["status"] != "queued"
        || occurrence["attempt"] != admission["attempt"]
    {
        return Err(anyhow!("Automation proposal withdrawn"));
    }
    let attempt = admission["attempt"].as_u64().unwrap_or(0);
    let receipt = format!("receipt:module:{id}:{}", digest(&json!([oid, attempt])));
    if let Some(saved) = app.store.get(&receipt).await? {
        return Ok(saved["result"].clone());
    }
    if generation(id)? != gen {
        return Err(anyhow!("Module generation changed"));
    }
    let prompt = admission["prompt"]
        .as_str()
        .filter(|v| !v.trim().is_empty() && v.len() <= 65536)
        .ok_or_else(|| anyhow!("Invalid automation prompt"))?;
    let agent = admission["agent_id"].as_str().unwrap_or("shared_default");
    let access = app
        .store
        .live_access(app.store.current_user_id(), agent)
        .await?;
    if execution_scope(app, agent, None, false).await.is_err() || !access.admitted() {
        return Err(anyhow!("Agent unavailable"));
    }
    let destination = &admission["destination"];
    let kind = destination["type"]
        .as_str()
        .or_else(|| destination["kind"].as_str())
        .or_else(|| destination.as_str())
        .unwrap_or("new_session");
    if !matches!(
        kind,
        "chat" | "existing_chat" | "existing_session" | "new_session" | "silent"
    ) {
        return Err(anyhow!("Destination unavailable"));
    }
    let sid = admission["session_id"]
        .as_str()
        .or_else(|| destination["session_id"].as_str());
    let new_session = matches!(kind, "new_session" | "silent");
    if !new_session && sid.is_none() {
        return Err(anyhow!("Destination session required"));
    }
    let request_id = digest(&json!([id, gen, oid, attempt]));
    let text = automation_prompt(prompt, admission)?;
    if text.len() > 65536 {
        return Err(anyhow!("Automation evidence exceeds admission limit"));
    }
    let body = json!({"client_message_id":request_id,"text":text,"agent_id":agent});
    let mut scoped = app.clone();
    scoped.store = app.store.for_agent(agent).await?;
    scoped.chat_actor = None;
    scoped.chat_access = None;
    let mut session = if new_session {
        let hash = Sha256::digest(request_id.as_bytes());
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&hash[..16]);
        Some(
            crate::api::sessions::prepare_session_id(
                &scoped,
                &body,
                Some(Uuid::from_bytes(bytes).to_string()),
            )
            .await
            .map_err(|_| anyhow!("Session admission rejected"))?,
        )
    } else {
        None
    };
    if kind == "silent" {
        if let Some(session) = session.as_mut() {
            session.value["module_silent"] = json!(true);
            session.result["module_silent"] = json!(true);
            for (key, record) in &mut session.docs {
                if key.starts_with("session:") {
                    record["module_silent"] = json!(true);
                }
            }
        }
    }
    let sid = session
        .as_ref()
        .map(|s| s.result["id"].as_str().unwrap())
        .or(sid)
        .unwrap();
    if !new_session {
        let stored = scoped
            .store
            .get(&key("session", sid))
            .await?
            .ok_or_else(|| anyhow!("Session unavailable"))?;
        if stored["agent_id"] != agent {
            return Err(anyhow!("Destination agent mismatch"));
        }
    }
    if !new_session && app.store.participation_state(sid).await?["active"] != true {
        return Err(anyhow!("Destination agent inactive"));
    }
    let mut prepared =
        crate::api::chat::prepare_message(&scoped, sid, &body, session.as_ref().map(|s| &s.value))
            .await
            .map_err(|_| anyhow!("Message admission rejected"))?;
    if prepared.kind == "human_message" {
        return Err(anyhow!("Destination agent inactive"));
    }
    separate_automation_message(&mut prepared, prompt, &text);
    let binding = json!({"id":id,"generation":gen,"occurrence_id":oid,"automation_id":admission["automation_id"],"definition_revision":admission["revision"],"causal_chain":admission["causal_chain"],"attempt":attempt});
    for (key, record) in &mut prepared.docs {
        if key.starts_with("run:") {
            record["module_binding"] = binding.clone();
            record["module_silent"] = json!(kind == "silent");
        }
        if key.starts_with("manager_review:") {
            record["run"]["module_binding"] = binding.clone();
            record["run"]["module_silent"] = json!(kind == "silent");
        }
    }
    prepared.message["module_silent"] = json!(kind == "silent");
    let mut result = prepared.result.clone();
    result["session_id"] = json!(sid);
    result["execution_id"] = prepared.message["run_id"].clone();
    result["attempt"] = json!(attempt);
    let mut docs = session.as_ref().map(|s| s.docs.clone()).unwrap_or_default();
    docs.extend(prepared.docs);
    docs.push((receipt, json!({"digest":digest(admission),"result":result})));
    if let Some(session) = &session {
        scoped
            .store
            .atomic_first_message(docs, sid, prepared.kind, &prepared.message, &session.result)
            .await?;
        scoped
            .store
            .register_shared_session(scoped.store.current_user_id(), sid, agent)
            .await?;
    } else {
        result = scoped
            .store
            .admit_shared_message(docs, sid, prepared.kind, &prepared.message)
            .await?;
    }
    result["attempt"] = json!(attempt);
    wake::notify(app).await;
    Ok(result)
}
pub(crate) async fn service(app: App) {
    let mut workers = tokio::task::JoinSet::new();
    for id in IDS {
        let scoped = app.clone();
        workers.spawn(async move {
            package_service(scoped, id).await;
        });
    }
    while workers.join_next().await.is_some() {}
}
async fn package_service(app: App, id: &'static str) {
    let mut jobs = tokio::task::JoinSet::new();
    let mut inflight = std::collections::HashSet::new();
    let mut seen = std::collections::HashSet::new();
    let mut last_started = HashMap::new();
    let mut wake_hints = crate::wake::subscribe();
    let mut last_probe = 0;
    let mut cursor = 0usize;
    loop {
        if app.stop.is_cancelled() {
            break;
        }
        if now().saturating_sub(last_probe) >= 5 {
            if let Ok(gen) = generation(id) {
                if let Ok(_lease) = package_lock(&app, id, false) {
                    let probe = request(
                        &app,
                        id,
                        "probe",
                        json!({}),
                        json!({}),
                        &gen,
                        true,
                        false,
                        &json!({}),
                    );
                    let installed = app
                        .store
                        .get(&format!("module_install:{id}"))
                        .await
                        .ok()
                        .flatten()
                        .is_some_and(|record| {
                            record["enabled"] == true && record["generation"] == gen
                        });
                    let healthy = installed
                        && tokio::time::timeout(Duration::from_secs(2), reduce(id, &probe))
                            .await
                            .is_ok_and(|result| result.is_ok());
                    let _ = mark_health(&app, id, &gen, healthy);
                }
            }
            last_probe = now();
        }
        // Omitted packages do not scan users. Separate package workers and two
        // bounded reducers prevent a slow webhook from delaying agent schedules.
        if available(id) && jobs.len() < 2 {
            if let Ok(mut users) = crate::agent_loop::user_apps(&app).await {
                let len = users.len();
                if len > 0 {
                    users.rotate_left(cursor % len);
                }
                for user in users {
                    if jobs.len() >= 2 {
                        break;
                    }
                    let uid = user.store.current_user_id().to_owned();
                    if last_started.get(&uid) == Some(&now()) {
                        continue;
                    }
                    if !inflight.insert(uid.clone()) {
                        continue;
                    }
                    let startup = !seen.contains(&uid);
                    cursor = cursor.wrapping_add(1);
                    last_started.insert(uid.clone(), now());
                    jobs.spawn(async move {
                        let success = tick_package(&user, id, startup).await.is_ok();
                        (uid, success)
                    });
                }
            }
        }
        tokio::select! {
            _=app.stop.cancelled()=>break,
            result=jobs.join_next(),if !jobs.is_empty()=>if let Some(Ok((uid,success)))=result{inflight.remove(&uid);if success{seen.insert(uid);}else{tracing::warn!(module_id=id,"Optional module work deferred");}},
            _=wake_hints.changed()=>{},
            _=tokio::time::sleep(Duration::from_secs(1))=>{},
        }
    }
    // Give already-reserved transport one bounded shutdown window. A killed
    // child leaves its durable sending lease for explicit uncertain recovery.
    let _ = tokio::time::timeout(Duration::from_secs(25), async {
        while jobs.join_next().await.is_some() {}
    })
    .await;
    jobs.abort_all();
}
// A fresh subscription starts at its trusted activation boundary. An older
// active consumer keeps its earlier boundary so retention gaps remain visible.
fn event_scan_cursor(id: &str, envelope: &Value, latest: i64) -> i64 {
    let persisted = if id == "automation" {
        envelope["event_cursor"].as_i64().unwrap_or(0)
    } else {
        envelope["state"]["cursor"].as_i64().unwrap_or(0)
    }
    .max(0);
    let collection = if id == "automation" {
        &envelope["state"]["definitions"]
    } else {
        &envelope["state"]["subscriptions"]
    };
    let oldest_active = collection
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, item)| {
            let active = if id == "automation" {
                item["status"] == "active" && item["trigger"]["kind"] == "event"
            } else {
                item["enabled"] == true
            };
            active.then(|| item["activation_seq"].as_i64().unwrap_or(0).max(0))
        })
        .min();
    persisted.max(oldest_active.unwrap_or(latest))
}

pub(crate) async fn tick_package(app: &App, id: &str, startup: bool) -> Result<()> {
    if !available(id) {
        return Ok(());
    }
    let gen = generation(id)?;
    let _package_lease = package_lock(app, id, false)?;
    let _lock = host_lock(app, id)?;
    let installed = app.store.get(&format!("module_install:{id}")).await?;
    if !installed.is_some_and(|v| v["enabled"] == true && v["generation"] == gen) {
        return Ok(());
    }
    if id == "outbound_notifications" && !administrator(app).await {
        return Ok(());
    }
    let initial = app.store.module_state(id).await?;
    let latest = app.store.notification_page(i64::MAX, 1).await?["latest_seq"]
        .as_i64()
        .unwrap_or(0);
    // A generation transition deactivates every retained subscription before tick.
    // It has no active consumer of pre-installation events.
    let cursor = if initial["generation"].is_string() && initial["generation"] != gen {
        let inactive = json!({"event_cursor":initial["event_cursor"],"state":{"cursor":initial["state"]["cursor"]}});
        event_scan_cursor(id, &inactive, latest)
    } else {
        event_scan_cursor(id, &initial, latest)
    };
    let page = if id == "outbound_notifications" {
        app.store.notification_page(cursor, 128).await?
    } else {
        app.store.module_events(cursor).await?
    };
    let mut settings = json!({});
    let mut allowed = json!({});
    if id == "automation" {
        let completed: Vec<_> = initial["admission_results"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (oid, result) in completed {
            let rid = result["execution_id"]
                .as_str()
                .or_else(|| result["run_id"].as_str())
                .or_else(|| result["review_id"].as_str());
            if let Some(rid) = rid {
                let observed_run = app.run(rid).await;
                let safe_retry = match &observed_run {
                    Ok(run) => app.store.module_retry_safe(run).await?,
                    Err(_) => false,
                };
                let completed_attempt = observed_run
                    .as_ref()
                    .ok()
                    .and_then(|run| run["module_binding"]["attempt"].as_u64())
                    .or_else(|| result["attempt"].as_u64())
                    .unwrap_or(0);
                let final_status = match observed_run {
                    Ok(run) if terminal(status(&run)) => Some(status(&run).to_owned()),
                    Ok(_) => None,
                    Err(_) => app
                        .store
                        .get(&format!("manager_review:{rid}"))
                        .await?
                        .and_then(|v| {
                            if matches!(
                                v["status"].as_str(),
                                Some("unacceptable" | "review_error" | "cancelled")
                            ) {
                                Some("failed".to_owned())
                            } else {
                                None
                            }
                        }),
                };
                if let Some(status) = final_status {
                    perform(
                        app,
                        id,
                        "complete",
                        json!({"occurrence_id":oid,"status":status,"attempt":completed_attempt,"effects_reconciled":safe_retry}),
                        None,
                        None,
                        true,
                        json!({}),
                    )
                    .await?;
                }
            }
        }
        let definitions = initial["state"]["definitions"].as_object();
        for definition in definitions.into_iter().flatten().map(|(_, d)| d) {
            let agent = definition["agent_id"].as_str().unwrap_or("shared_default");
            if settings.get(agent).is_none() {
                match execution_scope(app, agent, None, false).await {
                    Ok(scope) => {
                        settings[agent] = scope["settings"].clone();
                        allowed[agent] = json!(true);
                    }
                    Err(_) => {
                        settings[agent] = json!({});
                        allowed[agent] = json!(false);
                    }
                }
            }
        }
    }
    perform(app,id,"tick",json!({"events":page["events"],"cursor":page["next_seq"],"pruned_through":page["pruned_through"],"event_page":page,"agent_settings":settings,"agent_allowed":allowed}),None,None,true,json!({"startup":startup,"event_scan_cursor":cursor})).await?;
    if id == "outbound_notifications" {
        let reserve = perform(
            app,
            id,
            "reserve_delivery",
            json!({}),
            None,
            None,
            true,
            json!({}),
        )
        .await?;
        if !reserve["result"].is_null() {
            let _ = perform(
                app,
                id,
                "execute_delivery",
                reserve["result"].clone(),
                None,
                None,
                true,
                json!({}),
            )
            .await?;
        }
    }
    let mut envelope = app.store.module_state(id).await?;
    let expected = envelope["revision"].as_u64().unwrap_or(0);
    let mut remaining = Vec::new();
    if !envelope["admission_results"].is_object() {
        envelope["admission_results"] = json!({});
    }
    let mut acknowledged = Vec::new();
    let proposals = envelope["pending"].as_array().cloned().unwrap_or_default();
    for admission in proposals.iter().take(256) {
        match admit(app, id, &gen, admission).await {
            Ok(result) => {
                let oid = admission["occurrence_id"].as_str().unwrap_or("").to_owned();
                envelope["admission_results"][&oid] = result;
                acknowledged.push((oid, admission["attempt"].as_u64().unwrap_or(0)));
            }
            Err(error) if error.to_string() == "Automation proposal withdrawn" => {}
            Err(_) => remaining.push(admission.clone()),
        }
    }
    envelope["pending"] = json!(remaining);
    if id == "automation" {
        envelope["event_cursor"] = page["next_seq"].clone();
        if page["error"] == "cursor_expired" {
            envelope["event_gap"] = json!({"code":"cursor_expired","pruned_through":page["pruned_through"],"resumed_at":page["latest_seq"]});
        }
    }
    app.store.module_cas(id, expected, &envelope).await?;
    for (oid, attempt) in acknowledged {
        perform(
            app,
            id,
            "admitted",
            json!({"occurrence_id":oid,"attempt":attempt}),
            None,
            None,
            true,
            json!({}),
        )
        .await?;
    }
    Ok(())
}
fn copy_distribution(source: &FsPath, target: &FsPath) -> Result<()> {
    std::fs::create_dir(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let meta = entry.file_type()?;
        if meta.is_symlink() {
            return Err(anyhow!("Distribution symlink rejected"));
        }
        if meta.is_dir() {
            copy_distribution(&entry.path(), &target.join(entry.file_name()))?;
        } else if meta.is_file() {
            std::fs::copy(entry.path(), target.join(entry.file_name()))?;
        }
    }
    Ok(())
}
pub(crate) async fn lifecycle(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    if !administrator(&app).await {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Administrator required".into(),
        ));
    }
    Ok(Json(lifecycle_inner(app, id, body).await?))
}
// Enabling is limited to a package previously copied from the reviewed distribution.
fn installed_generation(record: &Value) -> Option<&str> {
    let generation = record["installed_generation"].as_str().or_else(|| {
        (record["enabled"] == true)
            .then(|| record["generation"].as_str())
            .flatten()
    })?;
    (generation.len() == 64 && generation.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(generation)
}
async fn lifecycle_inner(app: App, id: String, body: Value) -> Result<Value> {
    if !administrator(&app).await {
        return Err(anyhow!("Administrator required"));
    }
    let root = package_root(&id).ok_or_else(|| anyhow!("Unknown module"))?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
    let _lease = loop {
        match package_lock(&app, &id, true) {
            Ok(lease) => break lease,
            Err(error) => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(error);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    };
    let action = body["action"]
        .as_str()
        .ok_or_else(|| anyhow!("Action required"))?;
    match action {
        "install" => {
            if root.exists() {
                return Err(anyhow!("Package already exists"));
            }
            let distribution = PathBuf::from("/usr/local/lib/webagent/modules").join(&id);
            std::fs::create_dir_all(root.parent().unwrap())?;
            let staged = root.with_extension(format!("install-{}", uuid()));
            copy_distribution(&distribution, &staged)?;
            let mut manifest: Value =
                serde_json::from_slice(&std::fs::read(staged.join("package.json"))?)
                    .map_err(anyhow::Error::from)?;
            manifest["generation"] = json!(uuid());
            std::fs::write(
                staged.join("package.json"),
                serde_json::to_vec(&manifest).map_err(anyhow::Error::from)?,
            )?;
            std::fs::rename(staged, &root)?;
        }
        "disable" | "uninstall" => {
            let known = app
                .store
                .get(&format!("module_install:{id}"))
                .await?
                .unwrap_or(Value::Null);
            let pin = installed_generation(&known).map(str::to_owned);
            app.store
                .put(
                    &format!("module_install:{id}"),
                    &json!({"enabled":false,"generation":uuid(),"installed_generation":pin}),
                )
                .await?;
            if root.exists() {
                std::fs::write(root.join(".disabled"), b"disabled")?;
            }
            if action == "uninstall" && root.exists() {
                std::fs::remove_dir_all(&root)?;
            }
            wake::notify(&app).await;
            return Ok(json!({"available":false,"retained_data":true}));
        }
        "enable" => {
            let known = app
                .store
                .get(&format!("module_install:{id}"))
                .await?
                .unwrap_or(Value::Null);
            let pin = installed_generation(&known).ok_or_else(|| {
                anyhow!("Known installation required; reinstall the reviewed distribution")
            })?;
            if root.join(".disabled").exists() {
                std::fs::remove_file(root.join(".disabled"))?;
            }
            if !generation(&id).is_ok_and(|current| current == pin) {
                std::fs::write(root.join(".disabled"), b"changed package")?;
                return Err(anyhow!(
                    "Installed package changed; reinstall the reviewed distribution"
                ));
            }
            let file = root.join("package.json");
            let mut manifest: Value =
                serde_json::from_slice(&std::fs::read(&file)?).map_err(anyhow::Error::from)?;
            manifest["generation"] = json!(uuid());
            std::fs::write(
                file,
                serde_json::to_vec(&manifest).map_err(anyhow::Error::from)?,
            )?;
        }
        _ => return Err(anyhow!("Unknown module lifecycle action")),
    }
    let gen = generation(&id)?;
    let probe = request(
        &app,
        &id,
        "probe",
        json!({}),
        json!({}),
        &gen,
        true,
        false,
        &json!({}),
    );
    let healthy = tokio::time::timeout(Duration::from_secs(2), reduce(&id, &probe))
        .await
        .is_ok_and(|result| result.is_ok());
    if !healthy {
        mark_health(&app, &id, &gen, false)?;
        return Err(anyhow!("Module health probe failed"));
    }
    app.store
        .put(
            &format!("module_install:{id}"),
            &json!({"enabled":true,"generation":gen,"installed_generation":gen}),
        )
        .await?;
    mark_health(&app, &id, &gen, true)?;
    // Register installation defaults only; existing Deny and tenant grants stay authoritative.
    crate::migrate_tool_policy_defaults(&app.store).await?;
    wake::notify(&app).await;
    Ok(json!({"available":available(&id),"retained_data":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_reviewed_registered_packages_can_be_reenabled() {
        let pin = "a".repeat(64);
        assert!(installed_generation(&json!({})).is_none());
        assert!(installed_generation(&json!({"enabled":false,"generation":"revoked"})).is_none());
        assert_eq!(
            installed_generation(&json!({"enabled":true,"generation":pin})),
            Some(pin.as_str())
        );
        assert_eq!(
            installed_generation(
                &json!({"enabled":false,"generation":"revoked","installed_generation":pin})
            ),
            Some(pin.as_str())
        );
        assert!(
            installed_generation(&json!({"enabled":true,"generation":"arbitrary-directory"}))
                .is_none()
        );
    }
    #[test]
    fn event_scan_cursor_preserves_old_consumers_and_persisted_progress() {
        let mut fresh = json!({"event_cursor":0,"state":{"definitions":{
            "fresh":{"status":"active","trigger":{"kind":"event"},"activation_seq":600}
        }}});
        assert_eq!(event_scan_cursor("automation", &fresh, 601), 600);
        fresh["state"]["definitions"]["old"] =
            json!({"status":"active","trigger":{"kind":"event"},"activation_seq":100});
        assert_eq!(
            event_scan_cursor("automation", &fresh, 601),
            100,
            "an older active consumer retains gap detection"
        );
        fresh["event_cursor"] = json!(550);
        assert_eq!(
            event_scan_cursor("automation", &fresh, 601),
            550,
            "an already consumed range cannot rewind"
        );
        fresh["event_cursor"] = json!(700);
        assert_eq!(
            event_scan_cursor("automation", &fresh, 601),
            700,
            "persisted progress is monotonic"
        );
        fresh["state"]["definitions"]["old"]["activation_seq"] = Value::Null;
        fresh["event_cursor"] = json!(0);
        assert_eq!(
            event_scan_cursor("automation", &fresh, 601),
            0,
            "missing active provenance fails conservatively"
        );
    }
    #[test]
    fn event_scan_cursor_ignores_inactive_and_non_event_definitions() {
        let automation = json!({"event_cursor":100,"state":{"definitions":{
            "paused":{"status":"paused","trigger":{"kind":"event"},"activation_seq":20},
            "clock":{"status":"active","trigger":{"kind":"cron"},"activation_seq":10},
            "once":{"status":"active","trigger":{"kind":"once"}}
        }}});
        assert_eq!(event_scan_cursor("automation", &automation, 601), 601);
        assert_eq!(event_scan_cursor("automation", &json!({}), 601), 601);
        let outbound = json!({"state":{"cursor":100,"subscriptions":{
            "disabled":{"enabled":false,"activation_seq":20},
            "fresh":{"enabled":true,"activation_seq":600}
        }}});
        assert_eq!(
            event_scan_cursor("outbound_notifications", &outbound, 601),
            600
        );
        assert_eq!(
            event_scan_cursor(
                "outbound_notifications",
                &json!({"state":{"cursor":700}}),
                601
            ),
            700
        );
    }
    #[tokio::test]
    async fn fresh_activation_above_retention_floor_receives_future_events() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        sqlx::query("INSERT INTO notification_outbox(seq,event_id,type,session_id,run_id,agent_id,actor,payload,created_at,source_user,source_seq,retained_at) VALUES(601,'fresh-event','run.completed','fixture-session',NULL,'shared_default','runtime','{}',1,'admin',1,1)")
            .execute(&store.app_pool).await.unwrap();
        sqlx::query("UPDATE notification_retention SET pruned_through=500 WHERE singleton=1")
            .execute(&store.app_pool)
            .await
            .unwrap();
        assert_eq!(
            store.notification_page(0, 128).await.unwrap()["error"],
            "cursor_expired"
        );
        let automation = json!({"event_cursor":0,"state":{"definitions":{
            "fresh":{"status":"active","trigger":{"kind":"event"},"activation_seq":600}
        }}});
        let page = store
            .module_events(event_scan_cursor("automation", &automation, 601))
            .await
            .unwrap();
        assert!(page["error"].is_null());
        assert_eq!(page["events"][0]["event_id"], "fresh-event");
        assert_eq!(page["next_seq"], 601);
        let outbound = json!({"state":{"cursor":0,"subscriptions":{
            "fresh":{"enabled":true,"activation_seq":600}
        }}});
        let page = store
            .notification_page(
                event_scan_cursor("outbound_notifications", &outbound, 601),
                128,
            )
            .await
            .unwrap();
        assert!(page["error"].is_null());
        assert_eq!(page["events"][0]["event_id"], "fresh-event");
        let old = json!({"event_cursor":0,"state":{"definitions":{
            "old":{"status":"active","trigger":{"kind":"event"},"activation_seq":100}
        }}});
        let page = store
            .module_events(event_scan_cursor("automation", &old, 601))
            .await
            .unwrap();
        assert_eq!(
            page["error"], "cursor_expired",
            "retention loss for an existing consumer must stay visible"
        );
    }
    #[test]
    fn terminal_observation_without_completion_ack_retains_tracking() {
        let mut envelope =
            json!({"admission_results":{"occurrence":{"execution_id":"run","status":"completed"}}});
        apply_completion_ack(&mut envelope, None);
        assert!(envelope["admission_results"].get("occurrence").is_some());
        apply_completion_ack(&mut envelope, Some("occurrence"));
        assert!(envelope["admission_results"].get("occurrence").is_none());
    }
    #[test]
    fn health_withdraws_stale_changed_and_failed_packages() {
        let health = json!({"generation":"current","healthy":true,"checked_at":100});
        assert!(health_current(&health, "current", 115));
        assert!(!health_current(&health, "current", 116));
        assert!(!health_current(&health, "changed", 100));
        assert!(!health_current(
            &json!({"generation":"current","healthy":false,"checked_at":100}),
            "current",
            100
        ));
    }
    #[tokio::test]
    async fn absent_package_fences_module_work_without_affecting_chat() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        assert!(binding_valid(&app, &json!({"id":"ordinary"})).await);
        assert!(
            !binding_valid(
                &app,
                &json!({"module_binding":{"id":"missing","generation":"old"}})
            )
            .await
        );
    }
    #[tokio::test]
    async fn public_api_refuses_internal_receipts() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        for action in [
            "tick",
            "admitted",
            "complete",
            "deactivate",
            "reserve_delivery",
            "execute_delivery",
        ] {
            let error = call_api(
                State(app.clone()),
                Path("automation".into()),
                Json(json!({"action":action,"client_request_id":"internal","args":{}})),
            )
            .await
            .unwrap_err();
            assert_eq!(error.0, StatusCode::BAD_REQUEST);
        }
        assert_eq!(
            app.store.module_state("automation").await.unwrap()["revision"],
            0
        );
    }
    #[tokio::test]
    async fn lifecycle_lease_excludes_effectful_commands() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        let (_fixture, mut app) = crate::runtime_tests::fixture().await;
        app.store = store;
        let read = package_lock(&app, "automation", false).unwrap();
        assert!(package_lock(&app, "automation", true).is_err());
        drop(read);
        let write = package_lock(&app, "automation", true).unwrap();
        assert!(package_lock(&app, "automation", false).is_err());
        drop(write);
    }
}

#[cfg(test)]
mod evidence_bound_tests {
    use super::*;
    #[test]
    fn full_memory_does_not_block_next_admission_or_break_utf8() {
        let memory = "🦀".repeat(16000);
        let text = automation_prompt(
            "Perform the task once",
            &json!({"memory":memory,"event":{"type":"run.completed"}}),
        )
        .unwrap();
        assert!(text.len() < 65536);
        assert!(text.starts_with("Perform the task once"));
        assert!(text.contains("untrusted data"));
        assert!(
            text.ends_with("[Source evidence truncated; full remembered state remains private.]")
        );
        assert_eq!(
            automation_prompt("Plain task", &json!({})).unwrap(),
            "Plain task"
        );
    }
}

#[cfg(test)]
mod automation_evidence_privacy_tests {
    use super::*;
    const SECRET: &str = "PRIVATE_AUTOMATION_MEMORY_SENTINEL";
    const PROMPT: &str = "Check the event and report the task result";
    fn model_text() -> String {
        automation_prompt(
            PROMPT,
            &json!({"memory":{"private_note":SECRET},"event":{"type":"run.completed"}}),
        )
        .unwrap()
    }
    async fn assert_public_private_split(app: &App, sid: &str) {
        let chat = app.store.chat_snapshot(sid).await.unwrap().0;
        assert!(!json!(chat).to_string().contains(SECRET));
        assert!(json!(chat).to_string().contains(PROMPT));
        let stream = app.store.chat_events(sid, 0).await.unwrap();
        assert!(!json!(stream).to_string().contains(SECRET));
        let search = app
            .store
            .search_private_sessions_scoped(SECRET, "active", 20, None, None)
            .await
            .unwrap();
        assert_eq!(search["results"].as_array().unwrap().len(), 0);
    }
    async fn prepare(app: &App, sid: &str) -> crate::api::chat::PreparedMessage {
        let context = model_text();
        let mut prepared = crate::api::chat::prepare_message(
            app,
            sid,
            &json!({"client_message_id":uuid(),"text":context}),
            None,
        )
        .await
        .unwrap();
        separate_automation_message(&mut prepared, PROMPT, &context);
        prepared
    }
    #[tokio::test]
    async fn first_admission_keeps_source_out_of_public_chat_and_search() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let body = json!({"client_message_id":uuid(),"text":model_text(),"agent_id":"shared_default","title":"Evidence test"});
        let session = crate::api::sessions::prepare_session_id(&app, &body, None)
            .await
            .unwrap();
        let sid = session.result["id"].as_str().unwrap();
        let mut prepared =
            crate::api::chat::prepare_message(&app, sid, &body, Some(&session.value))
                .await
                .unwrap();
        separate_automation_message(&mut prepared, PROMPT, &model_text());
        let mut docs = session.docs;
        docs.extend(prepared.docs);
        app.store
            .atomic_first_message(docs, sid, prepared.kind, &prepared.message, &session.result)
            .await
            .unwrap();
        assert_public_private_split(&app, sid).await;
        assert!(json!(app.store.context(sid).await.unwrap())
            .to_string()
            .contains(SECRET));
    }
    #[tokio::test]
    async fn immediate_and_queued_admission_preserve_private_context() {
        for queued in [false, true] {
            let (_root, app) = crate::runtime_tests::fixture().await;
            let sid = crate::runtime_tests::conversation(&app).await;
            let blocker = if queued {
                Some(crate::runtime_tests::queued(&app, &sid).await)
            } else {
                None
            };
            let prepared = prepare(&app, &sid).await;
            let result = app
                .store
                .admit_shared_message(prepared.docs, &sid, prepared.kind, &prepared.message)
                .await
                .unwrap();
            assert_public_private_split(&app, &sid).await;
            if let Some(blocker) = blocker {
                assert_eq!(result["queued"], true);
                assert!(
                    !json!(app.store.context(&sid).await.unwrap())
                        .to_string()
                        .contains(SECRET),
                    "queued evidence is not model history before admission"
                );
                let mut run = app.run(&blocker).await.unwrap();
                run["status"] = json!("completed");
                app.store.put(&key("run", &blocker), &run).await.unwrap();
                app.store.drain_message_queues().await.unwrap();
            }
            assert_public_private_split(&app, &sid).await;
            assert!(json!(app.store.context(&sid).await.unwrap())
                .to_string()
                .contains(SECRET));
        }
    }
    #[tokio::test]
    async fn incoming_review_preserves_private_evidence_through_acceptance() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let agent = app.store.for_agent("shared_default").await.unwrap();
        let raw: Vec<u8> =
            sqlx::query_scalar("SELECT metadata FROM agents WHERE id='shared_default'")
                .fetch_one(&agent.agent_pool)
                .await
                .unwrap();
        let mut metadata = agent.unseal(&raw).unwrap();
        metadata["manager_incoming_message_revision"] = json!(1);
        metadata["manager_incoming_message"] =
            json!({"enabled":true,"criteria":"Review incoming automation tasks."});
        sqlx::query("UPDATE agents SET metadata=? WHERE id='shared_default'")
            .bind(agent.seal(&metadata).unwrap())
            .execute(&agent.agent_pool)
            .await
            .unwrap();
        let prepared = prepare(&app, &sid).await;
        assert_eq!(prepared.kind, "manager_submission");
        let rid = prepared.message["run_id"].as_str().unwrap().to_owned();
        let review = prepared
            .docs
            .iter()
            .find(|(name, _)| name.starts_with("manager_review:"))
            .unwrap()
            .1
            .clone();
        assert_eq!(review["message"]["content"], PROMPT);
        assert!(review["message"]["context_content"]
            .as_str()
            .unwrap()
            .contains(SECRET));
        app.store
            .admit_shared_message(prepared.docs, &sid, prepared.kind, &prepared.message)
            .await
            .unwrap();
        assert_public_private_split(&app, &sid).await;
        assert!(!json!(app.store.context(&sid).await.unwrap())
            .to_string()
            .contains(SECRET));
        let claimed = app.store.claim_manager_review(&rid).await.unwrap().unwrap();
        assert!(app
            .store
            .finish_manager_review(
                &claimed,
                "acceptable",
                Some(crate::manager::Verdict::Acceptable)
            )
            .await
            .unwrap());
        assert_public_private_split(&app, &sid).await;
        assert!(json!(app.store.context(&sid).await.unwrap())
            .to_string()
            .contains(SECRET));
    }
}
