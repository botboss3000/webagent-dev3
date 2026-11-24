//! Shared fixtures for the runtime acceptance tests: isolated storage, a local provider and helpers.
use super::*;
/// Build native catalog metadata without requiring independently compiled executable artifacts.
/// Keep the fixed Automation host declaration; generic isolated packages remain excluded.
pub(crate) fn native_catalog() -> abilities::Catalog {
    static ROOT: std::sync::OnceLock<TempDir> = std::sync::OnceLock::new();
    let root = ROOT.get_or_init(|| {
        let target = tempfile::tempdir().expect("native catalog fixture");
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("abilities");
        // A build without the Administrator tier has no adapters for the real pack, so it
        // gets the test-only ability like an installation without the pack.
        let tier = cfg!(feature = "administrator");
        copy_native(
            &source,
            target.path(),
            if tier { &[] } else { &["Administrator"] },
        );
        if !tier || !source.join("Administrator/codebase_admin").is_dir() {
            synthetic_codebase_admin(target.path());
        }
        target
    });
    abilities::Catalog::load(root.path()).expect("validated native fixture catalog")
}

/// Copy native declarations and their confined inputs; validate fixed host contracts through Catalog::load.
pub(crate) fn copy_native(source: &std::path::Path, target: &std::path::Path, skip: &[&str]) {
    let declaration = source.join("plugin.json");
    if declaration.is_file() {
        let manifest: Value = serde_json::from_slice(&std::fs::read(declaration).unwrap()).unwrap();
        // Host-interface plugins are dispatched by the app itself, so they load natively.
        let native_automation =
            manifest["id"] == "automation" && manifest["host_interface"] == "automation_v1";
        let host_requests = manifest["host_interface"] == "requests_v1";
        if manifest["dispatch"] == "isolated" && !native_automation && !host_requests {
            return;
        }
    }
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        assert!(
            !entry.file_type().unwrap().is_symlink(),
            "native fixture source symlink"
        );
        if skip.iter().any(|name| entry.file_name() == *name) {
            continue;
        }
        let output = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_native(&entry.path(), &output, &[]);
        } else {
            std::fs::copy(entry.path(), output).unwrap();
        }
    }
}

