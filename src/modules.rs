//! Optional administrator-installed packages. The host owns storage and admission;
//! package reducers never receive encryption keys, raw databases or approvals.
use crate::*;
use axum::Extension;
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
// @code-index {"version":1,"feature":"automation-drop-in","aliases":["standalone automation","automation ability plugin"],"purpose":"Trusted Automation lifecycle, pinned reducer execution and current permissions. Config: plugins/abilities/Core/automation/automation.json -> /config/settings; settings flow through host execution_scope and never grant authority.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"plugins/abilities/Core/automation/automation.json","pointers":["/config/settings"]}],"opportunities":[]}}
/// Automation has one fixed native ability bridge; legacy roots cannot restore a removed package.
fn automation_ability_host(id: &str) -> bool {
    id == "automation"
}
/// Pin one broker publication before resolving the Automation ability's assets.
fn automation_ability_root() -> PathBuf {
    crate::live_assets::published_root()
        .map(|root| root.join("plugins/abilities"))
        .unwrap_or_else(|| {
            PathBuf::from(
                std::env::var("V3_ABILITIES_ROOT")
                    .unwrap_or_else(|_| "/opt/v3/plugins/abilities".into()),
            )
        })
        .join("Core/automation")
}
/// Keep a bounded content-free activation marker outside immutable package source.
fn automation_activation_path(root: &FsPath) -> PathBuf {
    root.join("modules/activation-automation.json")
}
/// Read only host-written activation state; absence never installs or enables a package.
fn automation_activation(root: &FsPath) -> Result<Value> {
    let path = automation_activation_path(root);
    let meta = std::fs::symlink_metadata(&path)?;
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || meta.len() > 4096
        || meta.permissions().mode() & 0o077 != 0
    {
        return Err(anyhow!("Unsafe Automation activation marker"));
    }
    let value: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    if !value["installed"].is_boolean()
        || !value["enabled"].is_boolean()
        || value["generation"]
            .as_str()
            .is_none_or(|v| v.len() != 64 || !v.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(anyhow!("Invalid Automation activation marker"));
    }
    Ok(value)
}
/// Atomically persist host activation; write disabled first when changing an epoch.
fn save_automation_activation(
    root: &FsPath,
    installed: bool,
    enabled: bool,
    generation: &str,
) -> Result<()> {
    if generation.len() != 64 || !generation.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(anyhow!("Invalid Automation activation generation"));
    }
    let path = automation_activation_path(root);
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("Missing activation directory"))?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&serde_json::to_vec(
        &json!({"installed":installed,"enabled":enabled,"generation":generation}),
    )?)?;
    file.as_file().sync_all()?;
    file.persist(&path).map_err(anyhow::Error::from)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