/// Without the optional Administrator pack, give engine tests a test-only ability under the
/// same ID that declares the core-schema file, prompt-slot and command tools they drive.
/// It exists only in this temporary catalog; pack tools such as `build_work` stay absent.
pub(crate) fn synthetic_codebase_admin(root: &std::path::Path) {
    let group = root.join("Administrator");
    let ability = group.join("codebase_admin");
    std::fs::create_dir_all(ability.join("skills")).unwrap();
    std::fs::create_dir_all(ability.join("tools")).unwrap();
    for tool in ["file_read", "file_edit"] {
        std::fs::write(
            ability.join(format!("tools/{tool}.skill.md")),
            "Match one exact span.",
        )
        .unwrap();
    }
    std::fs::write(
        ability.join("skills/ui_design.skill.md"),
        "---\nid: ui_design\ntitle: UI design\nwhen_to_use: Use when changing UI colours.\n---\nUse theme tokens.",
    )
    .unwrap();
    std::fs::write(
        group.join("_group.json"),
        r#"{"name":"Administrator","order":1}"#,
    )
    .unwrap();
    let manifest = json!({
        "display_name": "Codebase Admin (test fixture)",
        "kind": "ability",
        "description": "Test-only workspace tools for engine coverage without the Administrator pack",
        "tools": ["file_list", "file_read", "file_search", "file_edit", "file_write",
            "state_inspect", "prompt_slot_read", "prompt_slot_write", "shell", "python",
            "checkout_edit"],
        "readiness": "ready"
    });
    std::fs::write(
        ability.join("codebase_admin.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::create_dir_all(ability.join("skills")).unwrap();
    std::fs::write(
        ability.join("skills/codebase_admin.skill.md"),
        "# Codebase Admin\n\nUse scoped workspace tools.",
    )
    .unwrap();
}

/// Held by a test that injects a process-wide engine component failure, and by tests
/// whose runs must not see one: the injected cooldown would pause their runs.
pub(crate) static COMPONENT_FAILURE_FENCE: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

pub(crate) async fn fixture_uninitialized() -> (TempDir, App) {
    let root = tempfile::tempdir().unwrap();
    let control_dir = root.path().join("control");
    std::fs::create_dir(&control_dir).unwrap();
    std::fs::write(control_dir.join("dispatch.lock"), b"").unwrap();
    std::fs::write(control_dir.join("shutoff"), b"0").unwrap();
    let store = Store::open(root.path()).await.unwrap();
    std::fs::create_dir_all(root.path().join("config")).unwrap();
    store
        .put("settings", &json!({"model":"fixture"}))
        .await
        .unwrap();
    store
        .put("control", &json!({"shutoff":false}))
        .await
        .unwrap();
    store
        .put(
            "provider_tokens",
            &json!({"access_token":"fixture-only","expires_at":now()+3600}),
        )
        .await
        .unwrap();
    store
        .put(
            "admin",
            &json!({"username":"admin","generation":null,"password_hash":"fixture"}),
        )
        .await
        .unwrap();
    // A per-test runtime file (absolute path) that handlers may save directly.
    let runtime_path = root.path().join("config/loop-runtime.json");
    std::fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let runtime = loop_runtime::Config::default();
    runtime.persist(&runtime_path).unwrap();
    // Isolate fixture edits; production imports legacy JSON only if data authority is absent.
    // COMPACTION FIXTURE: isolate JSON edits per test while exercising the same
    // Config::read call-boundary path as main.rs. This copy is test-only, not
    // production bootstrap; deploy.sh mounts the single repository config file.
    let compaction_config_path = root.path().join("context-compaction.json");
    std::fs::copy(
        context_compaction::Config::configured_path(),
        &compaction_config_path,
    )
    .unwrap();
    let app = App {
        runtime: Arc::new(RwLock::new(runtime)),
        runtime_path,
        compaction_config_path,
        message_api_token_hash: None,
        store,
        chat_actor: None,
        chat_access: None,
        identity: None,
        abilities: Arc::new(native_catalog()),
        gate: Arc::new(Mutex::new(())),
        provider: provider::Provider::new(),
        refresh: Arc::new(Mutex::new(())),
        login: Arc::new(RwLock::new(None)),
        active: Arc::new(Mutex::new(HashMap::new())),
        slots: Arc::new(Semaphore::new(1)),
        ready: Arc::new(AtomicBool::new(true)),
        stop: CancellationToken::new(),
        login_attempts: webagent_accounts::LoginAttempts::default(),
        control_dir,
        tool_socket: root.path().join("worker.sock"),
        build_socket: root.path().join("host-build.sock"),
        chat_ui_config_path: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui/chat/chat_ui.json"),
        secure: false,
    };
    (root, app)
}
/// The default agent's roster: one subscription model on the fixture's `provider_tokens`.
pub(crate) async fn default_subscription_roster(store: &Store, catalog: &mut model_selection::Catalog) {
    use model_selection::*;
    use std::collections::{BTreeMap, BTreeSet};
    let selection = provider::ProviderSelection::parse(
        &store.agent_model().await.unwrap(),
        DEFAULT_AGENT_MODEL,
    )
    .unwrap();
    let grants = BTreeSet::from(["shared_default".to_owned()]);
    let id = "default-subscription".to_owned();
    catalog.connections.insert(
        id.clone(),
        Connection {
            id: id.clone(),
            owner_id: "admin".into(),
            transport: Transport::OpenaiSubscription,
            credential_ref: "provider_tokens".into(),
            allowed_agents: grants.clone(),
            revision: 1,
            revoked: false,
        },
    );
    catalog.entries.insert(
        id.clone(),
        ModelEntry {
            id: id.clone(),
            owner_id: "admin".into(),
            connection_id: id.clone(),
            upstream_model: selection.model,
            display_name: "Default subscription".into(),
            capabilities: BTreeSet::from(["text".into()]),
            allowed_agents: grants,
            bridge_clients: Vec::new(),
            revision: 1,
        },
    );
    catalog.agent_models.insert(
        "shared_default".into(),
        AgentModels {
            retired_limits: Default::default(),
            efforts: BTreeMap::new(),
            agent_id: "shared_default".into(),
            owner_id: "admin".into(),
            entry_ids: vec![id.clone()],
            roles: BTreeMap::from([(ModelRole::Standard, RoleAssignment::new(id))]),
            mode_models: Default::default(),
            aliases: BTreeMap::new(),
            revision: 1,
        },
    );
}
pub(crate) async fn fixture() -> (TempDir, App) {
    crate::tools::install_tool_defaults();
    let (root, app) = fixture_uninitialized().await;
    app.store
        .create_admin_account("admin", "fixture-only-password-hash", "fixture-generation")
        .await
        .unwrap();
    // The default agent's subscription model is its Standard and, for compaction fixtures,
    // its System role; capabilities alone never select a role.
    let mut catalog = app.store.model_catalog().await.unwrap();
    default_subscription_roster(&app.store, &mut catalog).await;
    let roster = catalog.agent_models.get_mut("shared_default").unwrap();
    roster.roles.insert(
        model_selection::ModelRole::System,
        roster.roles[&model_selection::ModelRole::Standard].clone(),
    );
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
    grant_fixture_levels(&app.store, "shared_default").await;
    // Most runtime tests count task list revisions exactly; the task gate's filing is
    // switched on only where a test covers it.
    webagent_engine::runtime::run::TASK_GATE_DEFAULT
        .store(false, std::sync::atomic::Ordering::Relaxed);
    webagent_engine::runtime::critical_guard::GUARD_DEFAULT
        .store(false, std::sync::atomic::Ordering::Relaxed);
    crate::store::RECORD_MESSAGE_REQUESTS.store(false, std::sync::atomic::Ordering::Relaxed);
    (root, app)
}
/// Switch the default agent's task gate (Task Progress setting `task_gate`).
pub(crate) async fn set_task_gate(app: &App, on: bool) {
    let mut settings = app.store.agent_abilities().await.unwrap();
    let revision = settings["revision"].as_u64().unwrap();
    if !settings["settings"].is_object() {
        settings["settings"] = json!({});
    }
    settings["settings"]["task_progress"] = json!({"task_gate": on});
    app.store
        .save_agent_abilities(&settings, revision)
        .await
        .unwrap();
}
/// Unsaved capability levels grant nothing, so fixtures save the two executable
/// adapters explicitly at every level, as an Operator would.
pub(crate) async fn grant_fixture_levels(store: &Store, agent: &str) {
    crate::tools::install_tool_defaults();
    let mut levels = store.capability_levels(agent).await.unwrap();
    for id in ["operator", "member", "visitor"] {
        let mut level = levels.levels[id].clone();
        level.ability_permissions = [
            ("codebase_admin".into(), crate::access::Permission::Auto),
            ("web_access".into(), crate::access::Permission::Auto),
        ]
        .into();
        levels = store
            .save_capability_level(agent, level, levels.revision)
            .await
            .unwrap();
    }
}

// Runtime assertions change the authoritative roster entry; legacy settings remain
// independently covered by their own API tests.
pub(crate) async fn fixture_model(app: &App, model: &str) {
    let mut catalog = app.store.model_catalog().await.unwrap();
    let entry = catalog.agent_models["shared_default"].roles[&model_selection::ModelRole::Standard]
        .entry_id
        .clone();
    catalog.entries.get_mut(&entry).unwrap().upstream_model = model.into();
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
}

pub(crate) fn checkpoint_pre_tool(name: &str) -> workflow::Input {
    workflow::Input::PreTool(workflow::PreToolInput {
        name: name.into(),
        arguments_digest: "fixture-digest".into(),
        policy_version: 1,
        mode_revision: 0,
        visibility_revision: 0,
        ability_state_revision: 0,
    })
}

/// Bound for test work that must finish. It only catches hangs: a full suite runs hundreds of
/// tests on a few threads beside other builds, where short limits failed at random.
pub(crate) const TEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Set tool `name`'s permission on the operator level of the default agent.
/// Each level stays within the one above it, so the levels are retried until all three hold
/// `permission`, whichever direction it moves.
pub(crate) async fn set_tool_permission(app: &App, name: &str, permission: access::Permission) {
    for _ in 0..3 {
        for id in ["visitor", "member", "operator"] {
            let levels = app.store.capability_levels("shared_default").await.unwrap();
            let mut level = levels.levels[id].clone();
            if level.tool_permissions.get(name) == Some(&permission) {
                continue;
            }
            level.tool_permissions.insert(name.into(), permission);
            let _ = app
                .store
                .save_capability_level("shared_default", level, levels.revision)
                .await;
        }
    }
    let levels = app.store.capability_levels("shared_default").await.unwrap();
    assert_eq!(
        levels.levels["operator"].tool_permissions.get(name),
        Some(&permission)
    );
}

pub(crate) async fn activate_codebase(app: &App, sid: &str) {
    let state = session_ability_set(
        State(app.clone()),
        Path((sid.to_owned(), "codebase_admin".into())),
        Json(SessionAbilityBody {
            state: "active".into(),
            expected_revision: 0,
        }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(state["active"], json!(["codebase_admin"]));
}

pub(crate) async fn conversation(app: &App) -> String {
    create_session(
        State(app.clone()),
        Json(json!({"client_request_id":uuid(),"title":"Test"})),
    )
    .await
    .unwrap()
    .0["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

pub(crate) async fn queued(app: &App, sid: &str) -> String {
    send(
        State(app.clone()),
        Path(sid.to_owned()),
        Json(json!({"client_message_id":uuid(),"text":"hello"})),
    )
    .await
    .unwrap()
    .1
     .0["run_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

pub(crate) async fn running(app: &App, id: &str, generation: &str) {
    let mut run = app.run(id).await.unwrap();
    assert!(app
        .store
        .claim_run(&mut run, "fixture-loop", generation)
        .await
        .unwrap());
    // Renew the 30-second claim like the production loop, so build-server load cannot
    // expire it mid-test. Renewal stops once the run ends or a later claim replaces it.
    let epoch = app.run(id).await.unwrap()["lease_epoch"].as_i64().unwrap();
    let (store, id, generation) = (app.store.clone(), id.to_owned(), generation.to_owned());
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            if !store
                .renew_run_lease(&id, &generation, "fixture-loop", epoch)
                .await
                .unwrap_or(false)
            {
                break;
            }
        }
    });
}

// Manual multi-turn journeys need the same fenced lease renewal as the production loop.
pub(crate) async fn execute_fixture_journey(
    app: App,
    id: String,
    generation: String,
    cancel: CancellationToken,
) {
    let epoch = app.run(&id).await.unwrap()["lease_epoch"].as_i64().unwrap();
    let store = app.store.clone();
    let lease_id = id.clone();
    let lease_generation = generation.clone();
    let lease_cancel = cancel.clone();
    let watcher = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            if !store
                .renew_run_lease(&lease_id, &lease_generation, "fixture-loop", epoch)
                .await
                .unwrap_or(false)
            {
                lease_cancel.cancel();
                break;
            }
        }
    });
    let result = tokio::time::timeout(
        Duration::from_secs(90),
        execute_run(app, id, generation, cancel),
    )
    .await;
    watcher.abort();
    let _ = watcher.await;
    result
        .expect("fixture journey exceeded its bounded deadline")
        .unwrap();
}

/// True for the set_execution_mode entries the server adds to show the current mode.
pub(crate) fn mode_entry(item: &Value) -> bool {
    item["call_id"]
        .as_str()
        .is_some_and(|id| id.starts_with("set_execution_mode-"))
}

/// `input` without the server's set_execution_mode entries.
pub(crate) fn without_mode_entries(input: &Value) -> Vec<Value> {
    input
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| !mode_entry(item))
        .cloned()
        .collect()
}

/// Inspect the final call-scoped Responses developer suffix, never durable history or main instructions.
pub(crate) fn native_call_suffix(request: &Value) -> &str {
    // A call with no one-off items has no suffix.
    match request["input"].as_array().unwrap().last() {
        Some(last) if last["role"] == "developer" => last["content"].as_str().unwrap(),
        _ => "",
    }
}

/// Locate current ability guidance only in explicitly labelled Responses developer frames.
/// The ability's guidance block in the request's instructions, where every profile puts skills.
pub(crate) fn native_ability_frames<'a>(request: &'a Value, ability: &str) -> Vec<&'a str> {
    let header = format!("## Untrusted live guidance: ability:{ability}\n");
    let instructions = request["instructions"].as_str().unwrap_or_default();
    instructions
        .find(&header)
        .map(|start| {
            let body = &instructions[start..];
            let end = body[header.len()..]
                .find("## Untrusted live guidance: ")
                .map_or(body.len(), |next| next + header.len());
            vec![&body[..end]]
        })
        .unwrap_or_default()
}

/// Preload journey: the stub scores `file read` high and `file search` as a suggestion.
pub(crate) static WARM_ITEMS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub(crate) fn warm_stub(_query: &str, items: &[Vec<String>]) -> Option<Vec<f32>> {
    WARM_ITEMS.store(items.len(), std::sync::atomic::Ordering::SeqCst);
    Some(vec![0.0; items.len()])
}
pub(crate) fn warm_failure(_query: &str, _items: &[Vec<String>]) -> Option<Vec<f32>> {
    None
}

pub(crate) fn request_tools(request: &Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect()
}

pub(crate) async fn ability_request(
    app: &App,
    method: axum::http::Method,
    uri: &str,
    body: Option<Value>,
    cookie: Option<&str>,
    csrf: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    if let Some(csrf) = csrf {
        request = request.header("x-csrf-token", csrf);
    }
    let payload = body.map(|body| body.to_string()).unwrap_or_default();
    if !payload.is_empty() {
        request = request.header("content-type", "application/json");
    }
    let response = router(app.clone())
        .oneshot(request.body(Body::from(payload)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    // CATALOG-TEST-BOUND: the drop-in catalog exceeds 64 KiB; retain a bounded
    // response reader without truncating authorization/revision assertions.
    let bytes = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

pub(crate) async fn ability_auth(app: &App) -> String {
    let token = uuid();
    app.store
        .put(
            &format!("auth:{}", digest(&json!(token))),
            &json!({"csrf":"ability-fixture","expires":now()+3600,"generation":"fixture-generation"}),
        )
        .await
        .unwrap();
    format!("v3_session={token}")
}

// MODEL-SELECTION: agent-bound admission and dispatch safety, isolated fixtures only.
pub(crate) async fn selection_fixture(app: &App) -> model_selection::Catalog {
    use model_selection::*;
    use std::collections::{BTreeMap, BTreeSet};
    let agent_pool = crate::agent_storage::open_agent_pool(&app.store.root, "agent-a")
        .await
        .unwrap();
    let metadata = json!({"owner_id":"admin","ability_modes":{"codebase_admin":"loaded"},
        "skill_modes":{},"tool_modes":{},"ability_revision":0});
    sqlx::query("INSERT INTO agents(id,name,description,provider,model,status,metadata,revision,created_at,updated_at) VALUES('agent-a','Agent A','','openai_subscription','fixture','active',?,1,0,0)")
        .bind(app.store.seal(&metadata).unwrap()).execute(&agent_pool).await.unwrap();
    agent_pool.close().await;
    app.store.register_agent("agent-a", "admin").await.unwrap();
    app.store
        .set_agent_membership("admin", "agent-a", crate::access::Relationship::Owner, 0)
        .await
        .unwrap();
    let mut catalog = app.store.model_catalog().await.unwrap();
    for (id, model) in [("one", "model-one"), ("two", "model-two")] {
        let credential_ref = format!("credential:{id}");
        app.store
            .put(
                &credential_ref,
                &json!({"access_token":format!("token-{id}"),"expires_at":now()+3600}),
            )
            .await
            .unwrap();
        catalog.connections.insert(
            id.into(),
            Connection {
                id: id.into(),
                owner_id: "admin".into(),
                transport: Transport::OpenaiSubscription,
                credential_ref,
                allowed_agents: BTreeSet::from(["shared_default".into(), "agent-a".into()]),
                revision: 1,
                revoked: false,
            },
        );
        catalog.entries.insert(
            id.into(),
            ModelEntry {
                id: id.into(),
                owner_id: "admin".into(),
                connection_id: id.into(),
                upstream_model: model.into(),
                display_name: model.into(),
                capabilities: BTreeSet::from(["text".into()]),
                allowed_agents: BTreeSet::from(["shared_default".into(), "agent-a".into()]),
                bridge_clients: Vec::new(),
                revision: 1,
            },
        );
    }
    // Each tenant has an explicit roster, role appointments and aliases. Sharing
    // authorizes both the entry and its connection without relying on common ownership.
    for (agent, primary, aliases) in [
        ("shared_default", "one", vec![("one", "two")]),
        ("agent-a", "two", vec![("two", "one"), ("one", "two")]),
    ] {
        let revision = catalog
            .agent_models
            .get(agent)
            .map(|a| a.revision)
            .unwrap_or(1);
        catalog.agent_models.insert(
            agent.into(),
            AgentModels {
                retired_limits: Default::default(),
                efforts: std::collections::BTreeMap::new(),
                agent_id: agent.into(),
                owner_id: "admin".into(),
                entry_ids: vec!["one".into(), "two".into()],
                revision,
                roles: BTreeMap::from([
                    (ModelRole::Standard, RoleAssignment::new(primary)),
                    (ModelRole::System, RoleAssignment::new("one")),
                ]),
                mode_models: Default::default(),
                aliases: aliases
                    .into_iter()
                    .map(|(id, alias)| (id.to_owned(), vec![alias.to_owned()]))
                    .collect(),
            },
        );
    }
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap()
}

pub(crate) type BackgroundJourneyRequest = (Value, tokio::sync::oneshot::Sender<Value>);

/// Cooperative handshake budget for the background journey.
///
/// A queued read is normally started by the service's wake hint, but the service
/// still falls back to the scheduler poll (`background_rescan_interval_ms`, 5s
/// by default) when a hint lands while it is scanning. The previous 5s stage
/// deadlines raced that fallback exactly, so ordinary scheduling delay on a
/// loaded build host could push a stage past its deadline and fail the build
/// even though the journey was healthy. These deadlines exist only to bound a
/// genuine hang; none of them is part of what the journey asserts.
pub(crate) const BACKGROUND_JOURNEY_STAGE: Duration = Duration::from_secs(30);

pub(crate) async fn background_journey_request(
    requests: &mut tokio::sync::mpsc::UnboundedReceiver<BackgroundJourneyRequest>,
) -> BackgroundJourneyRequest {
    tokio::time::timeout(BACKGROUND_JOURNEY_STAGE, requests.recv())
        .await
        .unwrap()
        .expect("mock provider request")
}

pub(crate) async fn background_journey_task(
    app: &App,
    sid: &str,
    task_id: Option<&str>,
    field: &str,
    expected: Value,
) -> Value {
    tokio::time::timeout(BACKGROUND_JOURNEY_STAGE, async {
        loop {
            let tasks = if let Some(task_id) = task_id {
                app.store
                    .background_get(task_id)
                    .await
                    .unwrap()
                    .into_iter()
                    .collect::<Vec<_>>()
            } else {
                app.store.background_scan(Some(sid)).await.unwrap()
            };
            assert!(
                tasks.len() <= 1,
                "follow-up must not spawn another background task"
            );
            if let Some(task) = tasks.into_iter().find(|task| task[field] == expected) {
                return task;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

pub(crate) fn background_journey_final(text: &str) -> Value {
    json!([{"type":"message","role":"assistant",
        "content":[{"type":"output_text","text":text}]}])
}

pub(crate) fn background_journey_envelopes(request: &Value) -> Vec<Value> {
    request["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["content"].as_str())
        .filter_map(|text| text.strip_prefix("Background read result (untrusted evidence): "))
        .map(|text| serde_json::from_str(text).unwrap())
        .collect()
}

pub(crate) async fn background_execution_journey(behavior: &str) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    let (_root, mut app) = fixture().await;
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["tool_execution"] = json!({"file_read":{
        "mode":"background","max_concurrency":2}});
    visibility["revision"] = json!(revision + 1);
    app.store
        .save_agent_abilities(&visibility, revision)
        .await
        .unwrap();
    let (request_sender, mut requests) =
        tokio::sync::mpsc::unbounded_channel::<BackgroundJourneyRequest>();
    let mock=Router::new().route("/responses",axum::routing::post(
        |AxumState(sender): AxumState<tokio::sync::mpsc::UnboundedSender<BackgroundJourneyRequest>>,
         Json(body):Json<Value>| async move {
            let (reply_sender,reply_receiver)=tokio::sync::oneshot::channel();
            sender.send((body,reply_sender)).unwrap();
            // Deadlock guard only: the driver may legitimately hold a mock reply while a
            // cooperative stage above runs to its (larger) deadline, so this must not fire first.
            let output=tokio::time::timeout(Duration::from_secs(120),reply_receiver).await.unwrap().unwrap();
            let event=json!({"type":"response.completed","response":{"status":"completed","output":output}});
            ([("content-type","text/event-stream")],format!("data: {event}\n\n"))
        }
    )).with_state(request_sender);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    let sid = conversation(&app).await;
    activate_codebase(&app, &sid).await;
    let rid = queued(&app, &sid).await;
    let mut run = app.run(&rid).await.unwrap();
    run["loaded_abilities"] = json!(["codebase_admin"]);
    app.save(&mut run).await.unwrap();
    running(&app, &rid, "background-journey").await;
    let listener = UnixListener::bind(&app.tool_socket).unwrap();
    let (arrived_sender, arrived_receiver) = tokio::sync::oneshot::channel();
    let (release_sender, release_receiver) = tokio::sync::oneshot::channel::<()>();
    let service = tokio::spawn(background_tasks::service(app.clone()));
    let loop_app = app.clone();
    let loop_rid = rid.clone();
    let origin = tokio::spawn(async move {
        execute_fixture_journey(
            loop_app,
            loop_rid,
            "background-journey".into(),
            CancellationToken::new(),
        )
        .await;
    });
    let (_, reply) = background_journey_request(&mut requests).await;
    // Start the worker-stage deadline only after the first model has been admitted.
    let worker = tokio::spawn(async move {
        // Bound each socket stage; the driver explicitly releases the held read between stages.
        let (stream, _) = tokio::time::timeout(BACKGROUND_JOURNEY_STAGE, listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        tokio::time::timeout(BACKGROUND_JOURNEY_STAGE, reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["arguments"]["path"], "background-evidence");
        assert!(request["arguments"].get("_execution_mode").is_none());
        arrived_sender.send(()).unwrap();
        let mut extra = String::new();
        tokio::select! {
            released=release_receiver=>{released.unwrap();},
            closed=reader.read_line(&mut extra)=>{
                assert_eq!(closed.unwrap(),0,"stop must close the outstanding worker connection");
                return (true,1);
            },
        }
        let reply =
            json!({"id":request["id"],"status":"success","output":"BACKGROUND_JOURNEY_EVIDENCE"});
        let reply = test_rpc_reply(&request, reply);
        tokio::time::timeout(
            BACKGROUND_JOURNEY_STAGE,
            reader.get_mut().write_all(format!("{reply}\n").as_bytes()),
        )
        .await
        .unwrap()
        .unwrap();
        drop(reader);
        (false, 1)
    });
    reply
        .send(
            json!([{"type":"function_call","call_id":"journey-background","name":"file_read",
        "arguments":json!({"path":"background-evidence"}).to_string()}]),
        )
        .unwrap();
    let (advanced, reply) = background_journey_request(&mut requests).await;
    tokio::time::timeout(BACKGROUND_JOURNEY_STAGE, arrived_receiver)
        .await
        .unwrap()
        .unwrap();
    let task = background_journey_task(&app, &sid, None, "status", json!("running")).await;
    let task_id = task["id"].as_str().unwrap().to_owned();
    assert!(background_journey_envelopes(&advanced).is_empty());
    let handles = advanced["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| {
            item["type"] == "function_call_output" && item["call_id"] == "journey-background"
        })
        .map(|item| serde_json::from_str::<Value>(item["output"].as_str().unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(handles.len(), 1);
    assert_eq!(handles[0]["status"], "task_start");
    assert_eq!(handles[0]["call_id"], task_id);
    if behavior == "overlap" {
        reply
            .send(
                json!([{"type":"function_call","call_id":"while-background","name":"get_time",
            "arguments":"{}"}]),
            )
            .unwrap();
        let (foreground, reply) = background_journey_request(&mut requests).await;
        assert_eq!(
            app.store.background_get(&task_id).await.unwrap().unwrap()["status"],
            "running"
        );
        assert!(background_journey_envelopes(&foreground).is_empty());
        assert!(foreground["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "function_call_output"
                && item["call_id"] == "while-background"));
        release_sender.send(()).unwrap();
        background_journey_task(&app, &sid, Some(&task_id), "status", json!("completed")).await;
        reply
            .send(
                json!([{"type":"function_call","call_id":"next-boundary","name":"get_time",
            "arguments":"{}"}]),
            )
            .unwrap();
        let (boundary, reply) = background_journey_request(&mut requests).await;
        let envelopes = background_journey_envelopes(&boundary);
        assert_eq!(envelopes.len(), 1);
        assert_eq!(envelopes[0]["background_task_id"], task_id);
        assert_eq!(envelopes[0]["source_kind"], "untrusted_tool_result");
        assert_eq!(envelopes[0]["post_run_result"], false);
        assert_eq!(
            envelopes[0]["untrusted_tool_result"]["output"],
            "BACKGROUND_JOURNEY_EVIDENCE"
        );
        reply
            .send(background_journey_final("Overlap completed."))
            .unwrap();
        tokio::time::timeout(BACKGROUND_JOURNEY_STAGE, origin)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            tokio::time::timeout(BACKGROUND_JOURNEY_STAGE, worker)
                .await
                .unwrap()
                .unwrap(),
            (false, 1)
        );
        let mut finished = app.run(&rid).await.unwrap();
        assert_eq!(finished["status"], "completed");
        assert!(!background_tasks::collect(&app, &mut finished)
            .await
            .unwrap());
        assert_eq!(finished["background_delivered"], json!([task_id]));
    } else {
        reply
            .send(background_journey_final("Origin completed."))
            .unwrap();
        tokio::time::timeout(BACKGROUND_JOURNEY_STAGE, origin)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(app.run(&rid).await.unwrap()["status"], "completed");
        {
            assert_eq!(
                app.store.background_get(&task_id).await.unwrap().unwrap()["status"],
                "running"
            );
            release_sender.send(()).unwrap();
            background_journey_task(&app, &sid, Some(&task_id), "status", json!("completed")).await;
            // A read that finishes after its run ended reports as a background notice:
            // quiet for silent (it joins the next user message), waking for wake.
            let task = background_journey_task(
                &app,
                &sid,
                Some(&task_id),
                "delivery_run_id",
                json!("background_notice"),
            )
            .await;
            assert!(task["wake_run_id"].is_null());
            assert_eq!(
                tokio::time::timeout(BACKGROUND_JOURNEY_STAGE, worker)
                    .await
                    .unwrap()
                    .unwrap(),
                (false, 1)
            );
            // The notice is written just after the task records its delivery.
            let (notice, receipt) = tokio::time::timeout(BACKGROUND_JOURNEY_STAGE, async {
                loop {
                    let notice = app
                        .store
                        .get(&format!("background_notice:{sid}:tool:{task_id}"))
                        .await
                        .unwrap();
                    let receipt = app
                        .store
                        .get(&format!("receipt:background_notice:tool:{task_id}"))
                        .await
                        .unwrap();
                    if notice.is_some() || receipt.is_some() {
                        break (notice, receipt);
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert!(notice.is_some() || receipt.is_some());
            if let Some(notice) = notice {
                assert_eq!(notice["source"], "tool");
                assert!(notice["reply"]
                    .as_str()
                    .unwrap()
                    .contains("BACKGROUND_JOURNEY_EVIDENCE"));
            }
            assert!(
                requests.try_recv().is_err(),
                "a background notice never starts a read-only follow-up run"
            );
        }
        assert_eq!(app.run(&rid).await.unwrap()["status"], "completed");
    }
    let expected_replies = match behavior {
        "overlap" => vec!["Overlap completed."],
        _ => vec!["Origin completed."],
    };
    let history = app.store.context(&sid).await.unwrap();
    for expected in expected_replies {
        assert_eq!(
            history
                .iter()
                .filter(|item| item["role"] == "assistant" && item.to_string().contains(expected))
                .count(),
            1,
            "one durable final reply"
        );
    }
    assert!(
        requests.try_recv().is_err(),
        "unexpected extra provider request"
    );
    app.stop.cancel();
    tokio::time::timeout(BACKGROUND_JOURNEY_STAGE, service)
        .await
        .unwrap()
        .unwrap();
    server.abort();
    let _ = server.await;
}

/// Copy only native Base source into this fixture; shared repository inputs are read-only.
pub(crate) fn copy_live_context_fixture(source: &std::path::Path, target: &std::path::Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let metadata = entry.file_type().unwrap();
        assert!(
            !metadata.is_symlink(),
            "fixture source must not follow symlinks"
        );
        let output = target.join(entry.file_name());
        if metadata.is_dir() {
            copy_live_context_fixture(&entry.path(), &output);
        } else {
            assert!(metadata.is_file());
            std::fs::copy(entry.path(), output).unwrap();
        }
    }
}

/// Valid selectable metadata stays current while only its complete body changes.
pub(crate) fn live_context_fixture_skill(body: &str) -> String {
    format!("---\nid: live_refresh\ntitle: Live refresh fixture\nwhen_to_use: Verify current live context\ndefault_mode: on_request\n---\n{body}")
}

/// Drive native load_skill and a second tool round through the actual execute_run loop.
/// Current source prompts, saved overrides and encrypted slots replace old text; the loaded
/// skill text is its tool result and never enters selectors.
pub(crate) async fn live_context_default_stack_journey() {
    use axum::routing::post;
    let (root, mut app) = Box::pin(fixture()).await;
    Box::pin(visibility_fixture_base_access(&app)).await;
    Box::pin(visibility_fixture_ability_access(&app, "web_access")).await;
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["tool_modes"]["context_assets"] = json!("loaded");
    app.store
        .save_agent_abilities(&visibility, revision)
        .await
        .unwrap();
    let source = root.path().join("live-source");
    let abilities = source.join("abilities");
    // Base is built in, so the live selectable skill lives in a folder ability.
    copy_live_context_fixture(
        &app.abilities.snapshot_root().join("Web/web_access"),
        &abilities.join("Web/web_access"),
    );
    let skill = abilities.join("Web/web_access/skills/live_refresh.skill.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    std::fs::write(&skill, live_context_fixture_skill("LIVE_SKILL_FIRST")).unwrap();
    // The catalog source carries its own prompt files.
    let prompt = source.join(webagent_configuration::prompts::PROMPTS);
    copy_live_context_fixture(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(webagent_configuration::prompts::PROMPTS),
        &prompt,
    );
    crate::api::live_configuration::validate_source_prompts(
        &serde_json::from_slice(&crate::live_assets::prompt_defaults_at(Some(&source)).unwrap())
            .unwrap(),
    )
    .unwrap();
    app.abilities = Arc::new(abilities::Catalog::load(&abilities).unwrap());
    let slot = Box::pin(app.store.upsert_prompt_slot(
        "live_refresh",
        "LIVE_SLOT_FIRST",
        None,
        None,
        None,
        0,
    ))
    .await
    .unwrap();
    let slot_revision = slot["revision"].as_i64().unwrap();
    let sid = conversation(&app).await;
    let rid = queued(&app, &sid).await;
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let callback_store = app.store.clone();
    let mock=Router::new().route("/responses",post(move |Json(body):Json<Value>| {
        let captured=captured.clone();let store=callback_store.clone();let skill=skill.clone();
        Box::pin(async move {
            let mut captured=captured.lock().await;
            captured.push(body);
            let output=match captured.len() {
                1=>json!([{"type":"function_call","call_id":"select-live-skill","name":"load_skill",
                    "arguments":"{\"ability_id\":\"web_access\",\"kind\":\"selectable\",\"skill_id\":\"live_refresh\"}"}]),
                2=>{
                    std::fs::write(&skill,live_context_fixture_skill("LIVE_SKILL_CURRENT")).unwrap();
                    Box::pin(store.upsert_prompt_slot("live_refresh","LIVE_SLOT_CURRENT",None,None,None,slot_revision)).await.unwrap();
                    json!([{"type":"function_call","call_id":"refresh-native-round","name":"get_time","arguments":"{}"}])
                },
                3=>json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done."}]}]),
                count=>panic!("unexpected provider call {count}"),
            };
            let event=json!({"type":"response.completed","response":{"status":"completed","output":output}});
            ([("content-type","text/event-stream")],format!("data: {event}\n\n"))
        })
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    let generation = "live-context-default-stack";
    running(&app, &rid, generation).await;
    let task_app = app.clone();
    let task_id = rid.clone();
    // Match the production scheduler boundary; never widen thread/runtime stacks.
    let execution = tokio::spawn(async move {
        execute_run(
            task_app,
            task_id,
            generation.into(),
            CancellationToken::new(),
        )
        .await
    });
    tokio::time::timeout(TEST_TIMEOUT, execution)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let durable = app.run(&rid).await.unwrap();
    assert_eq!(durable["status"], "completed", "{}", durable["error"]);
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 3);
    for request in &requests[..2] {
        let instructions = request["instructions"].as_str().unwrap();
        assert!(instructions.contains("LIVE_SLOT_FIRST"));
        assert!(!instructions.contains("LIVE_SLOT_CURRENT"));
    }
    let instructions = requests[2]["instructions"].as_str().unwrap();
    assert!(instructions.contains("LIVE_SLOT_CURRENT"));
    assert!(!instructions.contains("LIVE_SLOT_FIRST"));
    for request in requests.iter() {
        // Assert the actual Responses wire, where omitted strict defaults can make
        // unload-only fields mandatory and prevent a valid list invocation.
        let control = request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "context_assets")
            .unwrap();
        assert_eq!(control["strict"], false);
        assert_eq!(control["parameters"]["required"], json!(["action"]));
        assert_eq!(
            control["parameters"]["properties"]["selector"]["anyOf"][1]["required"],
            json!(["type", "tool_name"])
        );
    }
    assert!(!requests[0]["input"]
        .to_string()
        .contains("LIVE_SKILL_FIRST"));
    assert!(requests[1]["input"]
        .to_string()
        .contains("LIVE_SKILL_FIRST"));
    // The skill text is the load_skill result; while that result is in the history it is
    // not placed again, so a later edit waits for compaction.
    assert!(requests[2]["input"]
        .to_string()
        .contains("LIVE_SKILL_FIRST"));
    assert!(!requests[2]["input"]
        .to_string()
        .contains("LIVE_SKILL_CURRENT"));
    // A finished run keeps no input copy; the skill result lives in the session history.
    assert!(json!(app.store.context(&sid).await.unwrap())
        .to_string()
        .contains("LIVE_SKILL_FIRST"));
    for marker in ["LIVE_SKILL_FIRST", "LIVE_SKILL_CURRENT"] {
        assert!(!durable["loaded_skills"].to_string().contains(marker));
    }
    assert_eq!(
        durable["loaded_skills"],
        json!([{"ability_id":"web_access","kind":"selectable","skill_id":"live_refresh"}])
    );
    let selector = json!({"type":"skill","ability_id":"web_access","kind":"selectable","skill_id":"live_refresh"});
    let ledger = app
        .store
        .context_assets(&sid, "shared_default", app.store.current_user_id())
        .await
        .unwrap();
    assert_eq!(ledger.selectors, vec![selector]);
    let reopened = Box::pin(Store::open(root.path())).await.unwrap();
    let reopened = Box::pin(reopened.for_user(app.store.current_user_id()))
        .await
        .unwrap();
    let persisted = reopened
        .context_assets(&sid, "shared_default", app.store.current_user_id())
        .await
        .unwrap();
    assert_eq!(persisted, ledger);
    let (messages, _) = app.store.chat_snapshot(&sid).await.unwrap();
    assert_eq!(
        messages
            .iter()
            .filter(|message| message["role"] == "assistant")
            .count(),
        1
    );
    server.abort();
}

// Native catalog journeys use isolated storage and exact dispatch, without a
// Decision model or external worker; grants remain independent of discovery.
pub(crate) async fn visibility_fixture_base_access(app: &App) {
    visibility_fixture_ability_access(app, "base").await;
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["visibility"]["codebase_admin"] = json!("loaded");
    app.store
        .save_agent_abilities(&visibility, revision)
        .await
        .unwrap();
}
/// Grant one ability to the admin on shared_default and make it and its skills visible.
pub(crate) async fn visibility_fixture_ability_access(app: &App, id: &str) {
    let mut level = app
        .store
        .live_access("admin", "shared_default")
        .await
        .unwrap()
        .capability_level;
    level
        .ability_permissions
        .insert(id.into(), access::Permission::Auto);
    app.store
        .save_capability_level(
            "shared_default",
            level,
            app.store
                .capability_levels("shared_default")
                .await
                .unwrap()
                .revision,
        )
        .await
        .unwrap();
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["visibility"][id] = json!("loaded");
    visibility["skill_visibility"][id] = json!("loaded");
    app.store
        .save_agent_abilities(&visibility, revision)
        .await
        .unwrap();
}
pub(crate) async fn visibility_native_call(app: &App, sid: &str, id: &str, name: &str, args: Value) -> Value {
    let mut run = app.run(id).await.unwrap();
    run["pending"] = json!({"name":name,"call_id":uuid(),"arguments":args});
    let policy = workflow::Policy::resolve(None).unwrap();
    workflow::record(
        &mut run,
        &policy,
        &checkpoint_pre_tool(name),
        workflow::Decision::Pass,
    )
    .unwrap();
    app.save(&mut run).await.unwrap();
    assert!(tools::core_skills::owns(name));
    let result = tools::core_skills::execute(app, sid, id, name, &args, CancellationToken::new())
        .await
        .unwrap();
    let mut run = app.run(id).await.unwrap();
    append_tool(&mut run, &result).unwrap();
    app.save(&mut run).await.unwrap();
    result
}
/// Synthetic peers distinguish the protected host's existing protocol from worker protocol 2.
pub(crate) fn test_rpc_reply(request: &Value, mut reply: Value) -> Value {
    if request["version"] == 2 {
        let id = reply.as_object_mut().unwrap().remove("id").unwrap();
        json!({"version":2,"id":id,"result":reply})
    } else {
        reply
    }
}

/// Acknowledge only normal resource-shutdown controls while the effect peer is held.
pub(crate) async fn late_single_control_reply(stream: tokio::net::UnixStream) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let request: Value = serde_json::from_str(&line).unwrap();
    assert!(matches!(
        request["name"].as_str(),
        Some("run_stop" | "administrator_host_revoke")
    ));
    let reply = test_rpc_reply(
        &request,
        json!({"id":request["id"],"status":"success","output":{}}),
    );
    reader
        .get_mut()
        .write_all(format!("{reply}\n").as_bytes())
        .await
        .unwrap();
}

/// Hold a real authorized worker request across the normal API control boundary.
/// API and loop processes have separate active-token maps; closing the peer then
/// produces a deterministic late transport error instead of a timing-dependent cancel.
pub(crate) async fn late_single_tool_case(name: &str, action: &str, mutation: &str, success: bool) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (root, app) = fixture().await;
    let sid = conversation(&app).await;
    let _ = session_mode_set(
        State(app.clone()),
        Path(sid.clone()),
        Json(SessionModeBody {
            mode: "auto".into(),
            expected_revision: Some(0),
            confirm_write: true,
        }),
    )
    .await
    .unwrap();
    activate_codebase(&app, &sid).await;
    if name == "build_work" {
        set_tool_permission(&app, "build_work", access::Permission::Auto).await;
        let revision = app
            .store
            .capability_levels("shared_default")
            .await
            .unwrap()
            .revision;
        let mut level = app
            .store
            .live_access("admin", "shared_default")
            .await
            .unwrap()
            .capability_level;
        level
            .ability_permissions
            .insert("codebase_admin".into(), access::Permission::Auto);
        app.store
            .save_capability_level("shared_default", level, revision)
            .await
            .unwrap();
    }
    let id = queued(&app, &sid).await;
    let args = if name == "build_work" {
        json!({"action":"finish","task_id":"late-build-fixture","expected_inputs_sha256":"0".repeat(64)})
    } else if name == "file_write" {
        json!({"path":format!("/workspaces/{sid}/fixture.txt"),"content":"fixture"})
    } else {
        json!({"path":"."})
    };
    let mut run = app.run(&id).await.unwrap();
    run["phase"] = json!("tools");
    run["turns"] = json!(1);
    run["loaded_abilities"] = json!(["codebase_admin"]);
    let call = json!({"type":"function_call","call_id":"late-call","name":name,"arguments":args.to_string()});
    run["calls"] = json!([call.clone()]);
    run["input"].as_array_mut().unwrap().push(call.clone());
    app.store
        .save_run_observed(&mut run, "model_output", &json!({"output":[call]}))
        .await
        .unwrap();
    running(&app, &id, "late-generation").await;
    if name == "build_work" {
        let before = tools::administrator::host_fingerprint(&app, &sid, &id, "build_work")
            .await
            .unwrap();
        // Loading an unrelated ability mid-turn keeps a running build's lease.
        let mut other = app.run(&id).await.unwrap();
        other["loaded_abilities"] = json!(["codebase_admin", "diagnostics"]);
        app.save(&mut other).await.unwrap();
        let after = tools::administrator::host_fingerprint(&app, &sid, &id, "build_work")
            .await
            .unwrap();
        assert_eq!(before, after);
        tools::administrator::host_fingerprint(&app, &sid, &id, "build_work_finish")
            .await
            .unwrap();
    }
    let socket = if name == "build_work" {
        &app.build_socket
    } else {
        &app.tool_socket
    };
    let listener = tokio::net::UnixListener::bind(socket).unwrap();
    let (sent, received) = tokio::sync::oneshot::channel();
    let (release, held) = tokio::sync::oneshot::channel();
    let worker = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        sent.send(request.clone()).unwrap();
        let mut held = held;
        loop {
            tokio::select! {
                released = &mut held => { released.unwrap(); break; },
                accepted = listener.accept() => {
                    late_single_control_reply(accepted.unwrap().0).await;
                }
            }
        }
        if success {
            let reply = json!({"version":2,"id":request["id"],"result":{"status":"success","output":"known effect"}});
            reader
                .get_mut()
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .unwrap();
        }
    });
    let loop_app = app.clone();
    let loop_id = id.clone();
    let task = tokio::spawn(async move {
        execute_run(
            loop_app,
            loop_id,
            "late-generation".into(),
            CancellationToken::new(),
        )
        .await
    });
    let request = tokio::time::timeout(TEST_TIMEOUT, received)
        .await
        .unwrap()
        .unwrap();
    let executing = app.run(&id).await.unwrap();
    assert_eq!(executing["pending"]["state"], "executing");
    let _ = command(
        State(app.clone()),
        Path((id.clone(), action.into())),
        Json(json!({"client_request_id":"late-control"})),
    )
    .await
    .unwrap();
    let mut before = app.run(&id).await.unwrap();
    if mutation == "generation" {
        before["generation"] = json!("replacement-generation");
    }
    if mutation == "pending" {
        before["pending"]["id"] = json!("replacement-pending");
    }
    if mutation == "requested" {
        before["pending"]["state"] = json!("requested");
    }
    if mutation != "none" {
        app.store.put(&key("run", &id), &before).await.unwrap();
    }
    release.send(()).unwrap();
    tokio::time::timeout(TEST_TIMEOUT, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    worker.await.unwrap();
    let after = app.run(&id).await.unwrap();
    if mutation != "none" {
        assert_eq!(
            after, before,
            "stale attempt must not mutate the replacement checkpoint"
        );
    } else if success {
        assert_eq!(after["status"], "cancelled");
        assert_eq!(after["uncertain"], false);
        assert!(after["pending"].is_null());
        assert_eq!(after["call_index"], 1);
        // A finished run keeps no input copy; its result lives in the session history.
        assert_eq!(after["input"], json!([]));
        assert_eq!(
            app.store
                .context(&sid)
                .await
                .unwrap()
                .iter()
                .filter(|v| v["type"] == "function_call_output")
                .count(),
            1
        );
    } else {
        let failure = &after["pending"]["failure"];
        assert_eq!(failure["code"], "disconnected_response");
        assert_eq!(failure["stage"], "response_read");
        assert_eq!(failure["delivery"], "possibly_sent");
        assert_eq!(failure["transport_request_id"], request["id"]);
        assert_eq!(failure["pending_id"], executing["pending"]["id"]);
        assert_eq!(failure["call_id"], "late-call");
        assert_eq!(failure["run_id"], id);
        assert_eq!(failure["session_id"], sid);
        assert!(failure["elapsed_ms"].is_u64());
        assert_eq!(&after["latest_tool_failure"], failure);
        let mut expected = before.clone();
        expected["pending"]["failure"] = failure.clone();
        expected["latest_tool_failure"] = failure.clone();
        expected["run_revision"] = after["run_revision"].clone();
        if name == "build_work" {
            assert_eq!(after["status"], "stopped");
            assert_eq!(after["uncertain"], false);
            assert_eq!(after["pending"]["external_job"]["kind"], "v3_build");
            assert_eq!(
                after["pending"]["external_job"]["task_id"],
                "late-build-fixture"
            );
            assert_eq!(after["call_index"], 0);
            expected["status"] = json!("stopped");
            expected["uncertain"] = json!(false);
            expected["pending"]["external_job"] = after["pending"]["external_job"].clone();
            expected["updated_at"] = after["updated_at"].clone();
        }
        assert_eq!(
            after, expected,
            "late evidence cannot alter control, inputs, approval or retry state"
        );
        let reopened = Store::open(root.path()).await.unwrap();
        let durable = reopened.get(&key("run", &id)).await.unwrap().unwrap();
        assert_eq!(durable["pending"]["failure"], *failure);
        assert_eq!(public_run(&durable)["tool_failure"], *failure);
    }
    let (chat, _) = app.store.chat_snapshot(&sid).await.unwrap();
    assert_eq!(chat.iter().filter(|v| v["role"] == "assistant").count(), 0);
}

/// Run one Auto-mode turn in which the agent calls the critical tool `save_own_skill`, with the
/// critical tool guard enforced and the System model (the guard's model here, since the
/// fixture assigns no Decision model) answering `verdict`. Returns the run and the model
/// requests the guard made.
pub(crate) async fn guarded_turn(verdict: &'static str) -> (App, Value, Vec<Value>) {
    use axum::routing::post;

    let (_root, mut app) = fixture().await;
    let mut settings = app.store.agent_abilities().await.unwrap();
    let revision = settings["revision"].as_u64().unwrap();
    if !settings["settings"].is_object() {
        settings["settings"] = json!({});
    }
    settings["settings"]["task_progress"] = json!({"critical_guard":"enforce"});
    app.store
        .save_agent_abilities(&settings, revision)
        .await
        .unwrap();
    let args = json!({"name":"weekly_report","expected_revision":0,
        "description":"How I write the weekly report","instructions":"Keep it short."});
    let call = json!({"type":"function_call","call_id":"skill-call",
        "name":"save_own_skill","arguments":args.to_string()});
    let guard_requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let agent_calls = Arc::new(Mutex::new(0usize));
    let (captured, calls) = (guard_requests.clone(), agent_calls.clone());
    let mock = Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let (captured, calls, call) = (captured.clone(), calls.clone(), call.clone());
            async move {
                let output = if body.to_string().contains("You check one action") {
                    captured.lock().await.push(body);
                    json!([{"type":"message","role":"assistant",
                        "content":[{"type":"output_text","text":verdict}]}])
                } else {
                    let mut calls = calls.lock().await;
                    *calls += 1;
                    if *calls == 1 {
                        json!([call])
                    } else {
                        json!([{"type":"message","role":"assistant",
                            "content":[{"type":"output_text","text":"Saved."}]}])
                    }
                };
                let event = json!({"type":"response.completed",
                    "response":{"status":"completed","output":output}});
                (
                    [("content-type", "text/event-stream")],
                    format!("data: {event}\n\n"),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    let sid = conversation(&app).await;
    session_mode_set(
        State(app.clone()),
        Path(sid.clone()),
        Json(SessionModeBody {
            mode: "auto".into(),
            expected_revision: Some(
                session_mode(&app, &sid).await.unwrap()["revision"]
                    .as_u64()
                    .unwrap(),
            ),
            confirm_write: true,
        }),
    )
    .await
    .unwrap();
    let id = queued(&app, &sid).await;
    let mut tx = app.store.pool.begin().await.unwrap();
    app.store
        .record_task_request(&mut tx, &sid, &format!("user:{id}"), "hello")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    running(&app, &id, "guard-first").await;
    execute_run(
        app.clone(),
        id.clone(),
        "guard-first".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let run = app.run(&id).await.unwrap();
    let requests = guard_requests.lock().await.clone();
    server.abort();
    (app, run, requests)
}