/// Explicit installed=false wins over retained legacy metadata after uninstall.
fn automation_installed(record: &Value, legacy_exists: bool) -> bool {
    record["installed"]
        .as_bool()
        .unwrap_or_else(|| legacy_exists && installed_generation(record).is_some())
}
/// Retain the old copied package as migration/rollback material without executing it.
fn legacy_automation_exists(root: &FsPath) -> bool {
    std::fs::symlink_metadata(root.join("plugins/services/automation/package.json"))
        .is_ok_and(|meta| meta.is_file() && !meta.file_type().is_symlink())
}
/// Read the trusted descriptor's activation generation without confusing it with a build version.
fn manifest_generation(id: &str, path: &FsPath, descriptor: &Value) -> Result<String> {
    if automation_ability_host(id) {
        return descriptor["_host_generation"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("Missing Automation activation binding"));
    }
    let meta = std::fs::metadata(path)?;
    Ok(digest(&json!([
        descriptor["generation"],
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec()
    ])))
}
/// Bind a reducer invocation to its exact code/assets while compatible updates keep the activation epoch.
fn manifest_implementation(id: &str, path: &FsPath, descriptor: &Value) -> Result<String> {
    if automation_ability_host(id) {
        return descriptor["_host_implementation"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("Missing Automation implementation binding"));
    }
    manifest_generation(id, path, descriptor)
}
/// Require the native protocol acknowledgement before granting implementation-bound health.
fn automation_probe_ready(reply: &Value) -> bool {
    reply["result"]["protocol"] == 1
        && reply["result"]["package"] == "automation"
        && reply["result"]["ready"] == true
}
/// Probe one current implementation and publish health only for that same activation and code version.
async fn probe_automation(app: &App, generation: &str) -> Result<bool> {
    let (path, descriptor) = manifest("automation")?;
    let implementation = manifest_implementation("automation", &path, &descriptor)?;
    if manifest_generation("automation", &path, &descriptor)? != generation {
        return Err(anyhow!("Automation activation changed before probe"));
    }
    let frame = request(
        app,
        "automation",
        "probe",
        json!({}),
        json!({}),
        generation,
        true,
        false,
        &json!({}),
    );
    let healthy = tokio::time::timeout(Duration::from_secs(2), reduce("automation", &frame))
        .await
        .is_ok_and(|result| result.is_ok_and(|reply| automation_probe_ready(&reply)));
    let (current_path, current) = manifest("automation")?;
    if manifest_generation("automation", &current_path, &current)? != generation
        || manifest_implementation("automation", &current_path, &current)? != implementation
    {
        return Err(anyhow!("Automation changed during health probe"));
    }
    write_health(
        app,
        "automation",
        generation,
        Some(&implementation),
        healthy,
    )?;
    Ok(healthy)
}
/// Enable only an explicit administrator-installed package; failed transitions stay unavailable.
/// Reads/Writes: global module_install:automation and the host activation marker. Reducers never write either.
async fn automation_lifecycle(app: &App, action: &str) -> Result<Value> {
    let known = app
        .store
        .get("module_install:automation")
        .await?
        .unwrap_or(Value::Null);
    let installed = automation_installed(&known, legacy_automation_exists(&app.store.root));
    let epoch = digest(&json!(uuid()));
    match action {
        "disable" | "uninstall" => {
            let retain = installed && action != "uninstall";
            save_automation_activation(&app.store.root, retain, false, &epoch)?;
            app.store.put("module_install:automation",&json!({"installed":retain,"enabled":false,"generation":epoch,"installed_generation":if retain{Some(epoch.as_str())}else{None},"source":"ability"})).await?;
            write_health(app, "automation", &epoch, None, false)?;
            wake::notify(app).await;
            return Ok(json!({"available":false,"retained_data":true,"installed":retain}));
        }
        "install" if installed => return Err(anyhow!("Package already exists")),
        "enable" if !installed => {
            return Err(anyhow!(
                "Known installation required; install the reviewed ability"
            ))
        }
        "install" | "enable" => {}
        _ => return Err(anyhow!("Unknown module lifecycle action")),
    }
    // Resolve a reviewed artifact before changing controls, with no legacy binary fallback.
    crate::ability_plugins::automation_service(&automation_ability_root())?;
    save_automation_activation(&app.store.root, true, false, &epoch)?;
    write_health(app, "automation", &epoch, None, false)?;
    app.store.put("module_install:automation",&json!({"installed":true,"enabled":true,"generation":epoch,"installed_generation":epoch,"source":"ability"})).await?;
    save_automation_activation(&app.store.root, true, true, &epoch)?;
    let healthy = probe_automation(app, &epoch).await;
    if !matches!(healthy, Ok(true)) {
        save_automation_activation(&app.store.root, true, false, &epoch)?;
        app.store.put("module_install:automation",&json!({"installed":true,"enabled":false,"generation":epoch,"installed_generation":epoch,"source":"ability"})).await?;
        return Err(anyhow!("Module health probe failed"));
    }
    crate::migrate_tool_policy_defaults(&app.store).await?;
    wake::notify(app).await;
    Ok(json!({"available":available("automation"),"retained_data":true,"installed":true}))
}

pub(crate) fn package_root(id: &str) -> Option<PathBuf> {
    if automation_ability_host(id) {
        return Some(automation_ability_root().join("service"));
    }
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
    if automation_ability_host(id) {
        let state =
            PathBuf::from(std::env::var("V3_STATE_ROOT").unwrap_or_else(|_| "/state".into()));
        let activation = automation_activation(&state)?;
        if activation["installed"] != true || activation["enabled"] != true {
            return Err(anyhow!("Module disabled"));
        }
        // Disabled installations never resolve or hash executable artifacts.
        let ability = root
            .parent()
            .ok_or_else(|| anyhow!("Missing Automation ability root"))?;
        let mut service = crate::ability_plugins::automation_service(ability)?;
        if service.root != root {
            return Err(anyhow!("Automation asset binding changed"));
        }
        service.descriptor["_host_generation"] = activation["generation"].clone();
        service.descriptor["_host_implementation"] = json!(service.implementation);
        return Ok((service.executable, service.descriptor));
    }
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
/// Bind native health to current implementation while retaining legacy probe semantics.
fn health_binding_current(
    health: &Value,
    generation: &str,
    implementation: Option<&str>,
    at: u64,
) -> bool {
    health_current(health, generation, at)
        && implementation.is_none_or(|value| health["implementation"] == value)
}
/// Resolve current code and activation together; health from an older implementation cannot authorize tools.
pub(crate) fn available(id: &str) -> bool {
    let Ok((path, descriptor)) = manifest(id) else {
        return false;
    };
    let Ok(gen) = manifest_generation(id, &path, &descriptor) else {
        return false;
    };
    let implementation = if automation_ability_host(id) {
        match manifest_implementation(id, &path, &descriptor) {
            Ok(value) => Some(value),
            Err(_) => return false,
        }
    } else {
        None
    };
    let root = PathBuf::from(std::env::var("V3_STATE_ROOT").unwrap_or_else(|_| "/state".into()));
    std::fs::read(health_path(&root, id))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|health| {
            health_binding_current(&health, &gen, implementation.as_deref(), now())
        })
}
/// Legacy package probes keep their existing activation-bound health format.
fn mark_health(app: &App, id: &str, gen: &str, healthy: bool) -> Result<()> {
    write_health(app, id, gen, None, healthy)
}
/// Publish content-free probe evidence for one activation and optional native implementation binding.
fn write_health(
    app: &App,
    id: &str,
    gen: &str,
    implementation: Option<&str>,
    healthy: bool,
) -> Result<()> {
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
        &json!({"generation":gen,"implementation":implementation,"healthy":healthy,"checked_at":now()}),
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
    let (path, descriptor) = manifest(id)?;
    manifest_generation(id, &path, &descriptor)
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
/// Install the app-owned pure-process sandbox before replaceable Automation code executes.
/// The outbound transport keeps its existing network-capable path; packages cannot select this boundary.
fn reducer_process(id: &str, executable: &FsPath) -> Result<tokio::process::Command> {
    let mut command = tokio::process::Command::new(executable);
    if automation_ability_host(id) {
        let sandbox = crate::engine_sandbox::prepare(executable)?;
        // Preparation allocates and holds path rules in the parent. The pre-exec
        // closure only applies the existing host Landlock/seccomp/descriptor fence.
        unsafe {
            command.pre_exec(move || sandbox.apply());
        }
        command.current_dir("/");
    }
    command.env_clear().env("PATH", "/usr/bin:/bin");
    Ok(command)
}
async fn reduce(id: &str, request: &Value) -> Result<Value> {
    let (executable, descriptor) = manifest(id)?;
    let pinned = manifest_generation(id, &executable, &descriptor)?;
    let implementation = manifest_implementation(id, &executable, &descriptor)?;
    if automation_ability_host(id) && request["context"]["generation"] != pinned {
        return Err(anyhow!("Module changed before request"));
    }
    let mut child = reducer_process(id, &executable)?
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
    let mut result = tokio::time::timeout(Duration::from_secs(20), async move {
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
    let (current_path, current) = manifest(id)?;
    // Compatible replacement retains the admitted immutable executable. Removal
    // or an activation change still withholds its result before namespace CAS.
    if manifest_generation(id, &current_path, &current)? != pinned {
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
    result["_host_implementation"] = json!(implementation);
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
/// Private schedule namespaces are available only after host-side Notes binding
/// checks. HTTP callers cannot manufacture this trusted execution-scope marker.
fn main_notes_schedule_guard(op:&str,args:&Value,scope:&Value)->Result<()> {
    if matches!(op,"schedule_task"|"remind_me"|"event_subscribe") {
        let id=args["request_id"].as_str().unwrap_or("");
        if id.starts_with("main-notes-review-") || id.starts_with("main-notes-action-")&&scope["main_notes_binding_verified"]!=true {
            return Err(anyhow!("Main Notes schedules require coordinator binding"));
        }
    }
    if op=="update_automation" {
        let id=args["id"].as_str().unwrap_or("");
        if id.starts_with("main-notes-review-") || id.starts_with("main-notes-action-")&&scope["main_notes_binding_verified"]!=true {
            return Err(anyhow!("Main Notes schedules require coordinator binding"));
        }
    }
    Ok(())
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
    if id=="automation"&&!internal{main_notes_schedule_guard(op,&args,&scope)?;}
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
    let mut public = json!({"result":result["result"],"revision":if unchanged{revision}else{revision+1},"available":true});
    if automation_ability_host(id) {
        public["implementation_generation"] = result["_host_implementation"].clone();
    }
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
        if target.starts_with("main-notes-action-")||target.starts_with("main-notes-review-") {
            let notes=scoped.store.for_agent(&agent).await?.main_chat_state().await?;
            if notes["session_id"]!=sid {return Err(anyhow!("Only Main Chat manages Main Notes schedules"));}
        }
        let state = scoped.store.module_state(id).await?;
        if let Some(owner) = state["state"]["definitions"][target]["agent_id"].as_str() {
            if owner != agent {
                return Err(anyhow!("Automation belongs to another agent"));
            }
        }
    }
    if let Some(binding)=args.get("main_notes_action").filter(|v|!v.is_null()) {
        if !matches!(name,"schedule_task"|"remind_me"|"update_automation") {return Err(anyhow!("Main Notes action binding requires a schedule operation"));}
        let notes=scoped.store.for_agent(&agent).await?.main_chat_state().await?;
        if notes["settings"]["enabled"]!=true||notes["session_id"]!=sid {return Err(anyhow!("Only Main Chat coordinates Main Notes schedules"));}
        let topic=binding["topic_id"].as_str().ok_or_else(||anyhow!("Topic ID required"))?.to_owned();
        let entry=binding["entry_id"].as_str().ok_or_else(||anyhow!("Entry ID required"))?.to_owned();
        let action=&notes["topics"][&topic]["entries"][&entry];
        let action_id=format!("main-notes-action-{}",&digest(&json!([agent,topic,entry]))[..40]);
        let updating=name=="update_automation";
        if updating&&args["id"]!=action_id {return Err(anyhow!("Schedule identity does not match the Main Notes action"));}
        let existing=if updating {scoped.store.module_state("automation").await?["state"]["definitions"][&action_id].clone()}else{Value::Null};
        let trigger=if updating&&args["trigger"].is_null(){
            if action["timing"]["kind"]=="delay"&&existing["trigger"]["kind"]=="once"&&existing["main_notes_intent"]==crate::main_chat::action_intent(&notes["topics"][&topic],action) {
                action["timing"].clone()
            }else{existing["trigger"].clone()}
        }else{args["trigger"].clone()};
        let prompt=if updating&&args["prompt"].is_null(){existing["prompt"].clone()}else{args["prompt"].clone()};
        if action.is_null()||action["timing"].is_null()||action["timing"]!=trigger||action["text"]!=prompt {
            return Err(anyhow!("Schedule prompt and timing must match the current Main Notes action"));
        }
        if matches!(action["state"].as_str(),Some("done"|"cancelled")) {return Err(anyhow!("Action is already terminal"));}
        if name!="remind_me" {
            let destination=if updating&&args["destination"].is_null(){&existing["destination"]}else{&args["destination"]};
            let kind=destination["type"].as_str().or_else(||destination["kind"].as_str()).unwrap_or("");
            if !matches!(kind,"existing_chat"|"existing_session"|"chat")||destination["session_id"]!=sid {
                return Err(anyhow!("Main Notes schedules must explicitly target Main Chat"));
            }
        }
        let intent=crate::main_chat::action_intent(&notes["topics"][&topic],action);
        if !updating {args["request_id"]=json!(action_id);}
        args["main_notes_intent"]=json!(intent);
    } else if name=="update_automation"&&args["id"].as_str().is_some_and(|v|v.starts_with("main-notes-action-")) {
        return Err(anyhow!("Main Notes updates require their current action binding"));
    }
    let mut scope = execution_scope(&scoped, &agent, Some(sid), false).await?;
    scope["main_notes_binding_verified"]=json!(args.get("main_notes_action").is_some_and(|v|!v.is_null()));
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
pub(crate) async fn admin_list(Extension(app): Extension<App>) -> Api {
    if !administrator(&app).await {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Administrator required".into(),
        ));
    }
    let mut modules = Vec::new();
    for id in IDS {
        let record = app
            .store
            .get(&format!("module_install:{id}"))
            .await?
            .unwrap_or(Value::Null);
        let installed = if automation_ability_host(id) {
            automation_installed(&record, legacy_automation_exists(&app.store.root))
        } else {
            package_root(id).is_some_and(|root| root.join("package.json").is_file())
        };
        let enabled = record["enabled"] == true;
        modules.push(json!({"id":id,"label":if id=="automation"{"Automation"}else{"Outbound Notifications"},"installed":installed,"available":available(id),"enabled":enabled,"protocol":1}));
    }
    Ok(Json(json!({"modules":modules})))
}
pub(crate) async fn status_api(Extension(app): Extension<App>, Path(id): Path<String>) -> Api {
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
    let installed = if automation_ability_host(&id) {
        let record = app
            .store
            .get("module_install:automation")
            .await?
            .unwrap_or(Value::Null);
        automation_installed(&record, legacy_automation_exists(&app.store.root))
    } else {
        package_root(&id).is_some_and(|root| root.join("package.json").is_file())
    };
    Ok(Json(
        json!({"available":available(&id),"id":id,"revision":state["revision"],"protocol":1,"installed":installed,"healthy":available(&id),"event_gap":state["event_gap"]}),
    ))
}
pub(crate) async fn call_api(
    Extension(app): Extension<App>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    if id == "outbound_notifications" && !administrator(&app).await {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Administrator required".into(),
        ));
    }
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
    if binding["id"]=="automation" {
        let aid=binding["automation_id"].as_str().unwrap_or("");
        if aid.starts_with("main-notes-review-")||aid.starts_with("main-notes-action-") {
            let Some(agent)=run["agent_id"].as_str() else{return false};
            let Ok(store)=app.store.for_agent(agent).await else{return false};
            let Ok(notes)=store.main_chat_state().await else{return false};
            if notes["session_id"]!=run["session_id"]{return false;}
            if notes["settings"]["enabled"]!=true {return false;}
            if aid.starts_with("main-notes-action-") {
                let definition=app.store.module_state("automation").await.ok()
                    .map(|s|s["state"]["definitions"][aid].clone()).unwrap_or(Value::Null);
                let current=crate::main_chat::scheduled_entry_current(&notes,aid,&definition);
                // A completed action may publish its final model reply. A further
                // tool dispatch has a pending call and still fails this live fence.
                if !current && !(run["phase"]=="model"&&run["pending"].is_null()) {return false;}
            }
        }
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
    let aid=admission["automation_id"].as_str().unwrap_or("");
    if aid.starts_with("main-notes-review-")||aid.starts_with("main-notes-action-") {
        let agent=definition["agent_id"].as_str().ok_or_else(||anyhow!("Missing Notes agent"))?;
        let notes=app.store.for_agent(agent).await?.main_chat_state().await?;
        if definition["destination"]["session_id"]!=notes["session_id"]{return Err(anyhow!("Main Chat destination changed"));}
        if notes["settings"]["enabled"]!=true {return Err(anyhow!("Main Chat is disabled"));}
        if aid.starts_with("main-notes-action-")&&!crate::main_chat::scheduled_entry_current(&notes,aid,&definition) {
            return Err(anyhow!("Automation proposal withdrawn"));
        }
    }
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
    let mut last_dispatched_wake = HashMap::new();
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
                    let installed = app
                        .store
                        .get(&format!("module_install:{id}"))
                        .await
                        .ok()
                        .flatten()
                        .is_some_and(|record| {
                            record["enabled"] == true && record["generation"] == gen
                        });
                    if automation_ability_host(id) {
                        if !installed || !probe_automation(&app, &gen).await.unwrap_or(false) {
                            let _ = write_health(&app, id, &gen, None, false);
                        }
                    } else {
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
                        let healthy = installed
                            && tokio::time::timeout(Duration::from_secs(2), reduce(id, &probe))
                                .await
                                .is_ok_and(|result| result.is_ok());
                        let _ = mark_health(&app, id, &gen, healthy);
                    }
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
                    let wake_sequence=*wake_hints.borrow();
                    // A wake received during an in-flight reducer stays pending for
                    // that user. Same-second writes do not depend on polling recovery.
                    let hinted=id=="automation"&&last_dispatched_wake.get(&uid)!=Some(&wake_sequence);
                    if !hinted && last_started.get(&uid) == Some(&now()) {
                        continue;
                    }
                    if !inflight.insert(uid.clone()) {
                        continue;
                    }
                    let startup = !seen.contains(&uid);
                    cursor = cursor.wrapping_add(1);
                    last_started.insert(uid.clone(), now());
                    last_dispatched_wake.insert(uid.clone(),wake_sequence);
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
    let mut notes_by_agent=json!({});
    if id=="automation" {
        for agent in app.store.main_chat_agent_ids().await? {
            let notes=app.store.for_agent(&agent).await?.main_chat_state().await?;
            if notes["settings"]["enabled"]==true {
                if let Some(sid)=notes["session_id"].as_str() {
                    if let Ok(scope)=execution_scope(app,&agent,Some(sid),false).await {
                        if perform(app,id,"bootstrap_main",json!({"session_id":sid,"id":crate::main_chat::review_id(&agent)}),None,None,true,scope).await.is_err() {
                            tracing::warn!(operation="main_notes_subscription","Main Notes review subscription unavailable");
                        }
                    }
                }
            }
            notes_by_agent[agent]=notes;
        }
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
    let mut notes_inputs=json!({});
    for (agent,notes) in notes_by_agent.as_object().into_iter().flatten() {
        let cursor=initial["state"]["main_notes_cursors"][agent].as_u64().unwrap_or(0);
        let mut changes:Vec<Value>=notes["changes"].as_object().into_iter().flatten().map(|(_,v)|v.clone()).filter(|v|v["version"].as_u64().unwrap_or(0)>cursor).collect();
        changes.sort_by_key(|v|v["version"].as_u64().unwrap_or(0));changes.truncate(128);
        notes_inputs[agent]=json!({"enabled":notes["settings"]["enabled"],"session_id":notes["session_id"],"changes":changes});
    }
    perform(app,id,"tick",json!({"events":page["events"],"cursor":page["next_seq"],"pruned_through":page["pruned_through"],"event_page":page,"agent_settings":settings,"agent_allowed":allowed,"main_notes_agents":notes_inputs}),None,None,true,json!({"startup":startup,"event_scan_cursor":cursor})).await?;
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
    Extension(app): Extension<App>,
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
    if automation_ability_host(&id) {
        return automation_lifecycle(&app, action).await;
    }
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
                Extension(app.clone()),
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

#[cfg(test)]
mod authenticated_caller_namespace_tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    async fn request(
        router: &Router,
        token: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .uri(path)
            .header("cookie", format!("v3_session={token}"));
        let bytes = if let Some(body) = body {
            builder = builder
                .method("POST")
                .header("content-type", "application/json")
                .header("x-csrf-token", "module-csrf");
            body.to_string()
        } else {
            String::new()
        };
        let response = router
            .clone()
            .oneshot(builder.body(Body::from(bytes)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    #[tokio::test]
    async fn real_router_binds_module_namespaces_and_admin_checks_to_authenticated_caller() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let account = app
            .store
            .create_user_account("modulecaller", "fixture-only-password-hash")
            .await
            .unwrap();
        let uid = account["user_id"].as_str().unwrap().to_owned();
        app.store
            .update_account_access(
                &uid,
                account["access_revision"].as_u64().unwrap(),
                crate::access::AccountStatus::Active,
                crate::access::Role::User,
                "basic",
            )
            .await
            .unwrap();
        app.store
            .set_agent_membership(
                &uid,
                "shared_default",
                crate::access::Relationship::Member,
                0,
            )
            .await
            .unwrap();
        // Give this ordinary caller Automation metadata authority, while the
        // global administrator's capability level remains denied for that ability.
        let mut tier = app.store.entitlement_tier("basic").await.unwrap().unwrap();
        let expected = tier.revision;
        tier.ability_permissions
            .insert("automation".into(), crate::access::Permission::Auto);
        app.store
            .save_entitlement_tier(tier, expected)
            .await
            .unwrap();
        let levels = app.store.capability_levels("shared_default").await.unwrap();
        let mut member = levels.levels["member"].clone();
        member
            .ability_permissions
            .insert("automation".into(), crate::access::Permission::Auto);
        app.store
            .save_capability_level("shared_default", member, levels.revision)
            .await
            .unwrap();
        let base = app
            .store
            .agent_ability_permissions("shared_default")
            .await
            .unwrap();
        let mut permissions: std::collections::BTreeMap<String, crate::access::Permission> =
            serde_json::from_value(base["ability_permissions"].clone()).unwrap();
        permissions.insert("automation".into(), crate::access::Permission::Auto);
        app.store
            .set_agent_ability_permissions(
                "shared_default",
                base["revision"].as_u64().unwrap(),
                permissions,
            )
            .await
            .unwrap();
        let caller = app.store.for_user(&uid).await.unwrap();
        app.store
            .module_cas("automation", 0, &json!({"state":{"owner":"admin"}}))
            .await
            .unwrap();
        app.store
            .module_cas("automation", 1, &json!({"state":{"owner":"admin"}}))
            .await
            .unwrap();
        caller
            .module_cas("automation", 0, &json!({"state":{"owner":"caller"}}))
            .await
            .unwrap();
        app.store
            .module_cas(
                "outbound_notifications",
                0,
                &json!({"state":{"private_admin_marker":"not-for-caller"}}),
            )
            .await
            .unwrap();
        let refreshed = app.store.account_by_id(&uid).await.unwrap().unwrap();
        let token = uuid();
        app.store.put(&format!("auth:{}",digest(&json!(token))),&json!({"user_id":uid,"generation":refreshed["generation"],"expires":now()+3600,"csrf":"module-csrf"})).await.unwrap();
        let router = crate::api::router(app.clone());
        let (status, body) =
            request(&router, &token, "/api/v3/modules/automation/status", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["revision"], 1,
            "State(global admin) must never substitute for Extension(caller)"
        );
        assert!(!body.to_string().contains("not-for-caller"));
        for (path, body) in [
            ("/api/v3/modules/outbound_notifications/status", None),
            (
                "/api/v3/modules/outbound_notifications/call",
                Some(json!({"action":"list","args":{},"client_request_id":"caller-list"})),
            ),
            (
                "/api/v3/modules/outbound_notifications/assets/page.js",
                None,
            ),
            ("/api/v3/admin/modules", None),
            (
                "/api/v3/admin/modules/automation",
                Some(json!({"action":"disable"})),
            ),
        ] {
            assert_eq!(
                request(&router, &token, path, body).await.0,
                StatusCode::FORBIDDEN,
                "{path}"
            );
        }
        let (status,_)=request(&router,&token,"/api/v3/modules/automation/call",Some(json!({"action":"schedule_task","agent_id":"shared_default","args":{},"client_request_id":"caller-scope"}))).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "the caller passes its own grant check before the omitted/invalid package operation"
        );
        let (status,_)=request(&router,&token,"/api/v3/modules/automation/call",Some(json!({"action":"schedule_task","agent_id":"unavailable_agent","args":{},"client_request_id":"forbidden-agent"}))).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(
            caller.module_state("automation").await.unwrap()["revision"],
            1
        );
        assert_eq!(
            app.store.module_state("automation").await.unwrap()["revision"],
            2
        );
        assert_eq!(
            caller.module_state("outbound_notifications").await.unwrap()["revision"],
            0
        );
        assert_eq!(
            app.store
                .module_state("outbound_notifications")
                .await
                .unwrap()["revision"],
            1
        );
    }
}

#[cfg(test)]
mod automation_ability_host_tests {
    use super::*;
    /// A process exit cannot grant health to a wrong or incomplete native protocol.
    #[test]
    fn native_health_requires_the_exact_probe_acknowledgement() {
        let ready = json!({"result":{"protocol":1,"package":"automation","ready":true}});
        assert!(automation_probe_ready(&ready));
        for bad in [
            json!({"result":{"protocol":2,"package":"automation","ready":true}}),
            json!({"result":{"protocol":1,"package":"outbound","ready":true}}),
            json!({"result":{"protocol":1,"package":"automation","ready":false}}),
            json!({"result":{}}),
            Value::Null,
        ] {
            assert!(!automation_probe_ready(&bad));
        }
    }
    /// Compatible replacement identifies code independently from the retained installation authority.
    #[test]
    fn compatible_implementation_changes_keep_activation_epoch_separate() {
        let path = FsPath::new("/not-an-installed-image-distribution");
        let first =
            json!({"_host_generation":"a".repeat(64),"_host_implementation":"b".repeat(64)});
        let mut next = first.clone();
        next["_host_implementation"] = json!("c".repeat(64));
        assert_eq!(
            manifest_generation("automation", path, &first).unwrap(),
            manifest_generation("automation", path, &next).unwrap()
        );
        assert_ne!(
            manifest_implementation("automation", path, &first).unwrap(),
            manifest_implementation("automation", path, &next).unwrap()
        );
        next["_host_generation"] = json!("d".repeat(64));
        assert_ne!(
            manifest_generation("automation", path, &first).unwrap(),
            manifest_generation("automation", path, &next).unwrap()
        );
    }
    /// A fresh receipt cannot carry health across either a code update or activation revocation.
    #[test]
    fn native_health_requires_both_current_bindings_and_a_fresh_probe() {
        let mut health =
            json!({"healthy":true,"generation":"epoch","implementation":"code-a","checked_at":100});
        assert!(health_binding_current(
            &health,
            "epoch",
            Some("code-a"),
            100
        ));
        assert!(!health_binding_current(
            &health,
            "epoch",
            Some("code-b"),
            100
        ));
        assert!(!health_binding_current(
            &health,
            "next-epoch",
            Some("code-a"),
            100
        ));
        assert!(!health_binding_current(
            &health,
            "epoch",
            Some("code-a"),
            116
        ));
        health["implementation"] = Value::Null;
        assert!(!health_binding_current(
            &health,
            "epoch",
            Some("code-a"),
            100
        ));
        assert!(health_binding_current(&health, "epoch", None, 100));
    }
    /// Source presence or retained legacy metadata must not silently reinstall an explicitly removed ability.
    #[test]
    fn installation_controls_preserve_omission_and_explicit_uninstall() {
        let legacy = json!({"enabled":false,"installed_generation":"a".repeat(64)});
        assert!(automation_installed(&legacy, true));
        assert!(!automation_installed(&legacy, false));
        assert!(!automation_installed(&json!({}), true));
        let mut removed = legacy.clone();
        removed["installed"] = json!(false);
        assert!(!automation_installed(&removed, true));
    }
    /// Marker updates are private, bounded and atomic; a rejected epoch preserves the previous controls.
    #[test]
    fn activation_files_reject_substitution_and_invalid_transitions() {
        let root = tempfile::tempdir().unwrap();
        let epoch = "a".repeat(64);
        save_automation_activation(root.path(), true, false, &epoch).unwrap();
        let original = automation_activation(root.path()).unwrap();
        assert_eq!(original["enabled"], false);
        assert!(save_automation_activation(root.path(), true, true, "invalid").is_err());
        assert_eq!(automation_activation(root.path()).unwrap(), original);
        let path = automation_activation_path(root.path());
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", &path).unwrap();
        assert!(automation_activation(root.path()).is_err());
    }
    /// Exercise the native bridge's real pre-exec boundary, not a plugin's voluntary self-check.
    #[tokio::test]
    async fn native_reducer_starts_without_access_to_host_files() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("private-state");
        std::fs::write(&state, b"host-private-fixture").unwrap();
        let mut allowed = reducer_process("automation", FsPath::new("/bin/true")).unwrap();
        assert!(allowed.status().await.unwrap().success());
        let mut denied = reducer_process("automation", FsPath::new("/bin/cat")).unwrap();
        let result = denied
            .arg(state)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .await
            .unwrap();
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
    }
}

#[cfg(test)]
mod main_notes_namespace_tests {
    use super::*;
    #[test]
    fn reserved_schedules_require_host_binding() {
        let args=json!({"request_id":"main-notes-action-intent"});
        assert!(main_notes_schedule_guard("schedule_task",&args,&json!({})).is_err());
        assert!(main_notes_schedule_guard("schedule_task",&args,&json!({"main_notes_binding_verified":true})).is_ok());
        assert!(main_notes_schedule_guard("event_subscribe",&json!({"request_id":"main-notes-review-agent"}),&json!({"main_notes_binding_verified":true})).is_err());
        assert!(main_notes_schedule_guard("update_automation",&json!({"id":"main-notes-action-intent"}),&json!({})).is_err());
        assert!(main_notes_schedule_guard("schedule_task",&json!({"request_id":"ordinary"}),&json!({})).is_ok());
    }
}
