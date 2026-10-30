//! Runtime acceptance tests using isolated encrypted storage and a local provider fixture.
use super::*;
use axum::body::Body;
use axum::extract::State as AxumState;
use futures_util::StreamExt;
use tempfile::TempDir;
use tower::ServiceExt;

/// Build native catalog metadata without requiring independently compiled executable artifacts.
/// Keep the fixed Automation host declaration; generic isolated packages remain excluded.
pub(crate) fn native_catalog() -> abilities::Catalog {
    static ROOT: std::sync::OnceLock<TempDir> = std::sync::OnceLock::new();
    let root = ROOT.get_or_init(|| {
        let target = tempfile::tempdir().expect("native catalog fixture");
        /// Copy native declarations and their confined inputs; validate fixed host contracts through Catalog::load.
        fn copy_native(source: &std::path::Path, target: &std::path::Path) {
            let declaration = source.join("plugin.json");
            if declaration.is_file() {
                let manifest: Value =
                    serde_json::from_slice(&std::fs::read(declaration).unwrap()).unwrap();
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
                let output = target.join(entry.file_name());
                if entry.file_type().unwrap().is_dir() {
                    copy_native(&entry.path(), &output);
                } else {
                    std::fs::copy(entry.path(), output).unwrap();
                }
            }
        }
        copy_native(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugins/abilities"),
            target.path(),
        );
        target
    });
    abilities::Catalog::load(root.path()).expect("validated native fixture catalog")
}

pub(crate) async fn fixture_uninitialized() -> (TempDir, App) {
    let root = tempfile::tempdir().unwrap();
    let control_dir = root.path().join("control");
    std::fs::create_dir(&control_dir).unwrap();
    std::fs::write(control_dir.join("dispatch.lock"), b"").unwrap();
    std::fs::write(control_dir.join("shutoff"), b"0").unwrap();
    let store = Store::open(root.path()).await.unwrap();
    std::fs::create_dir_all(root.path().join("config")).unwrap();
    store.put("policy", &policy_default()).await.unwrap();
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
        browser_login_bridge: None,
        store,
        chat_actor: None,
        chat_access: None,
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
        promotion_socket: root.path().join("promote.sock"),
        chat_ui_config_path: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui/chat/chat_ui.json"),
        secure: false,
    };
    (root, app)
}
pub(crate) async fn fixture() -> (TempDir, App) {
    crate::tools::install_tool_defaults();
    let (root, app) = fixture_uninitialized().await;
    app.store
        .create_admin_account("admin", "fixture-only-password-hash", "fixture-generation")
        .await
        .unwrap();
    // Test setup follows the production migration, then explicitly appoints System
    // for compaction fixtures; capabilities alone never select a role.
    crate::model_selection_api::migrate_legacy_default(&app.store)
        .await
        .unwrap();
    let mut catalog = app.store.model_catalog().await.unwrap();
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
    (root, app)
}
/// Unsaved capability levels grant nothing, so fixtures save the two executable
/// adapters explicitly at every level, as an Operator would.
pub(crate) async fn grant_fixture_levels(store: &Store, agent: &str) {
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
async fn fixture_model(app: &App, model: &str) {
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

fn checkpoint_pre_tool(name: &str) -> workflow::Input {
    workflow::Input::PreTool(workflow::PreToolInput {
        name: name.into(),
        arguments_digest: "fixture-digest".into(),
        policy_version: 1,
        mode_revision: 0,
        visibility_revision: 0,
        ability_state_revision: 0,
    })
}

#[tokio::test]
async fn chat_ui_config_reads_data_config_on_each_request_and_requires_auth() {
    let (root, mut app) = fixture().await;
    app.chat_ui_config_path = root.path().join("config/chat.json");
    std::fs::create_dir_all(app.chat_ui_config_path.parent().unwrap()).unwrap();
    std::fs::write(
        &app.chat_ui_config_path,
        r#"{"chat_common":{"messages":{"pill_placeholder":"First"}}}"#,
    )
    .unwrap();
    let request = || {
        Request::builder()
            .uri("/api/v3/auth/ui-config")
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        router(app.clone())
            .oneshot(request())
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let token = uuid();
    app.store
        .put(
            &format!("auth:{}", digest(&json!(token))),
            &json!({"csrf":"fixture","expires":now()+3600,"generation":"fixture-generation"}),
        )
        .await
        .unwrap();
    for expected in ["First", "Updated"] {
        if expected == "Updated" {
            std::fs::write(
                &app.chat_ui_config_path,
                r#"{"chat_common":{"messages":{"pill_placeholder":"Updated"}}}"#,
            )
            .unwrap();
        }
        let response = router(app.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/v3/auth/ui-config")
                    .header("cookie", format!("v3_session={token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let config: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            config["chat_ui"]["chat_common"]["messages"]["pill_placeholder"],
            expected
        );
    }
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

#[tokio::test]
async fn agent_ability_off_is_persisted_and_conflicts_on_stale_revision() {
    let (_root, app) = fixture().await;
    let initial = agent_ability_catalog(State(app.clone()), Path("shared_default".into()))
        .await
        .unwrap()
        .0;
    assert_eq!(initial["revision"], 0);
    let changed = agent_ability_visibility_set(
        State(app.clone()),
        Path(("shared_default".into(), "codebase_admin".into())),
        Json(AbilityVisibilityBody {
            visibility: "off".into(),
            expected_revision: 0,
        }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(changed["revision"], 1);
    assert_eq!(changed["visibility"]["codebase_admin"], "off");
    let catalog = agent_ability_catalog(State(app.clone()), Path("shared_default".into()))
        .await
        .unwrap()
        .0;
    assert_eq!(catalog["abilities"][0]["visibility"], "off");
    assert_eq!(app.store.agent_abilities().await.unwrap(), changed);
    let stale = agent_ability_visibility_set(
        State(app),
        Path(("shared_default".into(), "codebase_admin".into())),
        Json(AbilityVisibilityBody {
            visibility: "visible".into(),
            expected_revision: 0,
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(stale.0, StatusCode::CONFLICT);
}

#[tokio::test]
async fn agent_tool_off_revokes_schema_and_survives_reopen() {
    let (root, app) = fixture().await;
    let result = agent_tool_mode_set(
        State(app.clone()),
        Path((
            "shared_default".into(),
            "codebase_admin".into(),
            "file_read".into(),
        )),
        Json(AbilityVisibilityBody {
            visibility: "off".into(),
            expected_revision: 0,
        }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(result["tool_modes"]["file_read"], "off");
    let state = json!({"active":["codebase_admin"],"suppressed":[]});
    let policy = policy_default();
    assert!(!app.abilities.tool_available(
        "file_read",
        &result,
        &state,
        &json!(["codebase_admin"])
    ));
    assert!(!app
        .abilities
        .schemas_with(&policy, &json!(["codebase_admin"]), &result, &state)
        .iter()
        .any(|schema| schema["name"] == "file_read"));
    assert_eq!(
        app.store.agent_abilities().await.unwrap()["tool_modes"]["file_read"],
        "off"
    );
    app.store.app_pool.close().await;
    app.store.pool.close().await;
    app.store.agent_pool.close().await;
    let reopened = Store::open(root.path()).await.unwrap();
    assert_eq!(
        reopened.agent_abilities().await.unwrap()["tool_modes"]["file_read"],
        "off"
    );
}

#[tokio::test]
async fn discoverable_tool_requires_its_own_selection() {
    let (_root, app) = fixture().await;
    let result = agent_tool_mode_set(
        State(app.clone()),
        Path((
            "shared_default".into(),
            "codebase_admin".into(),
            "file_read".into(),
        )),
        Json(AbilityVisibilityBody {
            visibility: "discoverable".into(),
            expected_revision: 0,
        }),
    )
    .await
    .unwrap()
    .0;
    let state = json!({"active":["codebase_admin"],"suppressed":[]});
    assert!(!app
        .abilities
        .tool_available("file_read", &result, &state, &json!([])));
    assert!(!app.abilities.tool_available(
        "file_read",
        &result,
        &state,
        &json!(["codebase_admin"])
    ));
    assert!(app
        .abilities
        .tool_available("file_read", &result, &state, &json!(["tool:file_read"])));
}

#[tokio::test]
async fn default_agent_model_is_saved_independently_of_global_settings() {
    let (_root, app) = fixture().await;
    let initial = agent_model(State(app.clone()), Path("shared_default".into()))
        .await
        .unwrap()
        .0;
    assert_eq!(initial["model"], DEFAULT_AGENT_MODEL);
    let saved = agent_model_set(
        State(app.clone()),
        Path("shared_default".into()),
        Json(json!({"model":"gpt-6-sol"})),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(saved["provider"], "openai_subscription");
    assert_eq!(saved["model"], "gpt-6-sol");
    assert_eq!(
        app.store.get("settings").await.unwrap().unwrap()["model"],
        "fixture"
    );
    assert_eq!(
        agent_model(State(app.clone()), Path("shared_default".into()))
            .await
            .unwrap()
            .0["model"],
        "gpt-6-sol"
    );
    assert_eq!(
        agent_model(State(app), Path("unknown".into()))
            .await
            .unwrap_err()
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn old_default_model_is_migrated_once_and_new_choices_are_preserved() {
    let (_root, app) = fixture().await;
    app.store
        .save_agent_model(&json!({"provider":"openai_subscription","model":"gpt-5.4"}))
        .await
        .unwrap();
    migrate_default_agent_model(&app.store).await.unwrap();
    assert_eq!(
        app.store.agent_model().await.unwrap()["model"],
        DEFAULT_AGENT_MODEL
    );
    app.store
        .save_agent_model(&json!({"provider":"openai_subscription","model":"gpt-6-sol"}))
        .await
        .unwrap();
    migrate_default_agent_model(&app.store).await.unwrap();
    assert_eq!(app.store.agent_model().await.unwrap()["model"], "gpt-6-sol");
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

async fn running(app: &App, id: &str, generation: &str) {
    let mut run = app.run(id).await.unwrap();
    assert!(app
        .store
        .claim_run(&mut run, "fixture-loop", generation)
        .await
        .unwrap());
}

// Manual multi-turn journeys need the same fenced lease renewal as the production loop.
async fn execute_fixture_journey(
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

#[tokio::test]
async fn message_receipts_are_idempotent_and_conflicting_reuse_is_rejected() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let body = json!({"client_message_id":"same","text":"hello"});
    let first = send(State(app.clone()), Path(sid.clone()), Json(body.clone()))
        .await
        .unwrap();
    let second = send(State(app.clone()), Path(sid.clone()), Json(body))
        .await
        .unwrap();
    assert_eq!(first.0, StatusCode::ACCEPTED);
    assert_eq!(first.1 .0, second.1 .0);
    let error = send(
        State(app.clone()),
        Path(sid.clone()),
        Json(json!({"client_message_id":"same","text":"different"})),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, StatusCode::CONFLICT);
    let (chat, _) = app.store.chat_snapshot(&sid).await.unwrap();
    assert_eq!(chat.len(), 1);
    assert_eq!(chat[0]["role"], "user");
    assert_eq!(chat[0]["content"], "hello");
    assert_eq!(app.store.context(&sid).await.unwrap().len(), 1);
    assert_eq!(app.store.list("run:").await.unwrap().len(), 1);
}

#[tokio::test]
async fn agent_proposal_is_visible_and_admin_can_promote_the_same_review() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let rid = uuid();
    let baseline = "b".repeat(40);
    let expected = hex::encode(Sha256::digest(format!("{baseline}\0patch").as_bytes()));
    let worker_listener = UnixListener::bind(&app.build_socket).unwrap();
    let review_digest = expected.clone();
    let worker = tokio::spawn(async move {
        for _ in 0..2 {
            let (stream, _) = worker_listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let call: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(call["name"], "source_review");
            let reply = json!({"id":call["id"],"status":"success","truncated":false,
                "review_sha256":review_digest,"baseline_commit":baseline,
                "status_output":" M src/main.rs","diff":"patch"});
            let reply = test_rpc_reply(&call, reply);
            let reply = if call["name"] == "source_review" {
                json!({"id":reply["id"],"status":"success","output":reply})
            } else {
                reply
            };
            reader
                .get_mut()
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .unwrap();
        }
    });
    let proposal = propose_promotion(
        &app,
        &sid,
        &rid,
        "run-test",
        &json!({"expected_review_sha256":expected,"summary":"Verified Rust change"}),
        1,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(proposal["status"], "success");
    assert_eq!(proposal["state"], "proposed");
    assert_eq!(app.store.list("promotion:").await.unwrap().len(), 1);
    let record = app
        .store
        .get(&format!("promotion:{sid}:{rid}"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record["patch"], "patch");
    assert_eq!(record["proposed_by_run"], "run-test");
    let repeated = propose_promotion(
        &app,
        &sid,
        &rid,
        "run-test",
        &json!({"expected_review_sha256":expected,"summary":"Verified Rust change"}),
        1,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(repeated["state"], "proposed");
    assert_eq!(app.store.list("promotion:").await.unwrap().len(), 1);

    let token = uuid();
    app.store
        .put(
            &format!("auth:{}", digest(&json!(token))),
            &json!({"csrf":"fixture","expires":now()+3600,"generation":"fixture-generation"}),
        )
        .await
        .unwrap();
    let cookie = format!("v3_session={token}");
    let listing = Request::builder()
        .uri(format!("/api/v3/sessions/{sid}/promotions"))
        .header("cookie", &cookie)
        .body(Body::empty())
        .unwrap();
    let response = router(app.clone()).oneshot(listing).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let listed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(listed["promotions"][0]["state"], "proposed");
    assert!(listed["promotions"][0].get("patch").is_none());

    let review = Request::builder()
        .uri(format!("/api/v3/sessions/{sid}/promotions/{rid}/review"))
        .header("cookie", &cookie)
        .body(Body::empty())
        .unwrap();
    let response = router(app.clone()).oneshot(review).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let reviewed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(reviewed["diff"], "patch");
    assert_eq!(reviewed["promotion"]["summary"], "Verified Rust change");

    let broker_listener = UnixListener::bind(&app.promotion_socket).unwrap();
    let broker = tokio::spawn(async move {
        let (stream, _) = broker_listener.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let call: Value = serde_json::from_str(&line).unwrap();
        let reply = json!({"id":call["id"],"status":"success",
            "patch_sha256":call["patch_sha256"],"paths":["src/main.rs"]});
        let reply = test_rpc_reply(&call, reply);
        let reply = if call["name"] == "source_review" {
            json!({"id":reply["id"],"status":"success","output":reply})
        } else {
            reply
        };
        reader
            .get_mut()
            .write_all(format!("{reply}\n").as_bytes())
            .await
            .unwrap();
    });
    let payload = json!({"client_request_id":rid,"expected_review_sha256":expected});
    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/v3/sessions/{sid}/promotions"))
        .header("cookie", &cookie)
        .header("x-csrf-token", "fixture")
        .header("content-type", "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap();
    let response = router(app.clone()).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let promoted: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(promoted["state"], "applied");
    assert_eq!(promoted["summary"], "Verified Rust change");
    worker.await.unwrap();
    broker.await.unwrap();
    let events = app.store.events(&sid, 0).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e["kind"] == "promotion_proposed")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e["kind"] == "promotion_requested")
            .count(),
        1
    );
}

#[tokio::test]
async fn proposal_respects_policy_and_rejects_stale_review() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let rid = uuid();
    let expected = "a".repeat(64);
    let args = json!({"expected_review_sha256":expected,"summary":"Test"});
    let mut policy = policy_default();
    policy["rules"]["promotion_propose"] = json!("deny");
    app.store.put("policy", &policy).await.unwrap();
    let denied = propose_promotion(
        &app,
        &sid,
        &rid,
        "run-test",
        &args,
        1,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(denied["status"], "denied");
    assert!(app.store.list("promotion:").await.unwrap().is_empty());

    app.store.put("policy", &policy_default()).await.unwrap();
    let listener = UnixListener::bind(&app.build_socket).unwrap();
    let worker = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let call: Value = serde_json::from_str(&line).unwrap();
        let reply = json!({"id":call["id"],"status":"success","truncated":false,
            "review_sha256":"b".repeat(64),"baseline_commit":"c".repeat(40),
            "diff":"patch"});
        let reply = test_rpc_reply(&call, reply);
        let reply = if call["name"] == "source_review" {
            json!({"id":reply["id"],"status":"success","output":reply})
        } else {
            reply
        };
        reader
            .get_mut()
            .write_all(format!("{reply}\n").as_bytes())
            .await
            .unwrap();
    });
    let stale = propose_promotion(
        &app,
        &sid,
        &rid,
        "run-test",
        &args,
        1,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(stale["status"], "denied");
    assert!(app.store.list("promotion:").await.unwrap().is_empty());
    worker.await.unwrap();
}

#[tokio::test]
async fn promotion_requires_auth_and_csrf_and_reuses_durable_receipt() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let uri = format!("/api/v3/sessions/{sid}/promotions");
    let request_id = uuid();
    let baseline = "b".repeat(40);
    let expected = hex::encode(Sha256::digest(format!("{baseline}\0patch").as_bytes()));
    let payload = json!({"client_request_id":request_id,"expected_review_sha256":expected});
    let request = |cookie: Option<&str>, csrf: Option<&str>| {
        let mut builder = Request::builder()
            .method("POST")
            .uri(&uri)
            .header("content-type", "application/json");
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", cookie);
        }
        if let Some(csrf) = csrf {
            builder = builder.header("x-csrf-token", csrf);
        }
        builder.body(Body::from(payload.to_string())).unwrap()
    };
    assert_eq!(
        router(app.clone())
            .oneshot(request(None, None))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let token = uuid();
    app.store
        .put(
            &format!("auth:{}", digest(&json!(token))),
            &json!({"csrf":"fixture","expires":now()+3600,"generation":"fixture-generation"}),
        )
        .await
        .unwrap();
    let cookie = format!("v3_session={token}");
    assert_eq!(
        router(app.clone())
            .oneshot(request(Some(&cookie), None))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );

    let worker_listener = UnixListener::bind(&app.build_socket).unwrap();
    let broker_listener = UnixListener::bind(&app.promotion_socket).unwrap();
    let review_digest = expected.clone();
    let worker = tokio::spawn(async move {
        let (stream, _) = worker_listener.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let call: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(call["name"], "source_review");
        let reply = json!({"id":call["id"],"status":"success","truncated":false,
            "review_sha256":review_digest,"baseline_commit":baseline,
            "diff":"patch"});
        let reply = test_rpc_reply(&call, reply);
        let reply = if call["name"] == "source_review" {
            json!({"id":reply["id"],"status":"success","output":reply})
        } else {
            reply
        };
        reader
            .get_mut()
            .write_all(format!("{reply}\n").as_bytes())
            .await
            .unwrap();
    });
    let broker = tokio::spawn(async move {
        let (stream, _) = broker_listener.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let call: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(call["patch"], "patch");
        let reply = json!({"id":call["id"],"status":"success",
            "patch_sha256":call["patch_sha256"],"paths":["src/main.rs"]});
        let reply = test_rpc_reply(&call, reply);
        let reply = if call["name"] == "source_review" {
            json!({"id":reply["id"],"status":"success","output":reply})
        } else {
            reply
        };
        reader
            .get_mut()
            .write_all(format!("{reply}\n").as_bytes())
            .await
            .unwrap();
    });
    let response = router(app.clone())
        .oneshot(request(Some(&cookie), Some("fixture")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["state"], "applied");
    assert_eq!(value["paths"][0], "src/main.rs");
    worker.await.unwrap();
    broker.await.unwrap();

    let retry = tokio::time::timeout(
        Duration::from_secs(2),
        router(app.clone()).oneshot(request(Some(&cookie), Some("fixture"))),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(retry.status(), StatusCode::OK);
    let body = axum::body::to_bytes(retry.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let repeated: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(repeated, value);
    assert_eq!(app.store.list("promotion:").await.unwrap().len(), 1);
    let events = app.store.events(&sid, 0).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e["kind"] == "promotion_requested")
            .count(),
        1
    );
}

#[tokio::test]
async fn admin_cannot_promote_a_stale_agent_proposal() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let rid = uuid();
    let baseline = "b".repeat(40);
    let expected = hex::encode(Sha256::digest(format!("{baseline}\0old").as_bytes()));
    let body = json!({"client_request_id":rid,"expected_review_sha256":expected});
    app.store
        .put(
            &format!("promotion:{sid}:{rid}"),
            &json!({"session_id":sid,"client_request_id":rid,"state":"proposed",
            "request_digest":digest(&body),"review_sha256":expected,
            "patch_sha256":hex::encode(Sha256::digest(b"old")),
            "patch":"old","created_at":now()}),
        )
        .await
        .unwrap();
    let listener = UnixListener::bind(&app.build_socket).unwrap();
    let worker = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let call: Value = serde_json::from_str(&line).unwrap();
        let changed = hex::encode(Sha256::digest(format!("{baseline}\0new").as_bytes()));
        let reply = json!({"id":call["id"],"status":"success","truncated":false,
            "review_sha256":changed,"baseline_commit":baseline,"diff":"new"});
        let reply = test_rpc_reply(&call, reply);
        let reply = if call["name"] == "source_review" {
            json!({"id":reply["id"],"status":"success","output":reply})
        } else {
            reply
        };
        reader
            .get_mut()
            .write_all(format!("{reply}\n").as_bytes())
            .await
            .unwrap();
    });
    let error = promote(State(app.clone()), Path(sid.clone()), Json(body))
        .await
        .unwrap_err();
    assert_eq!(error.0, StatusCode::CONFLICT);
    assert_eq!(
        app.store
            .get(&format!("promotion:{sid}:{rid}"))
            .await
            .unwrap()
            .unwrap()["state"],
        "proposed"
    );
    worker.await.unwrap();
}

#[tokio::test]
async fn admin_rejects_proposal_once_and_csrf_is_required() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let rid = uuid();
    app.store
        .put(
            &format!("promotion:{sid}:{rid}"),
            &json!({"session_id":sid,"client_request_id":rid,"state":"proposed",
            "patch_sha256":"digest","created_at":now()}),
        )
        .await
        .unwrap();
    let token = uuid();
    app.store
        .put(
            &format!("auth:{}", digest(&json!(token))),
            &json!({"csrf":"fixture","expires":now()+3600,"generation":"fixture-generation"}),
        )
        .await
        .unwrap();
    let cookie = format!("v3_session={token}");
    let uri = format!("/api/v3/sessions/{sid}/promotions/{rid}/reject");
    let request = |csrf: Option<&str>| {
        let mut builder = Request::builder()
            .method("POST")
            .uri(&uri)
            .header("cookie", &cookie);
        if let Some(csrf) = csrf {
            builder = builder.header("x-csrf-token", csrf);
        }
        builder.body(Body::empty()).unwrap()
    };
    let denied = router(app.clone()).oneshot(request(None)).await.unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    for _ in 0..2 {
        let response = router(app.clone())
            .oneshot(request(Some("fixture")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["state"], "rejected");
    }
    let events = app.store.events(&sid, 0).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e["kind"] == "promotion_rejected")
            .count(),
        1
    );
    assert_eq!(
        app.store
            .get(&format!("promotion:{sid}:{rid}"))
            .await
            .unwrap()
            .unwrap()["state"],
        "rejected"
    );
}

#[tokio::test]
async fn interrupted_promotion_becomes_uncertain_without_dispatch() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let rid = uuid();
    app.store
        .put(
            &format!("promotion:{sid}:{rid}"),
            &json!({"session_id":sid,"client_request_id":rid,"state":"dispatching",
            "request_digest":"digest","patch_sha256":"patch","created_at":now()}),
        )
        .await
        .unwrap();
    recover_promotions(&app).await.unwrap();
    let record = app
        .store
        .get(&format!("promotion:{sid}:{rid}"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record["state"], "uncertain");
    recover_promotions(&app).await.unwrap();
    let events = app.store.events(&sid, 0).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e["kind"] == "promotion_recovered")
            .count(),
        1
    );
}

#[tokio::test]
async fn source_review_route_requires_auth_policy_and_existing_session() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let uri = format!("/api/v3/sessions/{sid}/source-review");
    let request = |path: &str, cookie: Option<&str>| {
        let mut builder = Request::builder().uri(path);
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", cookie);
        }
        builder.body(Body::empty()).unwrap()
    };
    let response = router(app.clone())
        .oneshot(request(&uri, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let token = uuid();
    app.store
        .put(
            &format!("auth:{}", digest(&json!(token))),
            &json!({"csrf":"fixture","expires":now()+3600,"generation":"fixture-generation"}),
        )
        .await
        .unwrap();
    let cookie = format!("v3_session={token}");
    let unknown = format!("/api/v3/sessions/{}/source-review", uuid());
    let response = router(app.clone())
        .oneshot(request(&unknown, Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let mut policy = policy_default();
    policy["rules"]["source_review"] = json!("deny");
    app.store.put("policy", &policy).await.unwrap();
    let response = router(app.clone())
        .oneshot(request(&uri, Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    policy["rules"]["source_review"] = json!("require_approval");
    app.store.put("policy", &policy).await.unwrap();
    let response = router(app.clone())
        .oneshot(request(&uri, Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    app.store.put("policy", &policy_default()).await.unwrap();
    app.store
        .put("control", &json!({"shutoff":true}))
        .await
        .unwrap();
    let response = router(app.clone())
        .oneshot(request(&uri, Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    app.store
        .put("control", &json!({"shutoff":false}))
        .await
        .unwrap();
    let listener = UnixListener::bind(&app.build_socket).unwrap();
    let expected_sid = sid.clone();
    let worker = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let call: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(call["name"], "source_review");
        assert_eq!(call["workspace"], expected_sid);
        let reply = json!({"id":call["id"],"status":"success",
            "status_output":" M src/main.rs","diff":"patch","truncated":false});
        let reply = test_rpc_reply(&call, reply);
        let reply = if call["name"] == "source_review" {
            json!({"id":reply["id"],"status":"success","output":reply})
        } else {
            reply
        };
        reader
            .get_mut()
            .write_all(format!("{reply}\n").as_bytes())
            .await
            .unwrap();
    });
    let response = router(app.clone())
        .oneshot(request(&uri, Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    let review: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(review["diff"], "patch");
    worker.await.unwrap();
}

#[tokio::test]
async fn tool_reply_must_match_the_dispatched_request() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    let (_root, app) = fixture().await;
    let listener = UnixListener::bind(&app.build_socket).unwrap();
    let worker = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        reader
            .get_mut()
            .write_all(b"{\"id\":\"wrong\",\"status\":\"success\",\"diff\":\"wrong patch\"}\n")
            .await
            .unwrap();
    });
    let result = tools::administrator::workspace_host(
        &app,
        &uuid(),
        "source_review",
        &json!({}),
        None,
        CancellationToken::new(),
    )
    .await;
    assert!(result.is_err());
    worker.await.unwrap();
}

#[tokio::test]
async fn authentication_and_csrf_protect_control_routes() {
    let (_root, app) = fixture().await;
    let response = router(app.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v3/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let token = uuid();
    app.store
        .put(
            &format!("auth:{}", digest(&json!(token))),
            &json!({"csrf":"expected","expires":now()+3600,"generation":"fixture-generation"}),
        )
        .await
        .unwrap();
    let request = |csrf: &str| {
        Request::builder()
            .method("PUT")
            .uri("/api/v3/control")
            .header("cookie", format!("v3_session={token}"))
            .header("content-type", "application/json")
            .header("x-csrf-token", csrf)
            .body(Body::from(
                r#"{"client_request_id":"control","shutoff":true}"#,
            ))
            .unwrap()
    };
    let response = router(app.clone()).oneshot(request("wrong")).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(!app.shutoff().await.unwrap());
    let response = router(app.clone())
        .oneshot(request("expected"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(app.shutoff().await.unwrap());
}

#[tokio::test]
async fn changed_policy_invalidates_pending_action_approval() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let args = json!({"command":"printf hello"});
    let mut run = app.run(&id).await.unwrap();
    run["status"] = json!("waiting_approval");
    run["pending"] = json!({"id":"approval","name":"shell","arguments":args,"digest":digest(&args),
        "policy_version":1,"state":"waiting","decision":null,"call_id":"call"});
    app.save(&mut run).await.unwrap();
    let mut policy = policy_default();
    policy["version"] = json!(2);
    app.store.put("policy", &policy).await.unwrap();
    let error = approval(
        State(app.clone()),
        Path("approval".into()),
        Json(json!({"client_request_id":"decision","decision":"approve"})),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, StatusCode::CONFLICT);
    let run = app.run(&id).await.unwrap();
    assert_eq!(run["status"], "queued");
    assert!(run["pending"]["decision"].is_null());
    assert_eq!(run["pending"]["digest"], digest(&args));
}

#[tokio::test]
async fn recovery_holds_uncertain_writes_and_requeues_safe_reads() {
    let (root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    running(&app, &id, "first").await;
    let mut run = app.run(&id).await.unwrap();
    run["phase"] = json!("tools");
    run["pending"] = json!({"name":"shell","state":"executing"});
    workflow::record(
        &mut run,
        &workflow::Policy::resolve(None).unwrap(),
        &checkpoint_pre_tool("shell"),
        workflow::Decision::Pass,
    )
    .unwrap();
    let receipt = run["workflow"]["receipts"]["pre_tool"].clone();
    app.save(&mut run).await.unwrap();
    recover(&app, true).await.unwrap();
    let run = app.run(&id).await.unwrap();
    assert_eq!(run["status"], "recovery");
    assert_eq!(run["uncertain"], true);
    assert_eq!(run["pending"]["state"], "executing");
    assert_eq!(run["workflow"]["receipts"]["pre_tool"], receipt);
    let sid2 = conversation(&app).await;
    let id2 = queued(&app, &sid2).await;
    running(&app, &id2, "second").await;
    let mut read = app.run(&id2).await.unwrap();
    read["pending"] = json!({"name":"file_read","state":"executing"});
    app.save(&mut read).await.unwrap();
    recover(&app, true).await.unwrap();
    let read = app.run(&id2).await.unwrap();
    assert_eq!(read["status"], "queued");
    assert_eq!(read["pending"]["state"], "requested");
    assert_eq!(app.run(&id).await.unwrap()["status"], "recovery");
    let reopened = Store::open(root.path()).await.unwrap();
    let durable = reopened.get(&key("run", &id)).await.unwrap().unwrap();
    assert_eq!(durable["workflow"]["receipts"]["pre_tool"], receipt);
}

#[tokio::test]
async fn interrupted_build_finish_waits_for_receipt_without_replaying_write() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    running(&app, &id, "build-generation").await;
    let mut run = app.run(&id).await.unwrap();
    run["phase"] = json!("tools");
    run["pending"] = json!({"name":"v3_build_work","state":"executing",
        "call_id":"original-build-call","arguments":{"action":"finish",
            "task_id":"build-recovery-test","expected_inputs_sha256":"0".repeat(64)}});
    app.save(&mut run).await.unwrap();
    recover(&app, true).await.unwrap();
    let waiting = app.run(&id).await.unwrap();
    assert_eq!(waiting["status"], "waiting_external");
    assert_eq!(waiting["uncertain"], false);
    assert_eq!(waiting["pending"]["call_id"], "original-build-call");
    assert_eq!(waiting["pending"]["external_job"]["kind"], "v3_build");
    recover(&app, false).await.unwrap();
    let repeated = app.run(&id).await.unwrap();
    assert_eq!(repeated["status"], "waiting_external");
    assert_eq!(repeated["pending"]["call_id"], "original-build-call");
    assert_eq!(repeated["call_index"], waiting["call_index"]);
    let mut stopped = repeated.clone();
    stopped["status"] = json!("stopped");
    app.save_override(&mut stopped).await.unwrap();
    let _ = command(
        State(app.clone()),
        Path((id.clone(), "resume".into())),
        Json(json!({"client_request_id":"resume-build-result"})),
    )
    .await
    .unwrap();
    let resumed = app.run(&id).await.unwrap();
    assert_eq!(resumed["status"], "waiting_external");
    assert_eq!(resumed["pending"]["state"], "executing");
    let result = json!({"status":"success","output":{"task_id":"build-recovery-test",
        "job_id":"existing-job","status":"completed","verified_healthy":true}});
    let mut settled = resumed.clone();
    agent_loop::attach_external_build_result(&mut settled, &result).unwrap();
    assert_eq!(settled["status"], "queued");
    assert!(agent_loop::attach_external_build_result(&mut settled, &result).is_err());
    app.store
        .save_run_override(&mut settled, "tool_result", &result)
        .await
        .unwrap();
    let durable = app.run(&id).await.unwrap();
    assert_eq!(durable["call_index"], json!(1));
    assert_eq!(
        durable["input"].as_array().unwrap().last().unwrap()["call_id"],
        "original-build-call"
    );
    assert_eq!(durable["pending"], Value::Null);
}

#[tokio::test]
async fn startup_resume_setting_stops_safe_runs_but_preserves_uncertain_writes() {
    let (_root, app) = fixture().await;
    crate::services::save_boot_orphan_resume(&app.store, "admin", false, 0)
        .await
        .unwrap();
    let sid = conversation(&app).await;
    let read_id = queued(&app, &sid).await;
    running(&app, &read_id, "read-generation").await;
    let mut read = app.run(&read_id).await.unwrap();
    read["pending"] = json!({"name":"file_read","state":"executing"});
    app.save(&mut read).await.unwrap();
    let sid2 = conversation(&app).await;
    let write_id = queued(&app, &sid2).await;
    running(&app, &write_id, "write-generation").await;
    let mut write = app.run(&write_id).await.unwrap();
    write["pending"] = json!({"name":"file_write","state":"executing"});
    app.save(&mut write).await.unwrap();
    recover(&app, true).await.unwrap();
    let stopped = app.run(&read_id).await.unwrap();
    assert_eq!(stopped["status"], "stopped");
    assert_eq!(stopped["pending"]["state"], "requested");
    assert!(stopped["error"].as_str().unwrap().contains("disabled"));
    let uncertain = app.run(&write_id).await.unwrap();
    assert_eq!(uncertain["status"], "recovery");
    assert_eq!(uncertain["uncertain"], true);
    assert_eq!(uncertain["pending"]["state"], "executing");
    // Non-startup recovery must still repair expired read leases.
    let sid3 = conversation(&app).await;
    let lease_id = queued(&app, &sid3).await;
    running(&app, &lease_id, "lease-generation").await;
    recover(&app, false).await.unwrap();
    assert_eq!(app.run(&lease_id).await.unwrap()["status"], "queued");
    // Corrupt optional configuration must not suppress mandatory classification.
    sqlx::query("UPDATE docs SET value=? WHERE key='service:boot_orphan_resume'")
        .bind(
            app.store
                .seal(&json!({"enabled":"invalid","revision":2}))
                .unwrap(),
        )
        .execute(&app.store.app_pool)
        .await
        .unwrap();
    for (name, expected) in [("file_read", "stopped"), ("file_write", "recovery")] {
        let sid = conversation(&app).await;
        let id = queued(&app, &sid).await;
        running(&app, &id, "corrupt-config-generation").await;
        let mut run = app.run(&id).await.unwrap();
        run["pending"] = json!({"name":name,"state":"executing"});
        app.save(&mut run).await.unwrap();
        recover(&app, true).await.unwrap();
        assert_eq!(app.run(&id).await.unwrap()["status"], expected);
    }
}

#[tokio::test]
async fn shutoff_survives_storage_reopen_and_clear_requires_explicit_resume() {
    let (root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    assert_eq!(
        control(
            State(app.clone()),
            Json(json!({"client_request_id":"off","shutoff":true}))
        )
        .await
        .unwrap()
        .0["shutoff"],
        true
    );
    let reopened = Store::open(root.path()).await.unwrap();
    assert_eq!(
        reopened.get("control").await.unwrap().unwrap()["shutoff"],
        true
    );
    assert_eq!(
        reopened.get(&key("run", &id)).await.unwrap().unwrap()["status"],
        "stopped"
    );
    assert_eq!(
        std::fs::read(app.control_dir.join("shutoff")).unwrap(),
        b"1"
    );
    assert_eq!(
        control(
            State(app.clone()),
            Json(json!({"client_request_id":"on","shutoff":false}))
        )
        .await
        .unwrap()
        .0["shutoff"],
        false
    );
    assert_eq!(app.run(&id).await.unwrap()["status"], "stopped");
    assert_eq!(
        command(
            State(app.clone()),
            Path((id.clone(), "resume".into())),
            Json(json!({"client_request_id":"resume"}))
        )
        .await
        .unwrap()
        .0["status"],
        "queued"
    );
    assert_eq!(app.run(&id).await.unwrap()["status"], "queued");
}

#[tokio::test]
async fn stale_generation_cannot_execute_or_commit_failure() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    running(&app, &id, "current").await;
    execute_run(
        app.clone(),
        id.clone(),
        "stale".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let run = app.run(&id).await.unwrap();
    assert_eq!(run["generation"], "current");
    assert_eq!(run["status"], "running");
    assert_eq!(run["attempt"], 0);
    assert!(run["error"].is_null());
}

#[tokio::test]
async fn agent_loop_creates_reviewable_proposal_without_dispatching_broker() {
    use axum::routing::post;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    let (_root, mut app) = fixture().await;
    // The retired proposal adapter stays covered through an explicit legacy
    // fixture. It is intentionally absent from the live Codebase Admin catalog.
    fn copy_catalog(source: &std::path::Path, target: &std::path::Path) {
        std::fs::create_dir_all(target).unwrap();
        for entry in std::fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let destination = target.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_catalog(&entry.path(), &destination);
            } else {
                std::fs::copy(entry.path(), destination).unwrap();
            }
        }
    }
    let legacy = _root.path().join("legacy-abilities");
    // Extend only the native fixture; external plugin artifacts belong to publication tests.
    copy_catalog(app.abilities.snapshot_root(), &legacy);
    let path = legacy.join("Administrator/codebase_admin/codebase_admin.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for tool in ["source_review", "promotion_propose"] {
        manifest["tools"].as_array_mut().unwrap().push(json!(tool));
    }
    std::fs::write(&path, manifest.to_string()).unwrap();
    app.abilities = Arc::new(abilities::Catalog::load(&legacy).unwrap());
    let baseline = "b".repeat(40);
    let expected = hex::encode(Sha256::digest(format!("{baseline}\0patch").as_bytes()));
    let arguments = json!({"expected_review_sha256":expected,"summary":"Verified coding change"});
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let requests = captured.clone();
    let tool_args = arguments.to_string();
    let mock = Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let requests = requests.clone();
            let tool_args = tool_args.clone();
            async move {
                let mut requests = requests.lock().await;
                requests.push(body);
                let output = if requests.len() == 1 {
                    json!([{"type":"function_call","call_id":"proposal-call",
                        "name":"promotion_propose","arguments":tool_args}])
                } else {
                    json!([{"type":"message","role":"assistant",
                        "content":[{"type":"output_text","text":"Proposal ready for review."}]}])
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

    let worker_listener = UnixListener::bind(&app.build_socket).unwrap();
    let worker_digest = expected.clone();
    let worker = tokio::spawn(async move {
        for _ in 0..4 {
            let (stream, _) = worker_listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let call: Value = serde_json::from_str(&line).unwrap();
            if call["name"] == "administrator_quiesce" {
                assert_eq!(call["arguments"], json!({}));
                let reply = json!({"id":call["id"],"status":"success"});
                let reply = test_rpc_reply(&call, reply);
                let reply = if call["name"] == "source_review" {
                    json!({"id":reply["id"],"status":"success","output":reply})
                } else {
                    reply
                };
                reader
                    .get_mut()
                    .write_all(format!("{reply}\n").as_bytes())
                    .await
                    .unwrap();
                continue;
            }
            // Only the review may reach this fixture worker. Promotion remains
            // a native proposal; no broker mutation is dispatched.
            assert_eq!(call["name"], "source_review");
            let reply = json!({"id":call["id"],"status":"success","truncated":false,
                "baseline_commit":baseline,"review_sha256":worker_digest,
                "status_output":" M src/main.rs","diff":"patch"});
            let reply = test_rpc_reply(&call, reply);
            let reply = if call["name"] == "source_review" {
                json!({"id":reply["id"],"status":"success","output":reply})
            } else {
                reply
            };
            reader
                .get_mut()
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .unwrap();
            return;
        }
        panic!("source review missing after bounded shutdown acknowledgements");
    });
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
    activate_codebase(&app, &sid).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    run["loaded_abilities"] = json!(["codebase_admin"]);
    app.save(&mut run).await.unwrap();
    running(&app, &id, "proposal-generation").await;
    execute_run(
        app.clone(),
        id.clone(),
        "proposal-generation".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(app.run(&id).await.unwrap()["status"], "completed");
    tokio::time::timeout(Duration::from_secs(10), worker)
        .await
        .expect("legacy source review must dispatch within the fixture timeout")
        .unwrap();
    let proposals = app.store.list(&format!("promotion:{sid}:")).await.unwrap();
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0]["state"], "proposed");
    assert_eq!(proposals[0]["proposed_by_run"], id);
    let requests = captured.lock().await;
    assert_eq!(requests.len(), 2);
    assert!(requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "promotion_propose"));
    server.abort();
}

#[tokio::test]
async fn automatic_code_index_retrieval_is_attempted_once_but_off_by_default() {
    use axum::routing::post;
    let (_root, mut app) = fixture().await;
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let mock = Router::new().route("/responses", post(
        |AxumState(requests): AxumState<Arc<Mutex<Vec<Value>>>>, Json(body): Json<Value>| async move {
            let mut requests=requests.lock().await;
            requests.push(body);
            let output=json!([{"type":"message","role":"assistant",
                "content":[{"type":"output_text","text":"No source retrieval was enabled."}]}]);
            let event=json!({"type":"response.completed","response":{"status":"completed","output":output}});
            ([ ("content-type","text/event-stream") ],format!("data: {event}\n\n"))
        }
    )).with_state(captured);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    running(&app, &id, "auto-index-disabled").await;
    execute_run(
        app.clone(),
        id.clone(),
        "auto-index-disabled".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let run = app.run(&id).await.unwrap();
    assert_eq!(run["status"], "completed");
    assert_eq!(run["code_index_retrieval_attempted"], true);
    assert_eq!(run["code_index_retrieval_status"], "disabled_or_no_matches");
    assert_eq!(
        run["workflow"]["receipts"]["publication"]["result"]["decision"],
        "pass"
    );
    assert!(!run["input"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["source"] == "automatic_code_index"));
    let (messages, _) = app.store.chat_snapshot(&sid).await.unwrap();
    assert_eq!(
        messages
            .iter()
            .filter(|item| item["role"] == "assistant")
            .count(),
        1
    );
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 1);
    assert!(!requests[0]["input"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["source"] == "automatic_code_index"));
    server.abort();
}

#[tokio::test]
async fn reasoning_only_completion_continues_to_one_final_publication() {
    use axum::routing::post;
    let (_root, mut app) = fixture().await;
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let mock = Router::new()
        .route(
            "/responses",
            post(
                |AxumState(requests): AxumState<Arc<Mutex<Vec<Value>>>>,
                 Json(body): Json<Value>| async move {
                    let mut requests = requests.lock().await;
                    requests.push(body);
                    let output = if requests.len() == 1 {
                        json!([{"type":"reasoning","encrypted_content":"opaque"}])
                    } else {
                        json!([{"type":"message","role":"assistant",
                    "content":[{"type":"output_text","text":"Continued reply."}]}])
                    };
                    let event = json!({"type":"response.completed",
                "response":{"status":"completed","output":output}});
                    (
                        [("content-type", "text/event-stream")],
                        format!("data: {event}\n\n"),
                    )
                },
            ),
        )
        .with_state(captured);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    running(&app, &id, "reasoning").await;
    execute_run(
        app.clone(),
        id.clone(),
        "reasoning".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let run = app.run(&id).await.unwrap();
    assert_eq!(run["status"], "completed");
    assert_eq!(run["reasoning_only_completions"], 1);
    assert_eq!(
        run["workflow"]["receipts"]["publication"]["result"]["decision"],
        "pass"
    );
    let (messages, _) = app.store.chat_snapshot(&sid).await.unwrap();
    assert_eq!(
        messages
            .iter()
            .filter(|item| item["role"] == "assistant")
            .count(),
        1
    );
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert!(requests[1]["input"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["type"] == "reasoning" && item["encrypted_content"] == "opaque"));
    server.abort();
}

#[tokio::test]
async fn image_capable_candidates_receive_sent_images_and_others_keep_text() {
    use axum::routing::post;
    let (_root, mut app) = fixture().await;
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let mock = Router::new().route("/responses", post(
        |AxumState(requests): AxumState<Arc<Mutex<Vec<Value>>>>, Json(body): Json<Value>| async move {
            requests.lock().await.push(body);
            let output = json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Seen."}]}]);
            let event = json!({"type":"response.completed","response":{"status":"completed","output":output}});
            ([("content-type","text/event-stream")], format!("data: {event}\n\n"))
        }
    )).with_state(captured);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    for (sees, generation) in [(true, "image-in"), (false, "text-only")] {
        let sid = conversation(&app).await;
        let aid = uuid();
        crate::api::upload(
            State(app.clone()),
            Path(sid.clone()),
            Json(json!({"client_upload_id":aid,
            "name":"dot.png","media_type":"image/png","data":"iVBORw0KGgo="})),
        )
        .await
        .unwrap();
        let (_, Json(sent)) = send(
            State(app.clone()),
            Path(sid.clone()),
            Json(json!({"client_message_id":uuid(),
            "text":"What is this?","attachment_ids":[aid]})),
        )
        .await
        .unwrap();
        let id = sent["run_id"].as_str().unwrap().to_owned();
        // Hydrate as the scheduler would, and mark the candidate as the plan would for an image_in entry.
        let mut run = app.run(&id).await.unwrap();
        run["input"] = json!(app.store.context(&sid).await.unwrap());
        run["context_hydration_pending"] = json!(false);
        run["selection_plan"]["candidates"][0]["image_in"] = json!(sees);
        app.store.put(&key("run", &id), &run).await.unwrap();
        running(&app, &id, generation).await;
        execute_run(
            app.clone(),
            id.clone(),
            generation.into(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(app.run(&id).await.unwrap()["status"], "completed");
        // Session naming shares the mock provider; the chat call is the one carrying this message.
        let user = requests
            .lock()
            .await
            .drain(..)
            .flat_map(|request| request["input"].as_array().cloned().unwrap_or_default())
            .find(|item| item["role"] == "user" && item.to_string().contains("What is this?"))
            .unwrap();
        let image = json!({"type":"input_image","image_url":"data:image/png;base64,iVBORw0KGgo="});
        if sees {
            assert_eq!(
                user["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|part| **part == image)
                    .count(),
                1
            );
        } else {
            assert!(user
                .to_string()
                .contains("Image description is not available"));
            assert!(!user.to_string().contains("input_image"));
        }
        // Durable context keeps text either way; the image is call-scoped.
        let context = app.store.context(&sid).await.unwrap();
        assert!(!json!(context).to_string().contains("input_image"));
        assert!(!app.run(&id).await.unwrap()["input"]
            .to_string()
            .contains("input_image"));
    }
    server.abort();
}

#[tokio::test]
async fn agent_loop_gates_tool_and_denial_continues_without_worker() {
    use axum::routing::post;
    let (_root, mut app) = fixture().await;
    assert_eq!(
        settings(State(app.clone()), Json(json!({"model":"pinned-model"})))
            .await
            .unwrap()
            .0["provider"],
        "openai_subscription"
    );
    fixture_model(&app, "pinned-model").await;
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let mock = Router::new().route("/responses", post(|AxumState(requests): AxumState<Arc<Mutex<Vec<Value>>>>, Json(body): Json<Value>| async move {
        let mut requests = requests.lock().await;
        requests.push(body);
        let output = if requests.len() == 1 {
            json!([{"type":"function_call","call_id":"call","name":"shell","arguments":"{\"command\":\"printf hello\"}"}])
        } else {
            json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Action denied."}]}])
        };
        let event = json!({"type":"response.completed","response":{"status":"completed","output":output}});
        ([("content-type","text/event-stream")], format!("data: {event}\n\n"))
    })).with_state(captured);
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
            mode: "write".into(),
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
    activate_codebase(&app, &sid).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    run["loaded_abilities"] = json!(["codebase_admin"]);
    app.save(&mut run).await.unwrap();
    running(&app, &id, "first").await;
    execute_run(
        app.clone(),
        id.clone(),
        "first".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let waiting = app.run(&id).await.unwrap();
    assert_eq!(waiting["status"], "waiting_approval");
    assert_eq!(waiting["provider_selection"]["model"], "pinned-model");
    assert_eq!(waiting["pending"]["arguments"]["command"], "printf hello");
    let approval_id = waiting["pending"]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        approval(
            State(app.clone()),
            Path(approval_id),
            Json(json!({"client_request_id":"deny","decision":"deny"}))
        )
        .await
        .unwrap()
        .0["status"],
        "queued"
    );
    running(&app, &id, "second").await;
    execute_run(
        app.clone(),
        id.clone(),
        "second".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let completed = app.run(&id).await.unwrap();
    assert_eq!(completed["status"], "completed");
    let pre_tool = &completed["workflow"]["receipts"]["pre_tool"];
    let tool_result = &completed["workflow"]["receipts"]["tool_result"];
    assert_eq!(pre_tool["result"]["decision"], "reject");
    assert_eq!(tool_result["result"]["decision"], "reject");
    assert_eq!(pre_tool["generation"], "second");
    assert_eq!(tool_result["generation"], pre_tool["generation"]);
    assert_eq!(tool_result["policy_revision"], pre_tool["policy_revision"]);
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert!(requests
        .iter()
        .all(|request| request["model"] == "pinned-model"));
    assert_eq!(
        settings(
            State(app.clone()),
            Json(json!({"model":"replacement-model","provider":"openai_subscription"}))
        )
        .await
        .unwrap()
        .0["model"],
        "replacement-model"
    );
    fixture_model(&app, "replacement-model").await;
    let next_sid = conversation(&app).await;
    let next_id = queued(&app, &next_sid).await;
    assert_eq!(
        app.run(&next_id).await.unwrap()["provider_selection"]["model"],
        "replacement-model"
    );
    let tool_output = requests[1]["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["type"] == "function_call_output")
        .unwrap();
    assert_eq!(tool_output["call_id"], "call");
    assert!(tool_output["output"].as_str().unwrap().contains("denied"));
    server.abort();
}

#[tokio::test]
/// Verify a catalog change revokes a loaded ability before its already queued tool can execute.
async fn catalog_change_revokes_loaded_ability_before_pending_tool() {
    use axum::routing::post;
    let (_root, mut app) = fixture().await;
    let mock = Router::new().route(
        "/responses",
        post(|| async {
            let event = json!({"type":"response.completed",
            "response":{"status":"completed","output":[{
                "type":"message","role":"assistant",
                "content":[{"type":"output_text","text":"Done."}]
            }]}});
            (
                [("content-type", "text/event-stream")],
                format!("data: {event}\n\n"),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    run["loaded_abilities"] = json!(["codebase_admin"]);
    run["ability_catalog_version"] = json!("old-catalog");
    run["phase"] = json!("tools");
    run["calls"] = json!([{"type":"function_call","call_id":"old-call",
        "name":"shell","arguments":"{\"command\":\"printf hello\"}"}]);
    let calls = run["calls"].as_array().unwrap().clone();
    run["input"].as_array_mut().unwrap().extend(calls);
    app.save(&mut run).await.unwrap();
    running(&app, &id, "new-catalog").await;
    execute_run(
        app.clone(),
        id.clone(),
        "new-catalog".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let finished = app.run(&id).await.unwrap();
    assert_eq!(finished["status"], "completed", "{}", finished["error"]);
    assert_eq!(finished["loaded_abilities"], json!([]));
    assert_eq!(finished["ability_catalog_version"], app.abilities.version());
    let output = finished["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    assert!(output["output"].as_str().unwrap().contains("denied"));
    server.abort();
}

/// Inspect the final call-scoped Responses developer suffix, never durable history or main instructions.
fn native_call_suffix(request: &Value) -> &str {
    let last = request["input"].as_array().unwrap().last().unwrap();
    assert_eq!(last["role"], "developer");
    let body = last["content"].as_str().unwrap();
    assert!(body.contains("Current session capability mode:"));
    body
}

/// Locate current ability guidance only in explicitly labelled Responses developer frames.
fn native_ability_frames<'a>(request: &'a Value, ability: &str) -> Vec<&'a str> {
    let header = format!("## Untrusted live guidance: ability:{ability}\n");
    request["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["role"] == "developer")
        .filter_map(|item| item["content"].as_str())
        .filter(|body| body.contains(&header))
        .collect()
}

#[tokio::test]
/// Verify compaction preserves chat history, measures the dispatched envelope, and keeps call-scoped mode guidance outside durable history.
async fn long_run_compacts_before_model_and_keeps_full_message_history() {
    use axum::routing::post;
    let (root, mut app) = fixture().await;
    let config_path = app.compaction_config_path.clone();
    let mut config = context_compaction::Config::read(&config_path).unwrap();
    config.tail_tokens = 20000;
    config.persist(&config_path).unwrap();
    let _saved = settings(State(app.clone()), Json(json!({"model":"summary-pinned"})))
        .await
        .unwrap();
    fixture_model(&app, "summary-pinned").await;
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let mock = Router::new()
        .route(
            "/responses",
            post(
                |AxumState(requests): AxumState<Arc<Mutex<Vec<Value>>>>,
                 Json(body): Json<Value>| async move {
                    let mut requests = requests.lock().await;
                    requests.push(body);
                    let text = if requests.len() == 1 {
                        "Earlier work was completed; continue the current task."
                    } else {
                        "Current task completed."
                    };
                    let event = json!({"type":"response.completed",
                    "response":{"status":"completed","output":[{
                        "type":"message","role":"assistant",
                        "content":[{"type":"output_text","text":text}]
                    }]}});
                    (
                        [("content-type", "text/event-stream")],
                        format!("data: {event}\n\n"),
                    )
                },
            ),
        )
        .with_state(captured);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    let mut input = Vec::new();
    for _ in 0..6 {
        input.push(json!({"role":"user","content":"x".repeat(50_000)}));
        input.push(json!({"type":"message","role":"assistant",
            "content":[{"type":"output_text","text":"done"}]}));
    }
    input.push(json!({"role":"user","content":"Current task"}));
    run["input"] = json!(input);
    app.save(&mut run).await.unwrap();
    let _saved = settings(
        State(app.clone()),
        Json(json!({"model":"summary-replacement"})),
    )
    .await
    .unwrap();
    running(&app, &id, "compact").await;
    execute_run(
        app.clone(),
        id.clone(),
        "compact".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    let finished = app.run(&id).await.unwrap();
    assert_eq!(finished["status"], "completed", "{}", finished["error"]);
    assert_eq!(finished["compactions"], 1);
    assert!(serde_json::to_vec(&finished["input"]).unwrap().len() <= 256 * 1024);
    let (chat, _) = app.store.chat_snapshot(&sid).await.unwrap();
    assert_eq!(chat.len(), 2);
    assert_eq!(chat[0]["content"], "hello");
    assert_eq!(chat[1]["content"], "Current task completed.");
    let context = app.store.context(&sid).await.unwrap();
    assert!(context[0]["content"]
        .as_str()
        .unwrap()
        .contains("Earlier conversation summary"));
    let events = app.store.events(&sid, 0).await.unwrap();
    assert!(events
        .iter()
        .any(|event| event["kind"] == "context_compacted"));
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert!(requests
        .iter()
        .all(|request| request["model"] == "summary-pinned"));
    assert!(requests[0]["tools"].is_null());
    // The durable report must describe the envelope actually sent after compaction.
    let report = &finished["context_assembly_report"];
    let request = &requests[1];
    let envelope = json!({"instructions":request["instructions"],
        "tools":request["tools"],"input":request["input"]});
    assert_eq!(
        report["envelope_bytes"],
        serde_json::to_vec(&envelope).unwrap().len()
    );
    assert_eq!(
        report["history_json_bytes"],
        serde_json::to_vec(&request["input"]).unwrap().len()
    );
    assert_eq!(
        report["schemas_json_bytes"],
        serde_json::to_vec(&request["tools"]).unwrap().len()
    );
    // Successful output resets attempts; the report describes the pre-dispatch call.
    assert_eq!(report["call"], 1);
    assert_eq!(
        report["turn"],
        finished["workflow"]["receipts"]["model_dispatch"]["turn"]
    );
    assert_eq!(report["transport"], "responses");
    assert_eq!(report["overlays"][0]["id"], "session_mode");
    assert_eq!(report["overlays"][0]["disposition"], "included");
    assert!(native_call_suffix(request).contains("Current session capability mode:"));
    assert!(!request["instructions"]
        .as_str()
        .unwrap()
        .contains("Current session capability mode:"));
    assert!(!report.to_string().contains("Earlier conversation summary"));
    let summaries: Vec<_> = request["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| {
            item["role"] == "user"
                && item["content"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("Earlier conversation summary"))
        })
        .collect();
    assert_eq!(summaries.len(), 1);
    assert!(!summaries[0]["content"]
        .as_str()
        .unwrap()
        .contains("Current session capability mode:"));
    assert!(!finished["input"]
        .to_string()
        .contains("Current session capability mode:"));
    assert!(!json!(context)
        .to_string()
        .contains("Current session capability mode:"));
    server.abort();
}

#[tokio::test]
async fn failed_context_summary_keeps_original_input_for_resume() {
    use axum::routing::post;
    let (root, mut app) = fixture().await;
    // Test invalid summaries, independently of Luna's smaller context budget.
    fixture_model(&app, "fixture").await;
    let config_path = app.compaction_config_path.clone();
    let mut config = context_compaction::Config::read(&config_path).unwrap();
    config.tail_tokens = 20000;
    config.persist(&config_path).unwrap();
    let mock = Router::new().route(
        "/responses",
        post(|| async {
            let event = json!({"type":"response.completed",
                "response":{"status":"completed","output":[]}});
            (
                [("content-type", "text/event-stream")],
                format!("data: {event}\n\n"),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let mut original = Vec::new();
    for _ in 0..6 {
        original.push(json!({"role":"user","content":"x".repeat(50_000)}));
        original.push(json!({"type":"message","role":"assistant",
            "content":[{"type":"output_text","text":"done"}]}));
    }
    original.push(json!({"role":"user","content":"Current task"}));
    let mut run = app.run(&id).await.unwrap();
    run["input"] = json!(original);
    app.save(&mut run).await.unwrap();
    running(&app, &id, "failed-summary").await;
    execute_run(
        app.clone(),
        id.clone(),
        "failed-summary".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let paused = app.run(&id).await.unwrap();
    assert_eq!(paused["status"], "paused");
    assert_eq!(
        paused["error"],
        "Context summary paused: summary_validation/summary_length; review this run"
    );
    assert_eq!(paused["input"], json!(original));
    let session = app.store.get(&key("session", &sid)).await.unwrap().unwrap();
    assert_eq!(session["checkpoint_through"], 0);
    assert_eq!(
        app.store.context(&sid).await.unwrap()[0]["content"],
        "hello"
    );
    let events = app.store.events(&sid, 0).await.unwrap();
    assert!(!events
        .iter()
        .any(|event| event["kind"] == "context_compacted"));
    server.abort();
}

#[tokio::test]
async fn shutoff_cancels_midstream_model_without_waiting_for_eof() {
    use axum::{
        response::{sse::Event, Sse},
        routing::post,
    };
    let (_root, mut app) = fixture().await;
    let started = Arc::new(tokio::sync::Notify::new());
    let signal = started.clone();
    let mock = Router::new().route(
        "/responses",
        post(move || {
            let signal = signal.clone();
            async move {
                signal.notify_one();
                let first = futures_util::stream::once(async {
                    Ok::<Event, Infallible>(
                        Event::default()
                            .data(r#"{"type":"response.output_text.delta","delta":"partial"}"#),
                    )
                });
                Sse::new(first.chain(futures_util::stream::pending()))
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
    let id = queued(&app, &sid).await;
    running(&app, &id, "generation").await;
    let cancel = CancellationToken::new();
    app.active.lock().await.insert(id.clone(), cancel.clone());
    let executing = tokio::spawn(execute_run(
        app.clone(),
        id.clone(),
        "generation".into(),
        cancel,
    ));
    // Catalog assembly is setup; the cancellation deadline below remains two seconds.
    tokio::time::timeout(Duration::from_secs(10), started.notified())
        .await
        .unwrap();
    assert_eq!(
        control(
            State(app.clone()),
            Json(json!({"client_request_id":"stop","shutoff":true}))
        )
        .await
        .unwrap()
        .0["shutoff"],
        true
    );
    tokio::time::timeout(Duration::from_secs(2), executing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(app.run(&id).await.unwrap()["status"], "stopped");
    let (chat, _) = app.store.chat_snapshot(&sid).await.unwrap();
    assert_eq!(chat.len(), 1);
    assert_eq!(chat[0]["role"], "user");
    server.abort();
}

// Exercise shutdown during a real streaming dispatch, then reopen durable state.
#[tokio::test]
async fn runtime_restart_resumes_model_once_but_preserves_intentional_stops() {
    use axum::response::{sse::Event, Sse};
    for action in [
        "restart",
        "disabled",
        "pause",
        "cancel",
        "shutoff",
        "lease_cancel",
    ] {
        let (root, mut app) = fixture().await;
        let started = Arc::new(tokio::sync::Notify::new());
        let signal = started.clone();
        let mock =
            Router::new().route(
                "/responses",
                post(move || {
                    let signal = signal.clone();
                    async move {
                        signal.notify_one();
                        Sse::new(
                            futures_util::stream::once(async {
                                Ok::<Event, Infallible>(Event::default().data(
                                    r#"{"type":"response.output_text.delta","delta":"partial"}"#,
                                ))
                            })
                            .chain(futures_util::stream::pending()),
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
        let id = queued(&app, &sid).await;
        running(&app, &id, "before-restart").await;
        let original = app.run(&id).await.unwrap()["input"].clone();
        let cancel = app.stop.child_token();
        app.active.lock().await.insert(id.clone(), cancel.clone());
        let executing = tokio::spawn(execute_run(
            app.clone(),
            id.clone(),
            "before-restart".into(),
            cancel.clone(),
        ));
        tokio::time::timeout(Duration::from_secs(20), started.notified())
            .await
            .unwrap();
        match action {
            "restart" | "disabled" => {
                app.stop.cancel();
            }
            "pause" | "cancel" => {
                command(
                    State(app.clone()),
                    Path((id.clone(), action.into())),
                    Json(json!({"client_request_id":uuid()})),
                )
                .await
                .unwrap();
                app.stop.cancel(); // A concurrent deployment must not undo the user's choice.
            }
            "shutoff" => {
                control(
                    State(app.clone()),
                    Json(json!({"client_request_id":uuid(),"shutoff":true})),
                )
                .await
                .unwrap();
                app.stop.cancel();
            }
            _ => cancel.cancel(),
        }
        tokio::time::timeout(Duration::from_secs(5), executing)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        app.active.lock().await.remove(&id);
        let interrupted = app.run(&id).await.unwrap();
        assert_eq!(interrupted["input"], original, "{action}");
        assert_eq!(app.store.chat_snapshot(&sid).await.unwrap().0.len(), 1);
        if matches!(action, "restart" | "disabled") {
            assert_eq!(interrupted["status"], "running");
            assert_eq!(
                interrupted["restart_interruption"]["kind"],
                "runtime_restart"
            );
        } else {
            assert!(interrupted["restart_interruption"].is_null(), "{action}");
        }
        server.abort();
        let mut restarted = app.clone();
        restarted.stop = CancellationToken::new();
        restarted.store = Store::open(root.path()).await.unwrap();
        if action == "disabled" {
            services::save_boot_orphan_resume(&restarted.store, "admin", false, 0)
                .await
                .unwrap();
        }
        recover(&restarted, true).await.unwrap();
        let expected = match action {
            "restart" => "queued",
            "pause" => "paused",
            "cancel" => "cancelled",
            _ => "stopped",
        };
        assert_eq!(
            restarted.run(&id).await.unwrap()["status"],
            expected,
            "{action}"
        );
        if action == "restart" {
            let mock = Router::new().route("/responses", post(|| async {
                let event = json!({"type":"response.completed","response":{"status":"completed",
                    "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Recovered reply"}]}]}});
                ([("content-type", "text/event-stream")], format!("data: {event}\n\n"))
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                axum::serve(listener, mock).await.unwrap();
            });
            restarted.provider =
                provider::Provider::for_test(format!("http://{address}/responses"));
            running(&restarted, &id, "after-restart").await;
            execute_run(
                restarted.clone(),
                id.clone(),
                "after-restart".into(),
                restarted.stop.child_token(),
            )
            .await
            .unwrap();
            assert_eq!(restarted.run(&id).await.unwrap()["status"], "completed");
            recover(&restarted, true).await.unwrap();
            execute_run(
                restarted.clone(),
                id.clone(),
                "after-restart".into(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
            let messages = restarted.store.chat_snapshot(&sid).await.unwrap().0;
            assert_eq!(
                messages.iter().filter(|m| m["role"] == "assistant").count(),
                1
            );
            assert_eq!(messages.last().unwrap()["content"], "Recovered reply");
            server.abort();
        }
    }
}

#[tokio::test]
async fn runtime_restart_retains_pending_read_for_startup_retry() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let (root, mut app) = fixture().await;
    let mock = Router::new().route("/responses", post(|| async {
        let event = json!({"type":"response.completed","response":{"status":"completed",
            "output":[{"type":"function_call","call_id":"restart-read","name":"file_read",
                "arguments":json!({"path":"ui/app.js","offset":1,"limit":5,"task_id":null}).to_string()}]}});
        ([("content-type", "text/event-stream")], format!("data: {event}\n\n"))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    let sid = conversation(&app).await;
    activate_codebase(&app, &sid).await;
    let started = Arc::new(tokio::sync::Notify::new());
    let signal = started.clone();
    let listener = tokio::net::UnixListener::bind(&app.tool_socket).unwrap();
    let worker = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        signal.notify_one();
        std::future::pending::<()>().await;
    });
    let id = queued(&app, &sid).await;
    running(&app, &id, "before-read-restart").await;
    let executing = tokio::spawn(execute_run(
        app.clone(),
        id.clone(),
        "before-read-restart".into(),
        app.stop.child_token(),
    ));
    tokio::time::timeout(Duration::from_secs(20), started.notified())
        .await
        .unwrap();
    app.stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), executing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let interrupted = app.run(&id).await.unwrap();
    assert_eq!(interrupted["status"], "running");
    assert_eq!(interrupted["pending"]["call_id"], "restart-read");
    assert_eq!(interrupted["pending"]["state"], "executing");
    assert_eq!(interrupted["uncertain"], false);
    assert!(!interrupted["input"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["type"] == "function_call_output"));
    let mut restarted = app.clone();
    restarted.stop = CancellationToken::new();
    restarted.store = Store::open(root.path()).await.unwrap();
    recover(&restarted, true).await.unwrap();
    let recovered = restarted.run(&id).await.unwrap();
    assert_eq!(recovered["status"], "queued");
    assert_eq!(recovered["pending"]["state"], "requested");
    assert_eq!(recovered["pending"]["call_id"], "restart-read");
    assert_eq!(
        restarted.store.chat_snapshot(&sid).await.unwrap().0.len(),
        1
    );
    server.abort();
    worker.abort();
}

// Run only through tests/run_real_worker_integration.py in a disposable container.
// The live worker rejects clients that are not the application UID.
#[tokio::test]
#[ignore = "requires the disposable real-worker integration container"]
async fn real_worker_coding_gates_are_end_to_end() {
    use axum::routing::post;

    let socket = PathBuf::from(std::env::var("V3_REAL_WORKER_SOCKET").unwrap());
    let workspace_root = PathBuf::from(std::env::var("V3_REAL_WORKSPACE_ROOT").unwrap());
    let control_dir = PathBuf::from(std::env::var("V3_REAL_CONTROL_DIR").unwrap());
    assert!(socket.exists(), "real worker socket is unavailable");

    async fn scenario(
        socket: &std::path::Path,
        workspace_root: &std::path::Path,
        control_dir: &std::path::Path,
        name: &str,
        arguments: Value,
        marker: &str,
        decision: &str,
    ) {
        let (_root, mut app) = fixture().await;
        app.tool_socket = socket.to_path_buf();
        app.control_dir = control_dir.to_path_buf();
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let captured = requests.clone();
        let call = json!({"type":"function_call","call_id":"real-worker-call",
            "name":name,"arguments":arguments.to_string()});
        let mock = Router::new().route(
            "/responses",
            post(move |Json(body): Json<Value>| {
                let captured = captured.clone();
                let call = call.clone();
                async move {
                    let mut requests = captured.lock().await;
                    requests.push(body);
                    let output = match requests.len() {
                        1 => json!([{"type":"function_call","call_id":"load-codebase",
                            "name":"load_ability",
                            "arguments":json!({"ability_id":"codebase_admin"}).to_string()}]),
                        2 => json!([call]),
                        _ => json!([{"type":"message","role":"assistant",
                            "content":[{"type":"output_text","text":"Done."}]}]),
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
        let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
        app.provider = provider::Provider::for_test(format!("http://{address}/responses"));

        let sid = conversation(&app).await;
        let effect = workspace_root.join(&sid).join(marker);
        assert!(!effect.exists());
        let id = queued(&app, &sid).await;
        if decision == "read_only" {
            let _ = session_mode_set(
                State(app.clone()),
                Path(sid.clone()),
                Json(SessionModeBody {
                    mode: "read_only".into(),
                    expected_revision: None,
                    confirm_write: false,
                }),
            )
            .await
            .unwrap();
        }

        running(&app, &id, "first").await;
        execute_run(
            app.clone(),
            id.clone(),
            "first".into(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(!effect.exists(), "the worker ran before approval");
        let first = app.run(&id).await.unwrap();
        if decision == "read_only" {
            assert_eq!(first["status"], "completed");
        } else {
            assert_eq!(
                first["status"],
                "waiting_approval",
                "tool={name}, requests={:?}, run={first}",
                requests.lock().await
            );
            assert_eq!(first["pending"]["name"], name);
            let pending = first["pending"]["id"].as_str().unwrap().to_owned();
            let chosen = if decision == "deny" {
                "deny"
            } else {
                "approve"
            };
            assert_eq!(
                approval(
                    State(app.clone()),
                    Path(pending),
                    Json(json!({"client_request_id":"real-worker-decision",
                        "decision":chosen})),
                )
                .await
                .unwrap()
                .0["status"],
                "queued"
            );
            assert!(!effect.exists(), "approval itself dispatched the worker");
            if decision == "shutoff" {
                assert_eq!(
                    control(
                        State(app.clone()),
                        Json(json!({"client_request_id":"real-worker-shutoff",
                            "shutoff":true})),
                    )
                    .await
                    .unwrap()
                    .0["shutoff"],
                    true
                );
                assert_eq!(app.run(&id).await.unwrap()["status"], "stopped");
                assert!(!effect.exists());
                server.abort();
                return;
            }
            running(&app, &id, "second").await;
            execute_run(
                app.clone(),
                id.clone(),
                "second".into(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        }

        assert_eq!(app.run(&id).await.unwrap()["status"], "completed");
        let requests = requests.lock().await;
        assert_eq!(requests.len(), 3);
        let outputs = requests[2]["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|value| {
                value["type"] == "function_call_output" && value["call_id"] == "real-worker-call"
            })
            .collect::<Vec<_>>();
        assert_eq!(outputs.len(), 1);
        let result: Value = serde_json::from_str(outputs[0]["output"].as_str().unwrap()).unwrap();
        if decision == "approve" {
            assert_eq!(result["status"], "success");
            assert_eq!(result["output"].as_str().unwrap().trim(), "1101");
            assert_eq!(std::fs::read_to_string(&effect).unwrap(), "once\n");
            drop(requests);
            execute_run(
                app.clone(),
                id.clone(),
                "second".into(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
            assert_eq!(std::fs::read_to_string(&effect).unwrap(), "once\n");
        } else {
            assert_eq!(result["status"], "denied");
            assert!(!effect.exists(), "a denied tool changed the workspace");
        }
        server.abort();
    }

    scenario(
        &socket,
        &workspace_root,
        &control_dir,
        "shell",
        json!({"command":"printf 'once\\n' >> shell-effect.log; id -u"}),
        "shell-effect.log",
        "approve",
    )
    .await;
    scenario(
        &socket,
        &workspace_root,
        &control_dir,
        "python",
        json!({"code":"import os; open('python-effect.log','a').write('once'+chr(10)); print(os.getuid())"}),
        "python-effect.log",
        "approve",
    )
    .await;
    scenario(
        &socket,
        &workspace_root,
        &control_dir,
        "shell",
        json!({"command":"printf denied >> denied-effect.log"}),
        "denied-effect.log",
        "deny",
    )
    .await;
    scenario(
        &socket,
        &workspace_root,
        &control_dir,
        "shell",
        json!({"command":"printf read-only >> read-only-effect.log"}),
        "read-only-effect.log",
        "read_only",
    )
    .await;
    scenario(
        &socket,
        &workspace_root,
        &control_dir,
        "shell",
        json!({"command":"printf stopped >> stopped-effect.log"}),
        "stopped-effect.log",
        "shutoff",
    )
    .await;
}

#[tokio::test]
async fn private_events_never_appear_in_chat_snapshot_or_cursor() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let before = snapshot(State(app.clone()), Path(sid.clone()))
        .await
        .unwrap()
        .0;
    assert_eq!(before["messages"].as_array().unwrap().len(), 1);
    let cursor = before["last_seq"].as_i64().unwrap();
    app.store
        .event(&sid, "delta", &json!({"run_id":id,"text":"draft"}))
        .await
        .unwrap();
    let after = snapshot(State(app.clone()), Path(sid.clone()))
        .await
        .unwrap()
        .0;
    assert_eq!(after["last_seq"], cursor);
    assert_eq!(after["messages"], before["messages"]);
    assert!(app
        .store
        .chat_events(&sid, cursor)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn sse_last_event_id_replays_only_chat_after_reconnect_cursor() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let cursor = app.store.chat_snapshot(&sid).await.unwrap().1;
    app.store
        .event(&sid, "delta", &json!({"run_id":id,"text":"draft"}))
        .await
        .unwrap();
    let mut run = app.run(&id).await.unwrap();
    run["status"] = json!("completed");
    run["turns"] = json!(1);
    app.store
        .save_run(
            &mut run,
            "model_output",
            &json!({
                "output":[{"type":"message","role":"assistant",
                    "content":[{"type":"output_text","text":"done"}]}],
                "terminal_text":"done"
            }),
        )
        .await
        .unwrap();
    let expected = app.store.chat_events(&sid, cursor).await.unwrap();
    assert_eq!(expected.len(), 1);
    let token = uuid();
    app.store
        .put(
            &format!("auth:{}", digest(&json!(token))),
            &json!({"csrf":"fixture","expires":now()+3600,"generation":"fixture-generation"}),
        )
        .await
        .unwrap();
    let request = Request::builder()
        .uri(format!("/api/v3/sessions/{sid}/events?after=0"))
        .header("cookie", format!("v3_session={token}"))
        .header("last-event-id", cursor.to_string())
        .body(Body::empty())
        .unwrap();
    let response = router(app.clone()).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body().into_data_stream();
    let chunk = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = std::str::from_utf8(&chunk).unwrap();
    let data = text
        .lines()
        .find_map(|line| line.strip_prefix("data:").map(str::trim))
        .unwrap();
    let event: Value = serde_json::from_str(data).unwrap();
    assert_eq!(event, expected[0]);
    assert_eq!(event["content"], "done");
    assert!(text
        .lines()
        .any(|line| line == format!("id: {}", expected[0]["seq"])));
    drop(stream);
}

#[tokio::test]
async fn expired_and_revoked_sessions_cannot_open_authenticated_routes() {
    let (_root, app) = fixture().await;
    let token = uuid();
    let auth_key = format!("auth:{}", digest(&json!(token)));
    app.store
        .put(
            &auth_key,
            &json!({"csrf":"fixture","expires":now()-1,
            "generation":"fixture-generation","user_id":"admin"}),
        )
        .await
        .unwrap();
    let request = || {
        Request::builder()
            .uri("/api/v3/status")
            .header("cookie", format!("v3_session={token}"))
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        router(app.clone())
            .oneshot(request())
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    app.store
        .put(
            &auth_key,
            &json!({"csrf":"fixture","expires":now()+3600,
            "generation":"fixture-generation","user_id":"admin"}),
        )
        .await
        .unwrap();
    assert_eq!(
        router(app.clone())
            .oneshot(request())
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    app.store.delete(&auth_key).await.unwrap();
    assert_eq!(
        router(app.clone())
            .oneshot(request())
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn agent_model_settings_normalize_without_losing_selected_model() {
    let (_root, app) = fixture().await;
    app.store
        .save_agent_model(&json!({"provider":"openai_subscription","model":"legacy-chosen"}))
        .await
        .unwrap();
    migrate_provider_selection(&app.store).await.unwrap();
    migrate_provider_selection(&app.store).await.unwrap();
    let value = app.store.agent_model().await.unwrap();
    assert_eq!(
        value,
        json!({"provider":"openai_subscription","model":"legacy-chosen"})
    );
    let read = agent_model(State(app.clone()), Path("shared_default".into()))
        .await
        .unwrap()
        .0;
    assert_eq!(read["provider"], "openai_subscription");
    assert_eq!(read["model"], "legacy-chosen");
}

#[tokio::test]
async fn unsupported_provider_is_rejected_without_changing_settings() {
    let (_root, app) = fixture().await;
    let original = json!({"model":"original","provider":"openai_subscription"});
    app.store.save_agent_model(&original).await.unwrap();
    for provider in [json!("unsupported"), json!(null), json!(7)] {
        let body = json!({"model":"replacement","provider":provider});
        assert_eq!(
            settings(State(app.clone()), Json(body.clone()))
                .await
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            agent_model_set(
                State(app.clone()),
                Path("shared_default".into()),
                Json(body)
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(app.store.agent_model().await.unwrap(), original);
    }
}

#[tokio::test]
async fn legacy_run_binds_selection_once_and_invalid_selection_pauses_before_dispatch() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    run.as_object_mut().unwrap().remove("provider_selection");
    // Exceeding the turn limit stops before HTTP, after the legacy binding checkpoint.
    run["turns"] = json!(50);
    app.save(&mut run).await.unwrap();
    running(&app, &id, "legacy").await;
    execute_run(
        app.clone(),
        id.clone(),
        "legacy".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let pinned = app.run(&id).await.unwrap()["provider_selection"].clone();
    assert_eq!(pinned["provider"], "openai_subscription");
    let sid2 = conversation(&app).await;
    let id2 = queued(&app, &sid2).await;
    let mut invalid = app.run(&id2).await.unwrap();
    invalid["provider_selection"] = json!({"provider":"unsupported","model":"valid"});
    app.save(&mut invalid).await.unwrap();
    running(&app, &id2, "invalid").await;
    execute_run(
        app.clone(),
        id2.clone(),
        "invalid".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let paused = app.run(&id2).await.unwrap();
    assert_eq!(paused["status"], "paused");
    assert_eq!(paused["attempt"], 0);
}

#[tokio::test]
async fn state_inspection_is_authenticated_policy_controlled_and_session_bounded() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let other = conversation(&app).await;
    let large = "x".repeat(800);
    for (session, content) in [(&sid, large.as_str()), (&other, "other session")] {
        sqlx::query(
            "INSERT INTO chat(id,session_id,run_id,role,content,created_at) VALUES(?,?,?,?,?,?)",
        )
        .bind(uuid())
        .bind(session)
        .bind("run-test")
        .bind("user")
        .bind(app.store.seal(&json!(content)).unwrap())
        .bind(1_i64)
        .execute(&app.store.pool)
        .await
        .unwrap();
    }

    let args: InspectArgs = serde_json::from_value(json!({"resource":"chat","limit":1})).unwrap();
    let page = inspect_state(&app, &sid, &args).await.unwrap();
    assert_eq!(page["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        page["messages"][0]["content_preview"]["text"]
            .as_str()
            .unwrap()
            .len(),
        512
    );
    assert_eq!(page["messages"][0]["content_preview"]["truncated"], true);
    assert!(page["messages"][0].get("content").is_none());
    assert!(!page.to_string().contains("other session"));
    assert!(serde_json::from_value::<InspectArgs>(
        json!({"resource":"session","session_id":other})
    )
    .is_err());
    for invalid in [
        json!({"resource":"chat","after":-1}),
        json!({"resource":"chat","limit":51}),
        json!({"resource":"runs","after":1}),
        json!({"resource":"unknown"}),
    ] {
        let parsed: InspectArgs = serde_json::from_value(invalid).unwrap();
        assert!(validate_inspect_args(&parsed).is_err());
    }

    let uri = format!("/api/v3/sessions/{sid}/state-inspect?resource=chat&limit=1");
    let request = |cookie: Option<&str>| {
        let mut builder = Request::builder().uri(&uri);
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", cookie);
        }
        builder.body(Body::empty()).unwrap()
    };
    let response = router(app.clone()).oneshot(request(None)).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let token = uuid();
    app.store
        .put(
            &format!("auth:{}", digest(&json!(token))),
            &json!({"csrf":"fixture","expires":now()+3600,"generation":"fixture-generation"}),
        )
        .await
        .unwrap();
    let cookie = format!("v3_session={token}");
    let response = router(app.clone())
        .oneshot(request(Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let returned: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        returned["messages"][0]["content_preview"]["truncated"],
        true
    );

    let mut policy = policy_default();
    policy["rules"]["state_inspect"] = json!("deny");
    app.store.put("policy", &policy).await.unwrap();
    let response = router(app.clone())
        .oneshot(request(Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
#[tokio::test]
/// Verify an ability that requires a model role cannot be loaded until the roster appoints it.
async fn image_generation_stays_off_without_an_image_out_model() {
    use axum::routing::post;

    let (_root, mut app) = fixture().await;
    let mut tier = app
        .store
        .entitlement_tier("operator")
        .await
        .unwrap()
        .unwrap();
    let tier_revision = tier.revision;
    tier.ability_permissions
        .insert("image_generation".into(), access::Permission::Auto);
    app.store
        .save_entitlement_tier(tier, tier_revision)
        .await
        .unwrap();
    let mut levels = app.store.capability_levels("shared_default").await.unwrap();
    for id in ["operator", "member", "visitor"] {
        let mut level = levels.levels[id].clone();
        level
            .ability_permissions
            .insert("image_generation".into(), crate::access::Permission::Auto);
        levels = app
            .store
            .save_capability_level("shared_default", level, levels.revision)
            .await
            .unwrap();
    }
    let mut policy = policy_default();
    policy["rules"]["generate_image"] = json!("allow");
    app.store.put("policy", &policy).await.unwrap();
    let mock = Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| async move {
            let answered = body["input"].as_array().is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["type"] == "function_call_output")
            });
            let output = if answered {
                json!([{"type":"message","role":"assistant",
                    "content":[{"type":"output_text","text":"Done."}]}])
            } else {
                json!([{"type":"function_call","call_id":"load","name":"load_ability",
                    "arguments":"{\"ability_id\":\"image_generation\"}"}])
            };
            let event = json!({"type":"response.completed",
                "response":{"status":"completed","output":output}});
            (
                [("content-type", "text/event-stream")],
                format!("data: {event}\n\n"),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    let load_result = |app: App, generation: &'static str| async move {
        let sid = conversation(&app).await;
        session_mode_set(
            State(app.clone()),
            Path(sid.clone()),
            Json(SessionModeBody {
                mode: "auto".into(),
                expected_revision: session_mode(&app, &sid).await.unwrap()["revision"].as_u64(),
                confirm_write: true,
            }),
        )
        .await
        .unwrap();
        let sent = send(
            State(app.clone()),
            Path(sid.clone()),
            Json(json!({"client_message_id":uuid(),"text":"draw a cat"})),
        )
        .await
        .unwrap();
        let id = sent.1 .0["run_id"].as_str().unwrap().to_owned();
        running(&app, &id, generation).await;
        execute_fixture_journey(
            app.clone(),
            id.clone(),
            generation.into(),
            CancellationToken::new(),
        )
        .await;
        let run = app.run(&id).await.unwrap();
        let item = run["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "function_call_output" && item["call_id"] == "load")
            .cloned()
            .unwrap();
        serde_json::from_str::<Value>(item["output"].as_str().unwrap()).unwrap()
    };
    let refused = load_result(app.clone(), "role-missing").await;
    assert_ne!(refused["status"], "ok", "{refused}");

    let mut catalog = app.store.model_catalog().await.unwrap();
    let standard = catalog.agent_models["shared_default"].roles
        [&model_selection::ModelRole::Standard]
        .entry_id
        .clone();
    let mut image = catalog.entries[&standard].clone();
    image.id = "image-out".into();
    image.capabilities.insert("image_out".into());
    image.revision = 1;
    catalog.entries.insert(image.id.clone(), image);
    let roster = catalog.agent_models.get_mut("shared_default").unwrap();
    roster.entry_ids.push("image-out".into());
    roster.roles.insert(
        model_selection::ModelRole::ImageOut,
        model_selection::RoleAssignment {
            entry_id: "image-out".into(),
            backup_entry_ids: vec![],
        },
    );
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
    let loaded = load_result(app.clone(), "role-present").await;
    assert_eq!(loaded["status"], "ok", "{loaded}");
    server.abort();
}
#[tokio::test]
/// Verify discovery calls batched with other calls run serially: both list calls succeed and a
/// tool from an ability loaded earlier in the same batch is authorized after the load.
async fn batched_discovery_calls_run_serially_and_succeed() {
    use axum::routing::post;

    let (_root, mut app) = fixture().await;
    let sid = conversation(&app).await;
    let sent = send(
        State(app.clone()),
        Path(sid.clone()),
        Json(json!({"client_message_id":uuid(),"text":"what can you inspect?"})),
    )
    .await
    .unwrap();
    let id = sent.1 .0["run_id"].as_str().unwrap().to_owned();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let mock = Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured.clone();
            async move {
                let mut captured = captured.lock().await;
                captured.push(body);
                let output = match captured.len() {
                    1 => json!([
                        {"type":"function_call","call_id":"list-a","name":"list_abilities",
                            "arguments":"{\"query\":\"state\"}"},
                        {"type":"function_call","call_id":"list-b","name":"list_abilities",
                            "arguments":"{\"include_available\":true}"}]),
                    2 => json!([
                        {"type":"function_call","call_id":"load","name":"load_ability",
                            "arguments":"{\"ability_id\":\"codebase_admin\"}"},
                        {"type":"function_call","call_id":"inspect","name":"state_inspect",
                            "arguments":"{\"resource\":\"chat\",\"limit\":1}"}]),
                    3 => json!([{"type":"message","role":"assistant",
                        "content":[{"type":"output_text","text":"Done."}]}]),
                    _ => panic!("Unexpected provider turn"),
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
    running(&app, &id, "batched-discovery").await;
    execute_fixture_journey(
        app.clone(),
        id.clone(),
        "batched-discovery".into(),
        CancellationToken::new(),
    )
    .await;

    assert_eq!(app.run(&id).await.unwrap()["status"], "completed");
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 3);
    let result_for = |request: &Value, call_id: &str| -> Value {
        let item = request["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "function_call_output" && item["call_id"] == call_id)
            .unwrap();
        serde_json::from_str(item["output"].as_str().unwrap()).unwrap()
    };
    for call_id in ["list-a", "list-b"] {
        let listed = result_for(&requests[1], call_id);
        assert_eq!(listed["status"], "ok", "{call_id}: {listed}");
    }
    let loaded = result_for(&requests[2], "load");
    assert_eq!(loaded["status"], "ok", "{loaded}");
    let inspected = result_for(&requests[2], "inspect");
    assert_eq!(inspected["status"], "success", "{inspected}");
    server.abort();
}
#[tokio::test]
/// Verify native ability loading adds current guidance only to model-call frames and keeps state inspection scoped to the active session.
async fn agent_discovers_ability_then_inspects_only_its_current_session() {
    use axum::routing::post;

    let (_root, mut app) = fixture().await;
    let sid = conversation(&app).await;
    let other = conversation(&app).await;
    let private_other_text = "only in the other session";
    let _ = send(
        State(app.clone()),
        Path(other.clone()),
        Json(json!({"client_message_id":uuid(),"text":private_other_text})),
    )
    .await
    .unwrap();
    let long_text = "current session text ".repeat(40);
    let sent = send(
        State(app.clone()),
        Path(sid.clone()),
        Json(json!({"client_message_id":uuid(),"text":long_text})),
    )
    .await
    .unwrap();
    let id = sent.1 .0["run_id"].as_str().unwrap().to_owned();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let other_for_call = other.clone();
    let mock = Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured.clone();
            let other_for_call = other_for_call.clone();
            async move {
                let mut captured = captured.lock().await;
                captured.push(body);
                let output = match captured.len() {
                    1 => json!([{"type":"function_call","call_id":"discover",
                        "name":"list_abilities","arguments":"{}"}]),
                    2 => json!([{"type":"function_call","call_id":"load",
                        "name":"load_ability",
                        "arguments":"{\"ability_id\":\"codebase_admin\"}"}]),
                    3 => json!([{"type":"function_call","call_id":"inspect",
                        "name":"state_inspect",
                        "arguments":"{\"resource\":\"chat\",\"limit\":1}"}]),
                    4 => json!([{"type":"function_call","call_id":"cross-session",
                        "name":"state_inspect",
                        "arguments":json!({"resource":"chat","session_id":other_for_call}).to_string()}]),
                    5 => json!([{"type":"message","role":"assistant",
                        "content":[{"type":"output_text","text":"Inspection complete."}]}]),
                    _ => panic!("Unexpected provider turn"),
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
    running(&app, &id, "discover-inspect").await;
    execute_fixture_journey(
        app.clone(),
        id.clone(),
        "discover-inspect".into(),
        CancellationToken::new(),
    )
    .await;

    assert_eq!(app.run(&id).await.unwrap()["status"], "completed");
    assert_eq!(
        app.run(&id).await.unwrap()["loaded_abilities"],
        json!(["codebase_admin"])
    );
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 5);
    let names = |request: &Value| {
        request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    for request in &requests[..2] {
        assert_eq!(names(request), vec!["list_abilities", "load_ability"]);
    }
    assert!(names(&requests[2]).contains(&"state_inspect".to_owned()));
    let result_for = |request: &Value, call_id: &str| -> Value {
        let item = request["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "function_call_output" && item["call_id"] == call_id)
            .unwrap();
        serde_json::from_str(item["output"].as_str().unwrap()).unwrap()
    };
    let discovery = result_for(&requests[1], "discover");
    assert_eq!(discovery["status"], "ok");
    assert!(discovery["abilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|ability| ability["id"] == "codebase_admin"));
    let loaded = result_for(&requests[2], "load");
    assert_eq!(loaded["status"], "ok");
    assert_eq!(loaded["ability_id"], "codebase_admin");
    assert_eq!(loaded["context_source"], "live_ability_catalog");
    for request in &requests[..2] {
        assert!(native_ability_frames(request, "codebase_admin").is_empty());
    }
    let guidance = native_ability_frames(&requests[2], "codebase_admin");
    assert_eq!(guidance.len(), 1);
    assert!(guidance[0].contains("Codebase Admin"));
    assert!(!requests[2]["instructions"]
        .as_str()
        .unwrap()
        .contains("## Untrusted live guidance: ability:codebase_admin\n"));
    assert!(!app.run(&id).await.unwrap()["input"]
        .to_string()
        .contains("## Untrusted live guidance: ability:codebase_admin\n"));
    assert!(!json!(app.store.context(&sid).await.unwrap())
        .to_string()
        .contains("## Untrusted live guidance: ability:codebase_admin\n"));
    let inspected = result_for(&requests[3], "inspect");
    assert_eq!(inspected["status"], "success", "{inspected}");
    let messages = inspected["output"]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0]["content_preview"]["text"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        512
    );
    assert_eq!(messages[0]["content_preview"]["truncated"], true);
    assert!(messages[0].get("content").is_none());
    assert!(!inspected.to_string().contains(private_other_text));
    assert_eq!(
        result_for(&requests[4], "cross-session")["status"],
        "denied"
    );
    server.abort();
}

#[tokio::test]
/// Verify dispatch reloads current prompt slots even when a queued message retained an older admission snapshot.
async fn prompt_slot_instructions_reload_after_a_message_is_accepted() {
    use axum::routing::post;

    let (_root, mut app) = fixture().await;
    let original = app
        .store
        .upsert_prompt_slot("coding", "Use the first instruction.", None, None, None, 0)
        .await
        .unwrap();
    let sid = conversation(&app).await;
    let first_id = queued(&app, &sid).await;
    assert!(app.run(&first_id).await.unwrap()["instructions"]
        .as_str()
        .unwrap()
        .contains("Use the first instruction."));
    app.store
        .upsert_prompt_slot(
            "coding",
            "Use the second instruction.",
            None,
            None,
            None,
            original["revision"].as_i64().unwrap(),
        )
        .await
        .unwrap();

    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let requests = captured.clone();
    let mock = Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let requests = requests.clone();
            async move {
                requests.lock().await.push(body);
                let event = json!({"type":"response.completed",
                "response":{"status":"completed","output":[{
                    "type":"message","role":"assistant",
                    "content":[{"type":"output_text","text":"Done."}]
                }]}});
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

    running(&app, &first_id, "first-slot-run").await;
    execute_run(
        app.clone(),
        first_id.clone(),
        "first-slot-run".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(app.run(&first_id).await.unwrap()["status"], "completed");
    let second_id = queued(&app, &sid).await;
    running(&app, &second_id, "second-slot-run").await;
    execute_run(
        app.clone(),
        second_id.clone(),
        "second-slot-run".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(app.run(&second_id).await.unwrap()["status"], "completed");
    let requests = captured.lock().await;
    assert_eq!(requests.len(), 2);
    let first = requests[0]["instructions"].as_str().unwrap();
    let second = requests[1]["instructions"].as_str().unwrap();
    assert!(!first.contains("Use the first instruction."));
    assert!(first.contains("Use the second instruction."));
    assert!(second.contains("Use the second instruction."));
    server.abort();
}

#[tokio::test]
async fn prompt_slot_api_requires_auth_csrf_and_current_revision() {
    let (_root, app) = fixture().await;
    let uri = "/api/v3/agents/shared_default/slots/coding";
    let body = json!({"content":"Use bounded tools.","expected_revision":0}).to_string();
    let put_request = |cookie: Option<&str>, csrf: Option<&str>, body: &str| {
        let mut builder = Request::builder()
            .method("PUT")
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", cookie);
        }
        if let Some(csrf) = csrf {
            builder = builder.header("x-csrf-token", csrf);
        }
        builder.body(Body::from(body.to_owned())).unwrap()
    };
    let response = router(app.clone())
        .oneshot(put_request(None, None, &body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let token = uuid();
    app.store
        .put(
            &format!("auth:{}", digest(&json!(token))),
            &json!({"csrf":"fixture","expires":now()+3600,"generation":"fixture-generation"}),
        )
        .await
        .unwrap();
    let cookie = format!("v3_session={token}");
    let response = router(app.clone())
        .oneshot(put_request(Some(&cookie), None, &body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = router(app.clone())
        .oneshot(put_request(Some(&cookie), Some("fixture"), &body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // The typed 82-entry configuration catalog includes bounded descriptor metadata.
    let bytes = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    let created: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(created["slot"]["revision"], 1);

    let response = router(app.clone())
        .oneshot(put_request(Some(&cookie), Some("fixture"), &body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let get = Request::builder()
        .uri(uri)
        .header("cookie", &cookie)
        .body(Body::empty())
        .unwrap();
    let response = router(app.clone()).oneshot(get).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let fetched: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(fetched["slot"]["content"], "Use bounded tools.");
    assert_eq!(fetched["slot"]["revision"], 1);
}

#[tokio::test]
async fn prompt_slot_tool_in_auto_writes_once_without_approval() {
    use axum::routing::post;

    let (_root, mut app) = fixture().await;
    let args = json!({"action":"put","slot_name":"coding","content":"Use checked patches.","expected_revision":0});
    let call = json!({"type":"function_call","call_id":"slot-call",
        "name":"prompt_slot_write","arguments":args.to_string()});
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let mock = Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let requests = captured.clone();
            let call = call.clone();
            async move {
                let mut requests = requests.lock().await;
                requests.push(body);
                let output = if requests.len() == 1 {
                    json!([call])
                } else {
                    json!([{"type":"message","role":"assistant",
                        "content":[{"type":"output_text","text":"Slot updated."}]}])
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
    activate_codebase(&app, &sid).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    run["loaded_abilities"] = json!(["codebase_admin"]);
    app.save(&mut run).await.unwrap();
    running(&app, &id, "slot-first").await;
    execute_run(
        app.clone(),
        id.clone(),
        "slot-first".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    // Auto crosses the policy's Ask threshold without a per-tool decision.
    assert_eq!(app.run(&id).await.unwrap()["status"], "completed");
    let slot = app.store.get_prompt_slot("coding").await.unwrap().unwrap();
    assert_eq!(slot["content"], "Use checked patches.");
    assert_eq!(slot["revision"], 1);
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 2);
    let output = requests[1]["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    let result: Value = serde_json::from_str(output["output"].as_str().unwrap()).unwrap();
    assert_eq!(result["status"], "success");
    assert_eq!(result["output"]["slot"]["revision"], 1);
    server.abort();
}

async fn ability_request(
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

async fn ability_auth(app: &App) -> String {
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

#[tokio::test]
async fn session_metadata_routes_enforce_auth_csrf_ownership_and_retries() {
    use axum::http::Method;
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let uri = format!("/api/v3/sessions/{sid}");
    let cookie = ability_auth(&app).await;
    let body = json!({"client_request_id":"rename-pin","expected_revision":0,
        "title":"  Renamed session  ","pinned":true});
    assert_eq!(
        ability_request(&app, Method::PATCH, &uri, Some(body.clone()), None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        ability_request(
            &app,
            Method::PATCH,
            &uri,
            Some(body.clone()),
            Some(&cookie),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, result) = ability_request(
        &app,
        Method::PATCH,
        &uri,
        Some(body.clone()),
        Some(&cookie),
        Some("ability-fixture"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["session"]["title"], "Renamed session");
    assert_eq!(result["session"]["pinned"], true);
    assert_eq!(result["session"]["metadata_revision"], 1);
    let (_, retry) = ability_request(
        &app,
        Method::PATCH,
        &uri,
        Some(body.clone()),
        Some(&cookie),
        Some("ability-fixture"),
    )
    .await;
    assert_eq!(retry, result);
    let mut changed = body.clone();
    changed["title"] = json!("Different input");
    assert_eq!(
        ability_request(
            &app,
            Method::PATCH,
            &uri,
            Some(changed),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        ability_request(
            &app,
            Method::PATCH,
            &uri,
            Some(json!({"client_request_id":"stale","expected_revision":0,"pinned":false})),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        ability_request(
            &app,
            Method::PATCH,
            &uri,
            Some(json!({"client_request_id":"empty","expected_revision":1,"title":"  "})),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let invalid = Request::builder()
        .method(Method::PATCH)
        .uri(&uri)
        .header("cookie", &cookie)
        .header("x-csrf-token", "ability-fixture")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"client_request_id":"invalid","expected_revision":1,"pinned":"yes"}).to_string(),
        ))
        .unwrap();
    assert_eq!(
        router(app.clone()).oneshot(invalid).await.unwrap().status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (status, snapshot) =
        ability_request(&app, Method::GET, &uri, None, Some(&cookie), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(snapshot["session"], result["session"]);
    let (status, list) = ability_request(
        &app,
        Method::GET,
        "/api/v3/sessions",
        None,
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(list["server_time"].as_i64().is_some());
    assert_eq!(list["sessions"][0], result["session"]);
    assert!(list["sessions"][0].get("checkpoint_input").is_none());
    // A session in another user's database is indistinguishable from missing.
    sqlx::query("INSERT INTO user_accounts(user_id,username,value) VALUES('other','other',?)")
        .bind(
            app.store
                .seal(&json!({"user_id":"other","username":"other"}))
                .unwrap(),
        )
        .execute(&app.store.app_pool)
        .await
        .unwrap();
    let other = app.store.for_user("other").await.unwrap();
    other
        .put(
            "session:foreign",
            &json!({"id":"foreign","agent_id":"shared_default"}),
        )
        .await
        .unwrap();
    assert_eq!(
        ability_request(
            &app,
            Method::PATCH,
            "/api/v3/sessions/foreign",
            Some(body.clone()),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    app.store
        .put(
            "session:no-access",
            &json!({"id":"no-access","agent_id":"missing-agent"}),
        )
        .await
        .unwrap();
    assert_eq!(
        ability_request(
            &app,
            Method::PATCH,
            "/api/v3/sessions/no-access",
            Some(body),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn ability_management_routes_enforce_auth_revisions_and_session_boundaries() {
    use axum::http::Method;
    let (_root, app) = fixture().await;
    let first = conversation(&app).await;
    let second = conversation(&app).await;
    let agent_uri = "/api/v3/agents/shared_default/abilities";
    let visibility_uri = "/api/v3/agents/shared_default/abilities/codebase_admin/visibility";
    let first_uri = format!("/api/v3/sessions/{first}/abilities");
    let first_set_uri = format!("{first_uri}/codebase_admin");
    let second_uri = format!("/api/v3/sessions/{second}/abilities");
    let second_set_uri = format!("{second_uri}/codebase_admin");
    let visible = json!({"visibility":"visible","expected_revision":0});
    let active = json!({"state":"active","expected_revision":0});

    assert_eq!(
        ability_request(&app, Method::GET, agent_uri, None, None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        ability_request(
            &app,
            Method::PUT,
            visibility_uri,
            Some(visible.clone()),
            None,
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let cookie = ability_auth(&app).await;
    assert_eq!(
        ability_request(
            &app,
            Method::PUT,
            visibility_uri,
            Some(visible.clone()),
            Some(&cookie),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        ability_request(
            &app,
            Method::PUT,
            &first_set_uri,
            Some(active.clone()),
            Some(&cookie),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );

    let (status, agent) =
        ability_request(&app, Method::GET, agent_uri, None, Some(&cookie), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(agent["revision"], 0);
    assert_eq!(agent["abilities"][0]["visibility"], "discoverable");
    let (status, first_state) =
        ability_request(&app, Method::GET, &first_uri, None, Some(&cookie), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        first_state["state"],
        json!({"revision":0,"active":[],"suppressed":[]})
    );

    let (status, result) = ability_request(
        &app,
        Method::PUT,
        visibility_uri,
        Some(visible.clone()),
        Some(&cookie),
        Some("ability-fixture"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["revision"], 1);
    assert_eq!(result["visibility"]["codebase_admin"], "visible");
    let (status, same) = ability_request(
        &app,
        Method::PUT,
        visibility_uri,
        Some(json!({"visibility":"visible","expected_revision":1})),
        Some(&cookie),
        Some("ability-fixture"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(same["revision"], 1);
    assert_eq!(
        ability_request(
            &app,
            Method::PUT,
            visibility_uri,
            Some(visible),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        ability_request(
            &app,
            Method::PUT,
            visibility_uri,
            Some(json!({"visibility":"hidden","expected_revision":1})),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        ability_request(
            &app,
            Method::PUT,
            "/api/v3/agents/shared_default/abilities/missing/visibility",
            Some(json!({"visibility":"visible","expected_revision":1})),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        ability_request(
            &app,
            Method::PUT,
            "/api/v3/agents/missing/abilities/codebase_admin/visibility",
            Some(json!({"visibility":"visible","expected_revision":1})),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    let (policy, _, visibility, state) = ability_controls(&app, &second).await.unwrap();
    assert!(app
        .abilities
        .schemas_with(&policy, &json!([]), &visibility, &state)
        .iter()
        .any(|tool| tool["name"] == "file_read"));
    assert!(!app
        .abilities
        .visible_guidance(&policy, &visibility, &state)
        .is_empty());

    let (status, activated) = ability_request(
        &app,
        Method::PUT,
        &first_set_uri,
        Some(active.clone()),
        Some(&cookie),
        Some("ability-fixture"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(activated["revision"], 1);
    assert_eq!(activated["active"], json!(["codebase_admin"]));
    assert_eq!(
        ability_request(
            &app,
            Method::PUT,
            &first_set_uri,
            Some(active),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        ability_request(
            &app,
            Method::PUT,
            &first_set_uri,
            Some(json!({"state":"active","expected_revision":1})),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .1["revision"],
        1
    );
    assert_eq!(
        ability_request(
            &app,
            Method::PUT,
            &first_set_uri,
            Some(json!({"state":"unknown","expected_revision":1})),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        ability_request(
            &app,
            Method::PUT,
            &format!("{first_uri}/missing"),
            Some(json!({"state":"active","expected_revision":1})),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        ability_request(
            &app,
            Method::PUT,
            &format!("/api/v3/sessions/{}/abilities/codebase_admin", uuid()),
            Some(json!({"state":"active","expected_revision":0})),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let (_, second_state) =
        ability_request(&app, Method::GET, &second_uri, None, Some(&cookie), None).await;
    assert_eq!(
        second_state["state"],
        json!({"revision":0,"active":[],"suppressed":[]})
    );

    let (status, suppressed) = ability_request(
        &app,
        Method::PUT,
        &first_set_uri,
        Some(json!({"state":"suppressed","expected_revision":1})),
        Some(&cookie),
        Some("ability-fixture"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(suppressed["revision"], 2);
    let (policy, _, visibility, first_state) = ability_controls(&app, &first).await.unwrap();
    assert!(!app
        .abilities
        .schemas_with(
            &policy,
            &json!(["codebase_admin"]),
            &visibility,
            &first_state
        )
        .iter()
        .any(|tool| tool["name"] == "file_read"));
    assert!(app
        .abilities
        .visible_guidance(&policy, &visibility, &first_state)
        .is_empty());
    let (_, _, visibility, second_state) = ability_controls(&app, &second).await.unwrap();
    assert!(app
        .abilities
        .schemas_with(&policy, &json!([]), &visibility, &second_state)
        .iter()
        .any(|tool| tool["name"] == "file_read"));

    let (status, changed) = ability_request(
        &app,
        Method::PUT,
        visibility_uri,
        Some(json!({"visibility":"discoverable","expected_revision":1})),
        Some(&cookie),
        Some("ability-fixture"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(changed["revision"], 2);
    let (policy, _, visibility, second_state) = ability_controls(&app, &second).await.unwrap();
    assert!(!app
        .abilities
        .schemas_with(&policy, &json!([]), &visibility, &second_state)
        .iter()
        .any(|tool| tool["name"] == "file_read"));
    assert!(app
        .abilities
        .visible_guidance(&policy, &visibility, &second_state)
        .is_empty());
    let (status, second_active) = ability_request(
        &app,
        Method::PUT,
        &second_set_uri,
        Some(json!({"state":"active","expected_revision":0})),
        Some(&cookie),
        Some("ability-fixture"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second_active["revision"], 1);
    let (policy, _, visibility, second_state) = ability_controls(&app, &second).await.unwrap();
    assert!(app
        .abilities
        .schemas_with(&policy, &json!([]), &visibility, &second_state)
        .iter()
        .any(|tool| tool["name"] == "file_read"));
    assert!(!app
        .abilities
        .visible_guidance(&policy, &visibility, &second_state)
        .is_empty());
}

#[tokio::test]
async fn ability_changes_revoke_waiting_approvals_for_both_scopes() {
    use axum::http::Method;
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let cookie = ability_auth(&app).await;
    let agent_uri = "/api/v3/agents/shared_default/abilities/codebase_admin/visibility";
    let session_uri = format!("/api/v3/sessions/{sid}/abilities/codebase_admin");
    let (status, _) = ability_request(
        &app,
        Method::PUT,
        agent_uri,
        Some(json!({"visibility":"visible","expected_revision":0})),
        Some(&cookie),
        Some("ability-fixture"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rid = queued(&app, &sid).await;
    let args = json!({"path":"notes.txt","content":"test"});
    let mut run = app.run(&rid).await.unwrap();
    run["status"] = json!("waiting_approval");
    run["phase"] = json!("tools");
    run["calls"] = json!([{"type":"function_call","call_id":"test-call",
        "name":"file_write","arguments":args.to_string()}]);
    run["pending"] = json!({"id":"old-visibility-approval","name":"file_write",
        "arguments":args,"call_id":"test-call","digest":digest(&args),
        "policy_version":1,"mode_revision":0,"visibility_revision":1,
        "ability_state_revision":0,"state":"waiting","decision":null});
    app.save(&mut run).await.unwrap();
    let (status, _) = ability_request(
        &app,
        Method::PUT,
        agent_uri,
        Some(json!({"visibility":"discoverable","expected_revision":1})),
        Some(&cookie),
        Some("ability-fixture"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(app.run(&rid).await.unwrap()["status"], "stopped");
    assert_eq!(
        approval(
            State(app.clone()),
            Path("old-visibility-approval".into()),
            Json(json!({"client_request_id":uuid(),"decision":"approve"}))
        )
        .await
        .unwrap_err()
        .0,
        StatusCode::CONFLICT
    );

    let (status, _) = ability_request(
        &app,
        Method::PUT,
        &session_uri,
        Some(json!({"state":"active","expected_revision":0})),
        Some(&cookie),
        Some("ability-fixture"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let mut run = app.run(&rid).await.unwrap();
    run["status"] = json!("waiting_approval");
    run["pending"]["id"] = json!("old-session-approval");
    run["pending"]["state"] = json!("waiting");
    run["pending"]["decision"] = Value::Null;
    run["pending"]["visibility_revision"] = json!(2);
    run["pending"]["ability_state_revision"] = json!(1);
    app.save(&mut run).await.unwrap();
    let (status, _) = ability_request(
        &app,
        Method::PUT,
        &session_uri,
        Some(json!({"state":"suppressed","expected_revision":1})),
        Some(&cookie),
        Some("ability-fixture"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(app.run(&rid).await.unwrap()["status"], "queued");
    assert_eq!(
        approval(
            State(app.clone()),
            Path("old-session-approval".into()),
            Json(json!({"client_request_id":uuid(),"decision":"approve"}))
        )
        .await
        .unwrap_err()
        .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn session_mode_api_requires_auth_and_csrf_and_is_session_scoped() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let other = conversation(&app).await;
    let uri = format!("/api/v3/sessions/{sid}/mode");
    let get = |cookie: Option<&str>, uri: &str| {
        let mut request = Request::builder().uri(uri);
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        request.body(Body::empty()).unwrap()
    };
    let put = |cookie: Option<&str>, csrf: Option<&str>, mode: &str| {
        let mut request = Request::builder()
            .method("PUT")
            .uri(&uri)
            .header("content-type", "application/json");
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        if let Some(csrf) = csrf {
            request = request.header("x-csrf-token", csrf);
        }
        request
            .body(Body::from(json!({"mode":mode,"expected_revision":if mode=="write"{Some(1)}else{None},"confirm_write":mode=="write"}).to_string()))
            .unwrap()
    };
    let response = router(app.clone()).oneshot(get(None, &uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = router(app.clone())
        .oneshot(put(None, None, "read_only"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let token = uuid();
    app.store
        .put(
            &format!("auth:{}", digest(&json!(token))),
            &json!({"csrf":"fixture","expires":now()+3600,"generation":"fixture-generation"}),
        )
        .await
        .unwrap();
    let cookie = format!("v3_session={token}");
    let response = router(app.clone())
        .oneshot(put(Some(&cookie), None, "read_only"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = router(app.clone())
        .oneshot(get(Some(&cookie), &uri))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["mode"], "read_only");
    assert_eq!(body["execution_mode"], "ask");
    assert_eq!(body["revision"], 0);

    for (mode, revision) in [("read_only", 1), ("read_only", 1), ("write", 2)] {
        let response = router(app.clone())
            .oneshot(put(Some(&cookie), Some("fixture"), mode))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body, json!({"mode":mode,"revision":revision}));
        let other_uri = format!("/api/v3/sessions/{other}/mode");
        let other_response = router(app.clone())
            .oneshot(get(Some(&cookie), &other_uri))
            .await
            .unwrap();
        let other_body: Value = serde_json::from_slice(
            &axum::body::to_bytes(other_response.into_body(), 65536)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(other_body["mode"], "read_only");
        assert_eq!(other_body["execution_mode"], "ask");
    }
    let response = router(app)
        .oneshot(get(
            Some(&cookie),
            &format!("/api/v3/sessions/{}/mode", uuid()),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
/// Verify switching to read-only invalidates prior write approval and denies the queued write at execution.
async fn read_only_revokes_old_approval_and_denies_write_at_execution() {
    use axum::routing::post;

    let (_root, mut app) = fixture().await;
    let sid = conversation(&app).await;
    activate_codebase(&app, &sid).await;
    let id = queued(&app, &sid).await;
    let args = json!({"command":"printf forbidden"});
    let mut run = app.run(&id).await.unwrap();
    run["loaded_abilities"] = json!(["codebase_admin"]);
    run["status"] = json!("waiting_approval");
    run["phase"] = json!("tools");
    run["calls"] = json!([{"type":"function_call","call_id":"old-call",
        "name":"shell","arguments":args.to_string()}]);
    let calls = run["calls"].as_array().unwrap().clone();
    run["input"].as_array_mut().unwrap().extend(calls);
    run["pending"] = json!({"id":"old-approval","name":"shell","arguments":args,
        "call_id":"old-call","digest":digest(&args),"policy_version":1,
        "mode_revision":0,"state":"waiting","decision":null});
    app.save(&mut run).await.unwrap();

    let mode = session_mode_set(
        State(app.clone()),
        Path(sid.clone()),
        Json(SessionModeBody {
            mode: "read_only".into(),
            expected_revision: None,
            confirm_write: false,
        }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(mode, json!({"mode":"read_only","revision":1}));
    let run = app.run(&id).await.unwrap();
    assert_eq!(run["status"], "queued");
    assert!(run["pending"]["decision"].is_null());
    assert_eq!(
        approval(
            State(app.clone()),
            Path("old-approval".into()),
            Json(json!({"client_request_id":"old-decision","decision":"approve"})),
        )
        .await
        .unwrap_err()
        .0,
        StatusCode::CONFLICT
    );
    let (policy, revision) = effective_policy(&app, &sid).await.unwrap();
    assert_eq!(revision, 1);
    assert_eq!(policy_decision(&policy, "shell"), "deny");
    assert_eq!(policy_decision(&policy, "state_inspect"), "allow");
    let schemas = app.abilities.schemas(&policy, &json!(["codebase_admin"]));
    assert!(!schemas.iter().any(|schema| schema["name"] == "shell"));
    assert!(schemas
        .iter()
        .any(|schema| schema["name"] == "state_inspect"));

    let mock = Router::new().route(
        "/responses",
        post(|| async {
            let event = json!({"type":"response.completed","response":{"status":"completed",
            "output":[{"type":"message","role":"assistant",
                "content":[{"type":"output_text","text":"Denied."}]}]}});
            (
                [("content-type", "text/event-stream")],
                format!("data: {event}\n\n"),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    running(&app, &id, "read-only-retry").await;
    execute_run(
        app.clone(),
        id.clone(),
        "read-only-retry".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let finished = app.run(&id).await.unwrap();
    assert_eq!(finished["status"], "completed", "{}", finished["error"]);
    let output = finished["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    assert!(output["output"].as_str().unwrap().contains("denied"));
    server.abort();
}

#[test]
fn messaging_header_adapter_delegates_fail_closed() {
    let token = "fixture-messaging-token-0123456789";
    let hash = message_api_token_hash(token).unwrap();
    let mut headers = HeaderMap::new();
    assert!(!message_api_authorized(hash.as_ref(), &headers));
    headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
    assert!(message_api_authorized(hash.as_ref(), &headers));
    assert!(!message_api_authorized(None, &headers));
    headers.insert(
        "authorization",
        HeaderValue::from_static("Bearer incorrect"),
    );
    assert!(!message_api_authorized(hash.as_ref(), &headers));
}

#[tokio::test]
async fn messaging_api_is_scoped_idempotent_and_preserves_shutoff() {
    let (_root, mut app) = fixture().await;
    let token = "fixture-messaging-token-0123456789";
    let request = |method: &str, path: &str, body: Value, bearer: &str| {
        Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("authorization", bearer)
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let base = "/api/v3/admin/messaging/sessions";
    let body = json!({"client_request_id":"machine-session","title":"API chat"});
    let bearer = format!("Bearer {token}");
    assert_eq!(
        router(app.clone())
            .oneshot(request("POST", base, body.clone(), &bearer))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    app.message_api_token_hash = message_api_token_hash(token).unwrap();
    assert_eq!(
        router(app.clone())
            .oneshot(request("POST", base, body.clone(), "Bearer wrong"))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let response = router(app.clone())
        .oneshot(request("POST", base, body.clone(), &bearer))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let created: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 1048576)
            .await
            .unwrap(),
    )
    .unwrap();
    let response = router(app.clone())
        .oneshot(request("POST", base, body, &bearer))
        .await
        .unwrap();
    let repeated: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 1048576)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(created, repeated);
    let sid = created["id"].as_str().unwrap();
    let path = format!("{base}/{sid}/messages");
    let message = json!({"client_message_id":"machine-message","text":"Hello"});
    for _ in 0..2 {
        assert_eq!(
            router(app.clone())
                .oneshot(request("POST", &path, message.clone(), &bearer))
                .await
                .unwrap()
                .status(),
            StatusCode::ACCEPTED
        );
    }
    let response = router(app.clone())
        .oneshot(request(
            "GET",
            &format!("{base}/{sid}"),
            json!(null),
            &bearer,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let state: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 1048576)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(state["runs"].as_array().unwrap().len(), 1);
    assert_eq!(state["messages"].as_array().unwrap().len(), 1);
    app.store
        .put("control", &json!({"shutoff":true}))
        .await
        .unwrap();
    assert_eq!(
        router(app.clone())
            .oneshot(request(
                "POST",
                &path,
                json!({"client_message_id":"another","text":"Hello"}),
                &bearer
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    // The machine credential cannot authorize the general admin API.
    assert_eq!(
        router(app)
            .oneshot(request("GET", "/api/v3/status", json!(null), &bearer))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn workflow_policy_is_authenticated_validated_and_live() {
    let (_root, app) = fixture().await;
    let response = router(app.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v3/workflow/policy")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(
        workflow_policy_set(State(app.clone()), Json(json!({"max_turns":51})))
            .await
            .is_err()
    );
    assert!(app.store.get("workflow_policy").await.unwrap().is_none());
    let first = current_workflow_policy(&app)
        .await
        .unwrap()
        .revision()
        .unwrap();
    let mut loop_peer = app.clone();
    loop_peer.runtime = Arc::new(RwLock::new(loop_runtime::Config::default()));
    workflow_policy_set(State(app.clone()), Json(json!({"max_turns":2})))
        .await
        .unwrap();
    let current = current_workflow_policy(&app).await.unwrap();
    assert_eq!(current.max_turns, 2);
    assert_ne!(first, current.revision().unwrap());
    assert!(workflow_policy_set(
        State(app.clone()),
        Json(json!({"modules":{"publication":["uninstalled"]}}))
    )
    .await
    .is_err());
    assert_eq!(current_workflow_policy(&app).await.unwrap().max_turns, 2);
    assert_eq!(
        current_workflow_policy(&loop_peer).await.unwrap().max_turns,
        2
    );
    std::fs::write(&app.runtime_path, b"{}").unwrap();
    assert!(current_workflow_policy(&loop_peer).await.is_err());
}

#[tokio::test]
async fn publication_commits_one_terminal_message_and_private_receipt() {
    let (_root, app) = fixture().await;
    let sid = uuid();
    let id = uuid();
    app.store
        .put(
            &key("session", &sid),
            &json!({"id":sid,"title":"Publication test"}),
        )
        .await
        .unwrap();
    let mut run = json!({"id":id,"session_id":sid,"status":"running","phase":"tools",
        "generation":"g1","input":[],"turns":1,"call_index":0,"updated_at":now()});
    let policy = workflow::Policy::resolve(None).unwrap();
    assert!(
        publication::terminal(&app.store, &mut run, &[], "", &policy)
            .await
            .is_err()
    );
    assert_eq!(run["status"], "running");
    let output = vec![
        json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}),
    ];
    publication::terminal(&app.store, &mut run, &output, "done", &policy)
        .await
        .unwrap();
    publication::terminal(&app.store, &mut run, &output, "done", &policy)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM chat WHERE terminal_run_id=?")
        .bind(&id)
        .fetch_one(&app.store.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        app.run(&id).await.unwrap()["workflow"]["receipts"]["publication"]["result"]["decision"],
        "pass"
    );
}

#[tokio::test]
/// Verify uncertain-checkpoint reconciliation persists the accepted result idempotently and publishes only one final reply.
async fn uncertain_write_recovery_decisions_preserve_effects_and_publish_once() {
    use axum::routing::post;
    let (root, mut app) = fixture().await;
    let mock = Router::new().route(
        "/responses",
        post(|| async {
            let event = json!({"type":"response.completed","response":{"status":"completed",
            "output":[{"type":"message","role":"assistant",
                "content":[{"type":"output_text","text":"Recovered reply."}]}]}});
            (
                [("content-type", "text/event-stream")],
                format!("data: {event}\n\n"),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));

    for decision in ["accept", "retry", "abandon"] {
        let sid = conversation(&app).await;
        let id = queued(&app, &sid).await;
        let mut run = app.run(&id).await.unwrap();
        run["status"] = json!("recovery");
        run["phase"] = json!("tools");
        run["uncertain"] = json!(true);
        run["calls"] = json!([{"type":"function_call","call_id":"effect-call",
            "name":"file_write","arguments":"{\"path\":\"effect.txt\",\"content\":\"content\"}"}]);
        let calls = run["calls"].as_array().unwrap().clone();
        run["input"].as_array_mut().unwrap().extend(calls);
        run["pending"] = json!({"id":"effect-approval","name":"file_write",
            "call_id":"effect-call","state":"executing","decision":"consumed"});
        workflow::record(
            &mut run,
            &workflow::Policy::resolve(None).unwrap(),
            &checkpoint_pre_tool("file_write"),
            workflow::Decision::Pass,
        )
        .unwrap();
        app.save(&mut run).await.unwrap();

        let mut body =
            json!({"client_request_id":format!("review-{decision}"),"decision":decision});
        if decision == "accept" {
            body["output"] = json!("confirmed effect");
        }
        let first = command(
            State(app.clone()),
            Path((id.clone(), "recovery".into())),
            Json(body.clone()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(
            first["status"],
            match decision {
                "accept" | "retry" => "queued",
                _ => "cancelled",
            }
        );
        assert_eq!(first["uncertain"], false);
        let again = command(
            State(app.clone()),
            Path((id.clone(), "recovery".into())),
            Json(body),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(again, first);
        let reviewed = app.run(&id).await.unwrap();
        let (chat, _) = app.store.chat_snapshot(&sid).await.unwrap();
        assert_eq!(chat.len(), 1);
        assert_eq!(chat[0]["role"], "user");

        match decision {
            "accept" => {
                assert!(reviewed["pending"].is_null());
                assert_eq!(reviewed["call_index"], 1);
                assert_eq!(
                    reviewed["input"].as_array().unwrap().last().unwrap()["call_id"],
                    "effect-call"
                );
                assert_eq!(
                    reviewed["workflow"]["receipts"]["tool_result"]["result"]["decision"],
                    "pass"
                );
                running(&app, &id, "recovered-generation").await;
                execute_run(
                    app.clone(),
                    id.clone(),
                    "recovered-generation".into(),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
                assert_eq!(app.run(&id).await.unwrap()["status"], "completed");
                let (chat, _) = app.store.chat_snapshot(&sid).await.unwrap();
                assert_eq!(
                    chat.iter()
                        .filter(|item| item["role"] == "assistant")
                        .count(),
                    1
                );
                assert_eq!(chat[1]["content"], "Recovered reply.");
                let reopened = Store::open(root.path()).await.unwrap();
                let (durable_chat, _) = reopened.chat_snapshot(&sid).await.unwrap();
                assert_eq!(
                    durable_chat
                        .iter()
                        .filter(|item| item["role"] == "assistant")
                        .count(),
                    1
                );
            }
            "retry" => {
                assert_eq!(reviewed["pending"]["state"], "requested");
                assert!(reviewed["pending"]["decision"].is_null());
                assert_eq!(reviewed["call_index"], 0);
            }
            _ => {
                assert_eq!(reviewed["status"], "cancelled");
                assert_eq!(reviewed["call_index"], 0);
            }
        }
    }
    server.abort();
}

#[test]
fn late_known_write_result_resolves_cancel_and_shutoff_without_replay() {
    let policy = workflow::Policy::resolve(None).unwrap();
    for (status, error, expected) in [
        ("recovery", "Cancelled by user", "cancelled"),
        ("stopped", "Stopped by global shutoff", "stopped"),
    ] {
        let mut run = json!({"status":status,"uncertain":true,"error":error,
            "generation":"dispatch","turns":1,"call_index":0,"input":[],
            "pending":{"call_id":"effect-call","name":"file_write","state":"executing"}});
        workflow::record(
            &mut run,
            &policy,
            &checkpoint_pre_tool("file_write"),
            workflow::Decision::Pass,
        )
        .unwrap();
        let result = json!({"status":"success","output":"known effect"});
        assert!(reconcile_late_tool_result(&mut run, Ok(result)).unwrap());
        assert_eq!(run["status"], expected);
        assert_eq!(run["uncertain"], false);
        assert!(run["pending"].is_null());
        assert_eq!(run["input"].as_array().unwrap().len(), 1);
        assert_eq!(run["input"][0]["call_id"], "effect-call");
        assert!(!reconcile_late_tool_result(
            &mut run,
            Ok(json!({"status":"success","output":"duplicate"}))
        )
        .unwrap());
        assert_eq!(run["input"].as_array().unwrap().len(), 1);
    }

    let mut unknown = json!({"status":"recovery","uncertain":true,
        "error":"Cancelled by user","input":[],"pending":{"call_id":"effect-call"}});
    assert!(!reconcile_late_tool_result(
        &mut unknown,
        Ok(json!({"status":"uncertain","output":"unknown"}))
    )
    .unwrap());
    assert_eq!(unknown["status"], "recovery");
    assert_eq!(unknown["uncertain"], true);
}

#[tokio::test]
/// Verify each call receives its current mode suffix while durable transcript and base instructions remain free of that call-scoped contribution.
async fn current_mode_contribution_changes_per_call_without_entering_history() {
    use axum::routing::post;
    let (_root, mut app) = fixture().await;
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let mock = Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured.clone();
            async move {
                captured.lock().await.push(body);
                let event = json!({"type":"response.completed","response":{"status":"completed",
                    "output":[{"type":"message","role":"assistant",
                        "content":[{"type":"output_text","text":"Done."}]}]}});
                (
                    [("content-type", "text/event-stream")],
                    format!("data: {event}\n\n"),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    let sid = conversation(&app).await;

    let first = queued(&app, &sid).await;
    running(&app, &first, "write-mode").await;
    execute_run(
        app.clone(),
        first.clone(),
        "write-mode".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(app.run(&first).await.unwrap()["status"], "completed");
    assert_eq!(
        session_mode_set(
            State(app.clone()),
            Path(sid.clone()),
            Json(SessionModeBody {
                mode: "plan".into(),
                expected_revision: None,
                confirm_write: false
            })
        )
        .await
        .unwrap()
        .0["revision"],
        1
    );

    let second = queued(&app, &sid).await;
    running(&app, &second, "read-mode").await;
    execute_run(
        app.clone(),
        second.clone(),
        "read-mode".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(app.run(&second).await.unwrap()["status"], "completed");
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 2);
    let first_suffix = native_call_suffix(&requests[0]);
    let second_suffix = native_call_suffix(&requests[1]);
    assert!(first_suffix.contains("Current session capability mode: read_only (revision 0)"));
    assert!(!first_suffix.contains("Current session capability mode: read_only (revision 1)"));
    assert!(second_suffix.contains("Current session capability mode: read_only (revision 1)"));
    assert!(!second_suffix.contains("Current session capability mode: read_only (revision 0)"));
    assert!(first_suffix.contains("Execution mode: Ask"));
    assert!(!first_suffix.contains("Execution mode: Plan"));
    assert!(second_suffix.contains("Execution mode: Plan"));
    assert!(!second_suffix.contains("Execution mode: Ask"));
    for request in requests.iter() {
        assert!(!request["instructions"]
            .as_str()
            .unwrap()
            .contains("Current session capability mode:"));
    }
    for id in [&first, &second] {
        let durable = app.run(id).await.unwrap();
        assert!(!durable["input"]
            .to_string()
            .contains("Current session capability mode:"));
    }
    assert!(!json!(app.store.context(&sid).await.unwrap())
        .to_string()
        .contains("Current session capability mode:"));
    server.abort();
}

#[tokio::test]
async fn diagnostics_route_is_authenticated_session_bound_and_redacted() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let other = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let sentinel = "secret tool arguments 8319";
    let mut run = app.run(&id).await.unwrap();
    run["pending"] = json!({"arguments":{"content":sentinel}});
    run["latest_tool_failure"] = json!({"version":1,"stage":"authorization","code":"authorization_rejected","delivery":"not_sent","elapsed_ms":3,"recorded_at":0,"raw_error":sentinel});
    run["context_assembly_report"] = json!({"version":1,"scope":"logical_call_envelope",
        "envelope_bytes":8192,"max_bytes":262144,"history_json_bytes":1024,
        "instruction_json_bytes":4096,"schemas_json_bytes":2048,"turn":0,"call":0,
        "private_overlay":sentinel});
    workflow::record(
        &mut run,
        &workflow::Policy::resolve(None).unwrap(),
        &checkpoint_pre_tool("file_write"),
        workflow::Decision::Reject {
            reason: sentinel.into(),
        },
    )
    .unwrap();
    app.save(&mut run).await.unwrap();
    let uri = format!("/api/v3/sessions/{sid}/runs/{id}/diagnostics");
    let request = |cookie: Option<&str>, uri: &str| {
        let mut builder = Request::builder().uri(uri);
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", cookie);
        }
        builder.body(Body::empty()).unwrap()
    };
    assert_eq!(
        router(app.clone())
            .oneshot(request(None, &uri))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let cookie = ability_auth(&app).await;
    assert_eq!(
        router(app.clone())
            .oneshot(request(
                Some(&cookie),
                &format!("/api/v3/sessions/{other}/runs/{id}/diagnostics")
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let response = router(app.clone())
        .oneshot(request(Some(&cookie), &uri))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains(sentinel));
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["run_id"], id);
    assert_eq!(body["session_id"], sid);
    assert_eq!(body["status"], "queued");
    assert_eq!(
        body["checkpoints"].as_array().unwrap().last().unwrap()["checkpoint"],
        "pre_tool"
    );
    let denied = body["checkpoints"].as_array().unwrap().last().unwrap();
    assert_eq!(denied["decision"], "reject");
    assert_eq!(denied["reason_code"], "rejected");
    assert_eq!(denied.as_object().unwrap().len(), 6);
    assert_eq!(body["context_summary"]["envelope_bytes"], 8192);
    assert_eq!(
        body["context_summary"]["accounting"],
        "serialized_json_bytes_not_provider_tokens"
    );
    assert_eq!(body["context_summary"].as_object().unwrap().len(), 8);
    assert_eq!(body["reasoning_cleanup_logical_bytes_removed"], 0);
    assert!(body["latest_context_assembly"].is_null());
    assert_eq!(body["last_successful_assembly"], body["context_summary"]);
    assert_eq!(body["tool_failure"]["code"], "authorization_rejected");
    assert!(body["tool_failure"]["raw_error"].is_null());
    assert_eq!(body.as_object().unwrap().len(), 11);
    assert!(body["ability_context"].is_null());
}

#[tokio::test]
async fn progress_route_is_session_bound_and_tracks_durable_states() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let other = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let uri = format!("/api/v3/sessions/{sid}/runs/{id}/progress");
    let request = |cookie: Option<&str>, uri: &str| {
        let mut builder = Request::builder().uri(uri);
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", cookie);
        }
        builder.body(Body::empty()).unwrap()
    };
    assert_eq!(
        router(app.clone())
            .oneshot(request(None, &uri))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let cookie = ability_auth(&app).await;
    assert_eq!(
        router(app.clone())
            .oneshot(request(
                Some(&cookie),
                &format!("/api/v3/sessions/{other}/runs/{id}/progress")
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let queued_response = router(app.clone())
        .oneshot(request(Some(&cookie), &uri))
        .await
        .unwrap();
    assert_eq!(queued_response.status(), StatusCode::OK);
    let queued_body = axum::body::to_bytes(queued_response.into_body(), 65536)
        .await
        .unwrap();
    let queued_view: Value = serde_json::from_slice(&queued_body).unwrap();
    assert_eq!(queued_view["status"], "queued");
    assert_eq!(
        queued_view["checkpoint_decisions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(queued_view["compaction_count"], 0);
    assert_eq!(queued_view["pending_approval"], false);

    let sentinel = "private tool arguments 5917";
    let mut run = app.run(&id).await.unwrap();
    run["status"] = json!("waiting_approval");
    run["phase"] = json!("tools");
    run["turns"] = json!(1);
    run["pending"] = json!({"state":"waiting","name":"file_write",
        "arguments":{"content":sentinel}});
    workflow::record(
        &mut run,
        &workflow::Policy::resolve(None).unwrap(),
        &checkpoint_pre_tool("file_write"),
        workflow::Decision::Pass,
    )
    .unwrap();
    app.save(&mut run).await.unwrap();
    let waiting_response = router(app.clone())
        .oneshot(request(Some(&cookie), &uri))
        .await
        .unwrap();
    assert_eq!(waiting_response.status(), StatusCode::OK);
    let waiting_body = axum::body::to_bytes(waiting_response.into_body(), 65536)
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&waiting_body).contains(sentinel));
    assert!(!String::from_utf8_lossy(&waiting_body).contains("file_write"));
    let waiting_view: Value = serde_json::from_slice(&waiting_body).unwrap();
    assert_eq!(waiting_view["pending_approval"], true);
    assert!(waiting_view["checkpoint_decisions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|receipt| receipt["checkpoint"] == "pre_tool" && receipt["decision"] == "pass"));
    assert!(waiting_view["tool_failure"].is_null());
    assert_eq!(waiting_view.as_object().unwrap().len(), 13);
    assert!(waiting_view["closing"].is_null());

    run["status"] = json!("recovery");
    run["uncertain"] = json!(true);
    run["pending"]["state"] = json!("executing");
    app.save(&mut run).await.unwrap();
    let recovery_response = router(app.clone())
        .oneshot(request(Some(&cookie), &uri))
        .await
        .unwrap();
    let recovery_body = axum::body::to_bytes(recovery_response.into_body(), 65536)
        .await
        .unwrap();
    let recovery_view: Value = serde_json::from_slice(&recovery_body).unwrap();
    assert_eq!(recovery_view["status"], "recovery");
    assert_eq!(recovery_view["uncertain_effect"], true);
    assert_eq!(recovery_view["pending_approval"], false);

    run["status"] = json!("completed");
    run["uncertain"] = json!(false);
    run["compactions"] = json!(1);
    run["pending"] = Value::Null;
    workflow::record(
        &mut run,
        &workflow::Policy::resolve(None).unwrap(),
        &workflow::Input::Publication {
            text_digest: "fixture-digest".into(),
        },
        workflow::Decision::Pass,
    )
    .unwrap();
    app.save(&mut run).await.unwrap();
    let completed_response = router(app.clone())
        .oneshot(request(Some(&cookie), &uri))
        .await
        .unwrap();
    let completed_body = axum::body::to_bytes(completed_response.into_body(), 65536)
        .await
        .unwrap();
    let completed_view: Value = serde_json::from_slice(&completed_body).unwrap();
    assert_eq!(completed_view["status"], "completed");
    assert_eq!(completed_view["compaction_count"], 1);
    assert!(completed_view["checkpoint_decisions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|receipt| receipt["checkpoint"] == "publication" && receipt["decision"] == "pass"));
    assert_eq!(completed_view["pending_approval"], false);
}

#[tokio::test]
async fn failed_publication_preserves_the_callers_run() {
    let (_root, app) = fixture().await;
    let mut run = json!({"id":uuid(),"session_id":uuid(),"status":"running",
        "phase":"tools","generation":"publication-failure","input":[],
        "turns":1,"call_index":0,"updated_at":now()});
    let before = run.clone();
    let policy = workflow::Policy::resolve(None).unwrap();
    // Close only this test fixture's pool; no live database is touched.
    app.store.pool.close().await;
    let output = vec![json!({"type":"message","role":"assistant",
        "content":[{"type":"output_text","text":"done"}]})];
    assert!(
        publication::terminal(&app.store, &mut run, &output, "done", &policy)
            .await
            .is_err()
    );
    assert_eq!(run, before);
}

#[tokio::test]
async fn admission_records_policy_and_explicitly_rejects_attachments() {
    let (_root, app) = fixture().await;
    let Json(session) = create_session(
        State(app.clone()),
        Json(json!({"client_request_id":"workflow-admit","title":"Workflow"})),
    )
    .await
    .unwrap();
    let sid = session["id"].as_str().unwrap().to_owned();
    let result=send(State(app.clone()),Path(sid.clone()),Json(json!({"client_message_id":"attachment","text":"hello","attachments":[{"id":"unsupported"}]}))).await;
    assert!(matches!(result, Err(ApiError(StatusCode::BAD_REQUEST, _))));
    let (_, Json(accepted)) = send(
        State(app.clone()),
        Path(sid),
        Json(json!({"client_message_id":"text","text":"hello"})),
    )
    .await
    .unwrap();
    let run = app.run(accepted["run_id"].as_str().unwrap()).await.unwrap();
    assert_eq!(
        run["workflow"]["receipts"]["admission"]["result"]["decision"],
        "pass"
    );
    assert_eq!(
        run["workflow"]["receipts"]["attachment_preparation"]["result"]["decision"],
        "pass"
    );
    assert_eq!(
        run["workflow"]["receipts"]["admission"]["policy_revision"],
        current_workflow_policy(&app)
            .await
            .unwrap()
            .revision()
            .unwrap()
    );
}

#[tokio::test]
async fn text_attachment_is_saved_once_and_scoped_to_its_session() {
    let (_root, app) = fixture().await;
    let Json(session) = create_session(
        State(app.clone()),
        Json(json!({"client_request_id":"attachment-session","title":"Attachments"})),
    )
    .await
    .unwrap();
    let sid = session["id"].as_str().unwrap().to_owned();
    let body = json!({"client_message_id":"attachment-once","text":"Summarize the note",
        "attachments":[{"name":"note.txt","media_type":"text/plain","text":"Source fact 42"}]});
    let (_, Json(first)) = send(State(app.clone()), Path(sid.clone()), Json(body.clone()))
        .await
        .unwrap();
    let (_, Json(retry)) = send(State(app.clone()), Path(sid.clone()), Json(body))
        .await
        .unwrap();
    assert_eq!(first, retry);
    let aid = first["attachments"][0]["id"].as_str().unwrap().to_owned();
    let run = app.run(first["run_id"].as_str().unwrap()).await.unwrap();
    assert_eq!(
        run["attachments"][0]["sha256"],
        first["attachments"][0]["sha256"]
    );
    assert_eq!(
        run["workflow"]["receipts"]["attachment_preparation"]["result"]["decision"],
        "pass"
    );
    let (chat, _) = app.store.chat_snapshot(&sid).await.unwrap();
    assert_eq!(chat.len(), 1);
    assert_eq!(chat[0]["content"], "Summarize the note");
    let context = app.store.context(&sid).await.unwrap();
    assert_eq!(context.len(), 1);
    assert!(context[0]["content"]
        .as_str()
        .unwrap()
        .contains("Source fact 42"));
    let Json(saved) = attachment_get(State(app.clone()), Path((sid.clone(), aid.clone())))
        .await
        .unwrap();
    assert_eq!(saved["text"], "Source fact 42");
    let token = uuid();
    app.store
        .put(
            &format!("auth:{}", digest(&json!(token))),
            &json!({"csrf":"fixture","expires":now()+3600,
            "generation":"fixture-generation"}),
        )
        .await
        .unwrap();
    let response = router(app.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/v3/sessions/{sid}/attachments/{aid}"))
                .header("cookie", format!("v3_session={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let Json(other) = create_session(
        State(app.clone()),
        Json(json!({"client_request_id":"other-attachment-session","title":"Other"})),
    )
    .await
    .unwrap();
    let other_sid = other["id"].as_str().unwrap().to_owned();
    assert!(matches!(
        attachment_get(State(app), Path((other_sid, aid))).await,
        Err(ApiError(StatusCode::NOT_FOUND, _))
    ));
}

#[tokio::test]
async fn completed_run_memory_is_scoped_and_frozen_for_next_message() {
    let (_root, app) = fixture().await;
    let Json(session) = create_session(
        State(app.clone()),
        Json(json!({"client_request_id":"memory-session","title":"Memory"})),
    )
    .await
    .unwrap();
    let sid = session["id"].as_str().unwrap().to_owned();
    let (_, Json(first)) = send(
        State(app.clone()),
        Path(sid.clone()),
        Json(json!({"client_message_id":"memory-source","text":"First turn"})),
    )
    .await
    .unwrap();
    let rid = first["run_id"].as_str().unwrap().to_owned();
    let id = uuid();
    let body = json!({"source_run_id":rid,"client_memory_id":id,"text":"The folder is blue"});
    assert!(matches!(
        memory_save(State(app.clone()), Path(sid.clone()), Json(body.clone())).await,
        Err(ApiError(StatusCode::CONFLICT, _))
    ));
    let mut run = app.run(&rid).await.unwrap();
    run["status"] = json!("completed");
    app.store.put(&key("run", &rid), &run).await.unwrap();
    let (created, Json(saved)) =
        memory_save(State(app.clone()), Path(sid.clone()), Json(body.clone()))
            .await
            .unwrap();
    assert_eq!(created, StatusCode::CREATED);
    assert_eq!(saved["text"], "The folder is blue");
    assert_eq!(
        memory_save(State(app.clone()), Path(sid.clone()), Json(body.clone()))
            .await
            .unwrap()
            .0,
        StatusCode::OK
    );
    let Json(other) = create_session(
        State(app.clone()),
        Json(json!({"client_request_id":"memory-other","title":"Other"})),
    )
    .await
    .unwrap();
    let other_sid = other["id"].as_str().unwrap().to_owned();
    assert!(matches!(
        memory_delete(State(app.clone()), Path((other_sid, id.clone()))).await,
        Err(ApiError(StatusCode::NOT_FOUND, _))
    ));
    let (_, Json(next)) = send(
        State(app.clone()),
        Path(sid.clone()),
        Json(json!({"client_message_id":"memory-next","text":"Second turn"})),
    )
    .await
    .unwrap();
    let next_run = app.run(next["run_id"].as_str().unwrap()).await.unwrap();
    let context = next_run["memory_context"].as_str().unwrap();
    assert!(context.contains("The folder is blue"));
    let mut input = vec![json!({"role":"user","content":"Second turn"})];
    memory::insert(&mut input, Some(context));
    assert_eq!(input[0]["role"], "user");
    assert!(input[0]["content"]
        .as_str()
        .unwrap()
        .contains("untrusted user data"));
    assert_eq!(input[1]["content"], "Second turn");
    let Json(list) = memory_list(State(app.clone()), Path(sid.clone()))
        .await
        .unwrap();
    assert_eq!(list["memories"].as_array().unwrap().len(), 1);
    let token = uuid();
    app.store
        .put(
            &format!("auth:{}", digest(&json!(token))),
            &json!({"csrf":"fixture","expires":now()+3600,
            "generation":"fixture-generation"}),
        )
        .await
        .unwrap();
    let uri = format!("/api/v3/sessions/{sid}/memories");
    let unauth = router(app.clone())
        .oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);
    let forbidden = router(app.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&uri)
                .header("cookie", format!("v3_session={token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    let accepted = router(app.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&uri)
                .header("cookie", format!("v3_session={token}"))
                .header("x-csrf-token", "fixture")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);
    let deleted = router(app.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("{uri}/{id}"))
                .header("cookie", format!("v3_session={token}"))
                .header("x-csrf-token", "fixture")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    assert!(
        memory_list(State(app), Path(sid)).await.unwrap().0["memories"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn tool_result_checkpoint_stays_private_and_binds_dispatch() {
    let policy = workflow::Policy::resolve(None).unwrap();
    let mut run = json!({"generation":"g1","turns":1,"call_index":0,"input":[],
        "pending":{"call_id":"c1"}});
    workflow::record(
        &mut run,
        &policy,
        &checkpoint_pre_tool("read"),
        workflow::Decision::Pass,
    )
    .unwrap();
    // Results must retain dispatch identity even if the caller's generation changes.
    run["generation"] = json!("g2");
    let result = json!({"status":"denied","output":"test"});
    append_tool(&mut run, &result).unwrap();
    assert_eq!(
        run["workflow"]["receipts"]["tool_result"]["generation"],
        "g1"
    );
    assert_eq!(
        run["workflow"]["receipts"]["tool_result"]["input_digest"],
        workflow::digest_input(&workflow::Input::ToolResult(workflow::ToolResultInput {
            status: "denied".into(),
            result_digest: digest(&result),
        }))
        .unwrap()
    );
    assert_eq!(
        run["workflow"]["receipts"]["tool_result"]["policy_revision"],
        policy.revision().unwrap()
    );
    assert_eq!(
        run["workflow"]["receipts"]["tool_result"]["result"]["decision"],
        "reject"
    );
    assert_eq!(
        run["workflow"]["receipts"]["pre_tool"]["result"]["decision"],
        "pass"
    );
    assert!(run["input"][0].get("workflow").is_none());
    assert_eq!(run["input"][0]["type"], "function_call_output");
}

#[test]
fn successful_tool_result_records_pass_without_changing_dispatch_identity() {
    let policy = workflow::Policy::resolve(None).unwrap();
    let mut run = json!({"generation":"dispatch","turns":2,"call_index":3,
        "input":[],"pending":{"call_id":"success-call"}});
    workflow::record(
        &mut run,
        &policy,
        &checkpoint_pre_tool("read"),
        workflow::Decision::Pass,
    )
    .unwrap();
    let dispatch = run["workflow"]["receipts"]["pre_tool"].clone();
    run["generation"] = json!("later-generation");
    let result = json!({"status":"success","output":"fixture result"});
    append_tool(&mut run, &result).unwrap();
    let receipt = &run["workflow"]["receipts"]["tool_result"];
    assert_eq!(receipt["result"]["decision"], "pass");
    for field in ["generation", "policy_revision", "turn", "call_index"] {
        assert_eq!(receipt[field], dispatch[field]);
    }
    assert_eq!(
        receipt["input_digest"],
        workflow::digest_input(&workflow::Input::ToolResult(workflow::ToolResultInput {
            status: "success".into(),
            result_digest: digest(&result),
        }))
        .unwrap()
    );
    assert_eq!(run["workflow"]["receipts"]["pre_tool"], dispatch);
    assert_eq!(run["call_index"], 4);
    assert!(run["pending"].is_null());
    assert_eq!(run["input"][0]["call_id"], "success-call");
    assert_eq!(run["input"][0]["output"], result.to_string());
    assert!(run["input"][0].get("workflow").is_none());
}

// MODEL-SELECTION: agent-bound admission and dispatch safety, isolated fixtures only.
pub(crate) async fn selection_fixture(app: &App) -> model_selection::Catalog {
    use model_selection::*;
    use std::collections::{BTreeMap, BTreeSet};
    let agent_pool = crate::agent_storage::open_agent_pool(&app.store.root, "agent-a")
        .await
        .unwrap();
    let metadata = json!({"owner_id":"admin","ability_modes":{"codebase_admin":"visible"},
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
                revision: 1,
            },
        );
    }
    // Each tenant has an explicit roster and independent role appointment. Sharing
    // authorizes both the entry and its connection without relying on common ownership.
    for (agent, primary, backup) in [
        ("shared_default", "one", vec![]),
        ("agent-a", "two", vec!["one".into()]),
    ] {
        let revision = catalog
            .agent_models
            .get(agent)
            .map(|a| a.revision)
            .unwrap_or(1);
        catalog.agent_models.insert(
            agent.into(),
            AgentModels {
                limits: std::collections::BTreeMap::new(),
                efforts: std::collections::BTreeMap::new(),
                agent_id: agent.into(),
                owner_id: "admin".into(),
                entry_ids: vec!["one".into(), "two".into()],
                revision,
                roles: BTreeMap::from([
                    (
                        ModelRole::Standard,
                        RoleAssignment {
                            entry_id: primary.into(),
                            backup_entry_ids: backup,
                        },
                    ),
                    (
                        ModelRole::System,
                        RoleAssignment {
                            entry_id: "one".into(),
                            backup_entry_ids: vec!["two".into()],
                        },
                    ),
                ]),
            },
        );
    }
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap()
}

#[tokio::test]
async fn model_selection_admission_pins_agent_model_prompt_and_credentials() {
    let (_root, app) = fixture().await;
    let mut catalog = selection_fixture(&app).await;
    app.store
        .upsert_prompt_slot(
            "selection-isolation",
            "DEFAULT PRIVATE PROMPT",
            Some(5),
            None,
            None,
            0,
        )
        .await
        .unwrap();
    let agent = app.store.for_agent("agent-a").await.unwrap();
    sqlx::query("INSERT INTO agent_prompts(id,agent_id,slot_name,order_index,lock,merge_mode,content,revision,updated_at) VALUES('agent-a:selection-isolation','agent-a','selection-isolation',5,0,'replace',?,1,0)")
        .bind(agent.seal_agent(&json!("AGENT A PRIVATE PROMPT")).unwrap()).execute(&agent.agent_pool).await.unwrap();
    for (agent_id, expected_model, expected_token, expected_prompt, excluded) in [
        (
            "shared_default",
            "model-one",
            "token-one",
            "DEFAULT PRIVATE PROMPT",
            "AGENT A PRIVATE PROMPT",
        ),
        (
            "agent-a",
            "model-two",
            "token-two",
            "AGENT A PRIVATE PROMPT",
            "DEFAULT PRIVATE PROMPT",
        ),
    ] {
        let Json(session) = create_session(
            State(app.clone()),
            Json(json!({"client_request_id":uuid(),"agent_id":agent_id})),
        )
        .await
        .unwrap();
        let sid = session["id"].as_str().unwrap().to_owned();
        assert_eq!(session_agent_id(&app, &sid).await.unwrap(), agent_id);
        let (_, Json(accepted)) = send(
            State(app.clone()),
            Path(sid.clone()),
            Json(json!({"client_message_id":uuid(),"text":"hello"})),
        )
        .await
        .unwrap();
        let run = app.run(accepted["run_id"].as_str().unwrap()).await.unwrap();
        assert_eq!(run["agent_id"], agent_id);
        assert_eq!(run["provider_selection"]["model"], expected_model);
        assert!(run["instructions"]
            .as_str()
            .unwrap()
            .contains(expected_prompt));
        assert!(!run["instructions"].as_str().unwrap().contains(excluded));
        let selection =
            provider::ProviderSelection::parse(&run["provider_selection"], "unused").unwrap();
        assert_eq!(
            app.tokens_for(&selection, &run).await.unwrap()["access_token"],
            expected_token
        );
        if agent_id == "agent-a" {
            catalog
                .agent_models
                .get_mut(agent_id)
                .unwrap()
                .roles
                .insert(
                    model_selection::ModelRole::Standard,
                    model_selection::RoleAssignment {
                        entry_id: "one".into(),
                        backup_entry_ids: vec![],
                    },
                );
            catalog = app
                .store
                .save_model_catalog(&catalog, catalog.revision)
                .await
                .unwrap();
            assert_eq!(
                app.run_candidate(&run)
                    .await
                    .unwrap()
                    .unwrap()
                    .upstream_model,
                "model-two"
            );
            assert_eq!(
                app.tokens_for(&selection, &run).await.unwrap()["access_token"],
                "token-two"
            );
            let (policy, _) = effective_policy(&app, &sid).await.unwrap();
            assert_eq!(policy_decision(&policy, "prompt_slot_read"), "allow");
        }
    }
    assert_eq!(
        app.store
            .list_prompt_slots()
            .await
            .unwrap()
            .iter()
            .filter(|p| p["slot_name"] == "selection-isolation")
            .count(),
        1
    );
    assert_eq!(
        app.store
            .get_prompt_slot("selection-isolation")
            .await
            .unwrap()
            .unwrap()["content"],
        "DEFAULT PRIVATE PROMPT"
    );
}

#[tokio::test]
async fn model_selection_failover_is_ordered_durable_and_revocation_is_live() {
    let (_root, app) = fixture().await;
    let mut catalog = selection_fixture(&app).await;
    let Json(session) = create_session(
        State(app.clone()),
        Json(json!({"client_request_id":uuid(),"agent_id":"agent-a"})),
    )
    .await
    .unwrap();
    let (_, Json(accepted)) = send(
        State(app.clone()),
        Path(session["id"].as_str().unwrap().into()),
        Json(json!({"client_message_id":uuid(),"text":"hello"})),
    )
    .await
    .unwrap();
    let mut run = app.run(accepted["run_id"].as_str().unwrap()).await.unwrap();
    let failure = provider::typed_failure(
        model_selection::FailureKind::ModelUnavailable,
        false,
        "Model unavailable",
    );
    model_failure(&mut run, &failure).unwrap();
    assert_eq!(run["status"], "queued");
    assert_eq!(run["provider_selection"]["model"], "model-one");
    assert_eq!(run["failover_state"]["history"][0]["to_entry_id"], "one");
    app.save(&mut run).await.unwrap();
    let mut restored = app.run(accepted["run_id"].as_str().unwrap()).await.unwrap();
    assert_eq!(restored["failover_state"], run["failover_state"]);
    let selection =
        provider::ProviderSelection::parse(&restored["provider_selection"], "unused").unwrap();
    assert_eq!(
        app.tokens_for(&selection, &restored).await.unwrap()["access_token"],
        "token-one"
    );
    catalog.connections.get_mut("one").unwrap().revoked = true;
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
    assert!(app.run_candidate(&restored).await.is_err());
    model_failure(&mut restored, &failure).unwrap();
    assert_eq!(restored["status"], "failed");
    assert!(restored["error"]
        .as_str()
        .unwrap()
        .contains("no safe retry or eligible backup"));
}

#[tokio::test]
// A cleared role is authoritative even while legacy credentials still exist.
async fn model_roles_standard_is_required_even_with_legacy_default_and_other_roles() {
    let (_root, app) = fixture().await;
    let mut catalog = selection_fixture(&app).await;
    for agent in ["shared_default", "agent-a"] {
        catalog
            .agent_models
            .get_mut(agent)
            .unwrap()
            .roles
            .remove(&model_selection::ModelRole::Standard);
    }
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
    for agent in ["shared_default", "agent-a"] {
        let Json(session) = create_session(
            State(app.clone()),
            Json(json!({"client_request_id":uuid(),"agent_id":agent})),
        )
        .await
        .unwrap();
        let error = send(
            State(app.clone()),
            Path(session["id"].as_str().unwrap().into()),
            Json(json!({"client_message_id":uuid(),"text":"hello"})),
        )
        .await
        .unwrap_err();
        assert_eq!(error.0, StatusCode::CONFLICT);
        assert!(error.1.contains("Standard"));
        assert!(app.store.active_runs().await.unwrap().is_empty());
    }
}

#[tokio::test]
// Custom admission pins one permitted entry without mutating durable appointments.
async fn model_roles_custom_uses_roster_without_reassigning_standard() {
    let (_root, app) = fixture().await;
    let catalog = selection_fixture(&app).await;
    let Json(session) = create_session(
        State(app.clone()),
        Json(json!({"client_request_id":uuid(),"agent_id":"agent-a"})),
    )
    .await
    .unwrap();
    let (_, Json(accepted)) = send(
        State(app.clone()),
        Path(session["id"].as_str().unwrap().into()),
        Json(json!({"client_message_id":uuid(),"text":"hello","model_entry_id":"one"})),
    )
    .await
    .unwrap();
    let run = app.run(accepted["run_id"].as_str().unwrap()).await.unwrap();
    assert_eq!(run["selection_plan"]["custom_entry_id"], "one");
    assert_eq!(run["provider_selection"]["model"], "model-one");
    assert_eq!(
        run["selection_plan"]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(app.store.model_catalog().await.unwrap(), catalog);
    assert_eq!(
        catalog.resolve("agent-a").unwrap().candidates[0].entry_id,
        "two"
    );
    let Json(other) = create_session(
        State(app.clone()),
        Json(json!({"client_request_id":uuid(),"agent_id":"agent-a"})),
    )
    .await
    .unwrap();
    assert!(send(
        State(app.clone()),
        Path(other["id"].as_str().unwrap().into()),
        Json(json!({"client_message_id":uuid(),"text":"hello","model_entry_id":"not-in-roster"}))
    )
    .await
    .is_err());
}

#[tokio::test]
// Compaction owns a separate pinned plan and retry state across storage recovery.
async fn model_roles_system_is_pinned_and_failover_is_separate_after_recovery() {
    let (root, app) = fixture().await;
    let mut catalog = selection_fixture(&app).await;
    let Json(session) = create_session(
        State(app.clone()),
        Json(json!({"client_request_id":uuid(),"agent_id":"agent-a"})),
    )
    .await
    .unwrap();
    let (_, Json(accepted)) = send(
        State(app.clone()),
        Path(session["id"].as_str().unwrap().into()),
        Json(json!({"client_message_id":uuid(),"text":"hello"})),
    )
    .await
    .unwrap();
    let id = accepted["run_id"].as_str().unwrap();
    let mut run = app.run(id).await.unwrap();
    let standard = run["selection_plan"].clone();
    let standard_state = run["failover_state"].clone();
    assert_eq!(run["system_selection_plan"]["role"], "system");
    assert_eq!(
        system_dispatch_run(&run).unwrap()["provider_selection"]["model"],
        "model-one"
    );
    catalog
        .agent_models
        .get_mut("agent-a")
        .unwrap()
        .roles
        .insert(
            model_selection::ModelRole::System,
            model_selection::RoleAssignment {
                entry_id: "two".into(),
                backup_entry_ids: vec![],
            },
        );
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
    system_model_failure(
        &mut run,
        &provider::typed_failure(
            model_selection::FailureKind::ModelUnavailable,
            false,
            "unavailable",
        ),
    )
    .unwrap();
    assert_eq!(run["selection_plan"], standard);
    assert_eq!(run["failover_state"], standard_state);
    assert_eq!(run["system_failover_state"]["candidate_index"], 1);
    assert_eq!(
        system_dispatch_run(&run).unwrap()["provider_selection"]["model"],
        "model-two"
    );
    app.save(&mut run).await.unwrap();
    let reopened = Store::open(root.path()).await.unwrap();
    let restored = reopened.get(&key("run", id)).await.unwrap().unwrap();
    assert_eq!(
        restored["system_selection_plan"],
        run["system_selection_plan"]
    );
    assert_eq!(
        restored["system_failover_state"],
        run["system_failover_state"]
    );
    assert_eq!(restored["selection_plan"], standard);
    catalog = app.store.model_catalog().await.unwrap();
    catalog.connections.get_mut("two").unwrap().credential_ref = "rotated-system-credential".into();
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
    assert!(app
        .run_candidate(&system_dispatch_run(&restored).unwrap())
        .await
        .is_err());
}

#[tokio::test]
async fn model_selection_unsafe_failures_do_not_change_candidate() {
    let (_root, app) = fixture().await;
    let catalog = selection_fixture(&app).await;
    let plan = catalog.resolve("agent-a").unwrap();
    for (kind, started, uncertain) in [
        (model_selection::FailureKind::MalformedRequest, false, false),
        (model_selection::FailureKind::Cancelled, false, false),
        (model_selection::FailureKind::PolicyRejected, false, false),
        (model_selection::FailureKind::Transient, true, false),
        (model_selection::FailureKind::Transient, false, true),
    ] {
        let mut run = json!({"agent_id":"agent-a","selection_plan":plan,"failover_state":model_selection::FailoverState::default(),
            "provider_selection":{"provider":"openai_subscription","model":"model-two"},"uncertain":uncertain});
        model_failure(
            &mut run,
            &provider::typed_failure(kind, started, "Sanitized failure"),
        )
        .unwrap();
        assert_eq!(run["status"], "failed");
        assert_eq!(run["failover_state"]["candidate_index"], 0);
        assert_eq!(run["provider_selection"]["model"], "model-two");
    }
}

#[tokio::test]
async fn model_catalog_api_uses_profile_admin_role_and_enforces_csrf_and_retries() {
    let (_root, app) = fixture().await;
    let token = uuid();
    app.store
        .put(
            &format!("auth:{}", digest(&json!(token))),
            &json!({"csrf":"catalog-csrf","expires":now()+3600,
        "generation":"fixture-generation","user_id":"admin"}),
        )
        .await
        .unwrap();
    let request = |method: &str, path: &str, cookie: bool, csrf: bool, body: Value| {
        let mut r = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if cookie {
            r = r.header("cookie", format!("v3_session={token}"));
        }
        if csrf {
            r = r.header("x-csrf-token", "catalog-csrf");
        }
        r.body(Body::from(body.to_string())).unwrap()
    };
    let response = router(app.clone())
        .oneshot(request(
            "GET",
            "/api/v3/models/catalog",
            false,
            false,
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = router(app.clone())
        .oneshot(request(
            "GET",
            "/api/v3/models/catalog",
            true,
            false,
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // Catalog compare-and-swap rejects a stale browser tab without replacing roles.
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap(),
    )
    .unwrap();
    let stale = body["catalog"].clone();
    let response = router(app.clone())
        .oneshot(request(
            "PUT",
            "/api/v3/models/catalog",
            true,
            true,
            stale.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = router(app.clone())
        .oneshot(request("PUT", "/api/v3/models/catalog", true, true, stale))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let new_agent = json!({"client_request_id":"catalog-agent","name":"Second Agent"});
    let response = router(app.clone())
        .oneshot(request(
            "POST",
            "/api/v3/models/agents",
            true,
            false,
            new_agent.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let mut created = Value::Null;
    for _ in 0..2 {
        let response = router(app.clone())
            .oneshot(request(
                "POST",
                "/api/v3/models/agents",
                true,
                true,
                new_agent.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap(),
        )
        .unwrap();
        if created.is_null() {
            created = body.clone();
            // Creating initial ownership must preserve the creator's authenticated session.
            let path = format!(
                "/api/v3/models/catalog?agent_id={}",
                body["id"].as_str().unwrap()
            );
            let response = router(app.clone())
                .oneshot(request("GET", &path, true, false, Value::Null))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        assert_eq!(body, created);
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agents WHERE id=?")
        .bind(created["id"].as_str().unwrap())
        .fetch_one(
            &app.store
                .for_agent(created["id"].as_str().unwrap())
                .await
                .unwrap()
                .agent_pool,
        )
        .await
        .unwrap();
    assert_eq!(count, 1);
    let response = router(app.clone())
        .oneshot(request(
            "POST",
            "/api/v3/models/agents",
            true,
            true,
            json!({"client_request_id":"catalog-agent","name":"Different Agent"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let mut profile = app.store.user_profile("admin").await.unwrap().unwrap();
    profile["is_admin"] = json!(false);
    sqlx::query("UPDATE user_profiles SET value=? WHERE user_id='admin'")
        .bind(app.store.seal(&profile).unwrap())
        .execute(&app.store.app_pool)
        .await
        .unwrap();
    let response = router(app.clone())
        .oneshot(request(
            "GET",
            "/api/v3/models/catalog",
            true,
            false,
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn deepseek_pins_transport_and_mixed_provider_backup() {
    let (_root, app) = fixture().await;
    let mut catalog = selection_fixture(&app).await;
    catalog.connections.get_mut("two").unwrap().transport = model_selection::Transport::Deepseek;
    catalog.entries.get_mut("two").unwrap().upstream_model = "deepseek-chat".into();
    app.store
        .put("credential:two", &json!({"api_key":"test-key-no-network"}))
        .await
        .unwrap();
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
    let Json(session) = create_session(
        State(app.clone()),
        Json(json!({"client_request_id":uuid(),"agent_id":"agent-a"})),
    )
    .await
    .unwrap();
    let (_, Json(accepted)) = send(
        State(app.clone()),
        Path(session["id"].as_str().unwrap().into()),
        Json(json!({"client_message_id":uuid(),"text":"hello"})),
    )
    .await
    .unwrap();
    let mut run = app.run(accepted["run_id"].as_str().unwrap()).await.unwrap();
    assert_eq!(run["provider_selection"]["provider"], "deepseek");
    let selection =
        provider::ProviderSelection::parse(&run["provider_selection"], "unused").unwrap();
    assert_eq!(
        app.tokens_for(&selection, &run).await.unwrap()["api_key"],
        "test-key-no-network"
    );
    let wrong = provider::ProviderSelection {
        limits: None,
        provider: provider::ProviderId::OpenaiSubscription,
        model: "deepseek-chat".into(),
    };
    assert!(app.tokens_for(&wrong, &run).await.is_err());
    model_failure(
        &mut run,
        &provider::typed_failure(
            model_selection::FailureKind::ModelUnavailable,
            false,
            "unavailable",
        ),
    )
    .unwrap();
    assert_eq!(run["provider_selection"]["provider"], "openai_subscription");
    assert_eq!(run["provider_selection"]["model"], "model-one");
}

#[tokio::test]
async fn lightweight_queue_keeps_history_durable_and_fifo_across_updates() {
    let (root, app) = fixture().await;
    let a = conversation(&app).await;
    let b = conversation(&app).await;
    let first = queued(&app, &a).await;
    let second = queued(&app, &b).await;
    let mut run = app.run(&first).await.unwrap();
    assert_eq!(run["input"], json!([]));
    assert_eq!(run["context_hydration_pending"], true);
    assert_eq!(app.store.context(&a).await.unwrap().len(), 1);
    run["updated_at"] = json!(now() + 500);
    app.save(&mut run).await.unwrap();
    let rows = app.store.active_runs().await.unwrap();
    assert_eq!(rows[0]["id"], first);
    assert_eq!(rows[1]["id"], second);
    let reopened = Store::open(root.path()).await.unwrap();
    assert_eq!(reopened.active_runs().await.unwrap()[0]["id"], first);
}
#[tokio::test]
async fn single_slot_blocks_queue_without_hydration() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let slot = app.slots.clone().acquire_owned().await.unwrap();
    assert!(app.slots.clone().try_acquire_owned().is_err());
    let task = tokio::spawn(scheduler(app.clone()));
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(status(&app.run(&id).await.unwrap()), "queued");
    assert_eq!(app.run(&id).await.unwrap()["input"], json!([]));
    app.stop.cancel();
    task.await.unwrap();
    drop(slot);
    assert_eq!(app.slots.available_permits(), 1);
}
#[tokio::test]
async fn phase3_http_isolation_and_membership_revocation() {
    let (_root, app) = fixture().await;
    let account = app
        .store
        .create_user_account("phase3_member", "fixture-only-password-hash")
        .await
        .unwrap();
    let user_id = account["user_id"].as_str().unwrap().to_owned();
    let admin_token = uuid();
    let user_token = uuid();
    let admin_key = format!("auth:{}", digest(&json!(admin_token)));
    let user_key = format!("auth:{}", digest(&json!(user_token)));
    app.store
        .put(
            &admin_key,
            &json!({"user_id":"admin","generation":"fixture-generation",
        "csrf":"admin-csrf","expires":now()+3600}),
        )
        .await
        .unwrap();
    let request = |token: &str, csrf: &str, method: &str, path: &str, body: Value| {
        Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("cookie", format!("v3_session={token}"))
            .header("x-csrf-token", csrf)
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let generation =
        app.store.account_by_id(&user_id).await.unwrap().unwrap()["generation"].clone();
    app.store
        .put(
            &user_key,
            &json!({"user_id":user_id,"generation":generation,
        "csrf":"user-csrf","expires":now()+3600}),
        )
        .await
        .unwrap();
    assert_eq!(
        router(app.clone())
            .oneshot(request(
                &user_token,
                "user-csrf",
                "GET",
                "/api/v3/auth/me",
                Value::Null
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    app.store
        .update_account_access(
            &user_id,
            1,
            access::AccountStatus::Active,
            access::Role::User,
            "operator",
        )
        .await
        .unwrap();
    app.store
        .set_agent_membership(&user_id, "shared_default", access::Relationship::Member, 0)
        .await
        .unwrap();
    let generation =
        app.store.account_by_id(&user_id).await.unwrap().unwrap()["generation"].clone();
    app.store
        .put(
            &user_key,
            &json!({"user_id":user_id,"generation":generation,
        "csrf":"user-csrf","expires":now()+3600}),
        )
        .await
        .unwrap();
    let body = json!({"client_request_id":"same-request","agent_id":"shared_default"});
    let response = router(app.clone())
        .oneshot(request(
            &user_token,
            "user-csrf",
            "POST",
            "/api/v3/sessions",
            body.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let user_session: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap(),
    )
    .unwrap();
    let response = router(app.clone())
        .oneshot(request(
            &user_token,
            "user-csrf",
            "POST",
            "/api/v3/sessions",
            body.clone(),
        ))
        .await
        .unwrap();
    let retried: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(retried, user_session);
    let response = router(app.clone())
        .oneshot(request(
            &admin_token,
            "admin-csrf",
            "POST",
            "/api/v3/sessions",
            body,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let admin_session: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_ne!(user_session["id"], admin_session["id"]);
    let user_path = format!("/api/v3/sessions/{}", user_session["id"].as_str().unwrap());
    let admin_path = format!("/api/v3/sessions/{}", admin_session["id"].as_str().unwrap());
    assert_eq!(
        router(app.clone())
            .oneshot(request(
                &admin_token,
                "admin-csrf",
                "GET",
                &user_path,
                Value::Null
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        router(app.clone())
            .oneshot(request(
                &user_token,
                "user-csrf",
                "GET",
                &admin_path,
                Value::Null
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let response = router(app.clone())
        .oneshot(request(
            &user_token,
            "user-csrf",
            "POST",
            &format!("{user_path}/messages"),
            json!({"client_message_id":"one","text":"hello"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let accepted: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap(),
    )
    .unwrap();
    let run_id = accepted["run_id"].as_str().unwrap();
    let response = router(app.clone())
        .oneshot(request(
            &admin_token,
            "admin-csrf",
            "PUT",
            &format!("/api/v3/admin/access/memberships/{user_id}/shared_default"),
            json!({"expected_revision":1,"relationship":"revoked"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let scoped = app.store.for_user(&user_id).await.unwrap();
    assert_eq!(
        scoped.get(&key("run", run_id)).await.unwrap().unwrap()["status"],
        "stopped"
    );
    assert_eq!(
        router(app.clone())
            .oneshot(request(
                &user_token,
                "user-csrf",
                "GET",
                &user_path,
                Value::Null
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let revision = app.store.account_by_id(&user_id).await.unwrap().unwrap()["access_revision"]
        .as_u64()
        .unwrap();
    app.store
        .update_account_access(
            &user_id,
            revision,
            access::AccountStatus::Suspended,
            access::Role::User,
            "operator",
        )
        .await
        .unwrap();
    let generation =
        app.store.account_by_id(&user_id).await.unwrap().unwrap()["generation"].clone();
    app.store
        .put(
            &user_key,
            &json!({"user_id":user_id,"generation":generation,
        "csrf":"user-csrf","expires":now()+3600}),
        )
        .await
        .unwrap();
    assert_eq!(
        router(app)
            .oneshot(request(
                &user_token,
                "user-csrf",
                "GET",
                "/api/v3/auth/me",
                Value::Null
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}
#[tokio::test]
async fn phase3_nondefault_agent_admin_routes_use_its_store_and_filter_denied_abilities() {
    let (_root, app) = fixture().await;
    selection_fixture(&app).await;
    let token = uuid();
    let auth_key = format!("auth:{}", digest(&json!(token)));
    let generation = app.store.account_by_id("admin").await.unwrap().unwrap()["generation"].clone();
    app.store
        .put(
            &auth_key,
            &json!({"user_id":"admin","generation":generation,
        "csrf":"agent-csrf","expires":now()+3600}),
        )
        .await
        .unwrap();
    let request = |method: &str, path: &str, body: Value| {
        Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("cookie", format!("v3_session={token}"))
            .header("x-csrf-token", "agent-csrf")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let response = router(app.clone())
        .oneshot(request("GET", "/api/v3/agents/agent-a/model", Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = router(app.clone())
        .oneshot(request(
            "PUT",
            "/api/v3/agents/agent-a/model",
            json!({"provider":"openai_subscription","model":"agent-a-model"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let default = router(app.clone())
        .oneshot(request(
            "GET",
            "/api/v3/agents/shared_default/model",
            Value::Null,
        ))
        .await
        .unwrap();
    let default: Value = serde_json::from_slice(
        &axum::body::to_bytes(default.into_body(), 65536)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_ne!(default["model"], "agent-a-model");
    let response = router(app.clone())
        .oneshot(request(
            "PUT",
            "/api/v3/agents/agent-a/slots/phase3",
            json!({"content":"AGENT A ONLY","expected_revision":0}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = router(app.clone())
        .oneshot(request(
            "GET",
            "/api/v3/agents/agent-a/slots/phase3",
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let slot: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(slot["slot"]["content"], "AGENT A ONLY");
    assert_eq!(
        router(app.clone())
            .oneshot(request(
                "GET",
                "/api/v3/agents/shared_default/slots/phase3",
                Value::Null
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let response = router(app.clone())
        .oneshot(request(
            "POST",
            "/api/v3/sessions",
            json!({"client_request_id":"agent-a-session","agent_id":"agent-a"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let session: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap(),
    )
    .unwrap();
    let sid = session["id"].as_str().unwrap();
    let response = router(app.clone())
        .oneshot(request(
            "GET",
            "/api/v3/admin/access/agents/agent-a/ability-permissions",
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let permissions: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap(),
    )
    .unwrap();
    let response = router(app.clone())
        .oneshot(request(
            "PUT",
            "/api/v3/admin/access/agents/agent-a/ability-permissions",
            json!({"expected_revision":permissions["revision"],
            "ability_permissions":{"codebase_admin":"deny"}}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = router(app.clone())
        .oneshot(request(
            "GET",
            &format!("/api/v3/sessions/{sid}/abilities"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let catalog: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(!catalog["abilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|ability| ability["id"] == "codebase_admin"));
}
#[tokio::test]
async fn phase3_http_tier_tools_and_agent_disable_reject_stale_approval() {
    let (_root, app) = fixture().await;
    let account = app
        .store
        .create_user_account("phase3_tier", "fixture-only-password-hash")
        .await
        .unwrap();
    let user_id = account["user_id"].as_str().unwrap().to_owned();
    app.store
        .update_account_access(
            &user_id,
            1,
            access::AccountStatus::Active,
            access::Role::User,
            "basic",
        )
        .await
        .unwrap();
    app.store
        .set_agent_membership(&user_id, "shared_default", access::Relationship::Member, 0)
        .await
        .unwrap();
    let admin_token = uuid();
    let user_token = uuid();
    let admin_generation =
        app.store.account_by_id("admin").await.unwrap().unwrap()["generation"].clone();
    let user_generation =
        app.store.account_by_id(&user_id).await.unwrap().unwrap()["generation"].clone();
    app.store.put(&format!("auth:{}",digest(&json!(admin_token))),
        &json!({"user_id":"admin","generation":admin_generation,"csrf":"admin-csrf","expires":now()+3600})).await.unwrap();
    app.store.put(&format!("auth:{}",digest(&json!(user_token))),
        &json!({"user_id":user_id,"generation":user_generation,"csrf":"user-csrf","expires":now()+3600})).await.unwrap();
    let request = |token: &str, csrf: &str, method: &str, path: &str, body: Value| {
        Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("cookie", format!("v3_session={token}"))
            .header("x-csrf-token", csrf)
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let response = router(app.clone())
        .oneshot(request(
            &user_token,
            "user-csrf",
            "POST",
            "/api/v3/sessions",
            json!({"client_request_id":"tier-session"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let session: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap(),
    )
    .unwrap();
    let sid = session["id"].as_str().unwrap();
    let message_path = format!("/api/v3/sessions/{sid}/messages");
    assert_eq!(
        router(app.clone())
            .oneshot(request(
                &user_token,
                "user-csrf",
                "POST",
                &message_path,
                json!({"client_message_id":"denied","text":"hello"})
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let granted_model = app.selection().await.unwrap().model;
    let response=router(app.clone()).oneshot(request(&admin_token,"admin-csrf","PUT",
        "/api/v3/admin/access/tiers/basic",
        json!({"expected_revision":1,"tier":{"id":"basic","revision":1,
            "allowed_models":[granted_model],"tools":{"file_read":"auto","file_write":"ask","v3_build_status":"auto"},
            "ability_permissions":{"codebase_admin":"auto"},"max_sessions":10,"max_active_runs":2}}))).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let access = app
        .store
        .live_access(&user_id, "shared_default")
        .await
        .unwrap();
    assert_eq!(
        access
            .tool_decision_for_abilities(
                "file_read",
                &["codebase_admin".into()],
                access::Permission::Auto,
                true,
                true,
                false,
                true
            )
            .permission,
        access::Permission::Auto
    );
    assert_eq!(
        access
            .tool_decision_for_abilities(
                "file_write",
                &["codebase_admin".into()],
                access::Permission::Auto,
                true,
                true,
                false,
                false
            )
            .permission,
        access::Permission::Ask
    );
    assert_eq!(
        access
            .tool_decision_for_abilities(
                "shell",
                &["codebase_admin".into()],
                access::Permission::Auto,
                true,
                true,
                false,
                false
            )
            .permission,
        access::Permission::Deny
    );
    let host = access.tool_decision(
        "v3_build_status",
        access::Permission::Auto,
        true,
        true,
        false,
        true,
    );
    assert_eq!(host.permission, access::Permission::Deny);
    assert_eq!(host.reason, access::Reason::CallerDenied);
    let restart = access.tool_decision(
        "v3_build_worker_restart",
        access::Permission::Auto,
        true,
        true,
        false,
        false,
    );
    assert_eq!(restart.permission, access::Permission::Deny);
    assert_eq!(restart.reason, access::Reason::CallerDenied);
    let user_generation =
        app.store.account_by_id(&user_id).await.unwrap().unwrap()["generation"].clone();
    app.store
        .put(
            &format!("auth:{}", digest(&json!(user_token))),
            &json!({"user_id":user_id,"generation":user_generation,
            "csrf":"user-csrf","expires":now()+3600}),
        )
        .await
        .unwrap();
    let response = router(app.clone())
        .oneshot(request(
            &user_token,
            "user-csrf",
            "POST",
            &message_path,
            json!({"client_message_id":"accepted","text":"hello"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let accepted: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap(),
    )
    .unwrap();
    let run_id = accepted["run_id"].as_str().unwrap();
    let mut user_app = app.clone();
    user_app.store = app.store.for_user(&user_id).await.unwrap();
    let mut run = user_app.run(run_id).await.unwrap();
    let args = json!({"path":"notes.txt","content":"test"});
    run["status"] = json!("waiting_approval");
    run["phase"] = json!("tools");
    run["calls"] = json!([{"type":"function_call","call_id":"c",
        "name":"file_write","arguments":args.to_string()}]);
    run["pending"] = json!({"id":"tier-stale-approval","name":"file_write","arguments":args,
        "digest":digest(&args),"call_id":"c","state":"waiting","decision":null,
        "access_revision":access.revision,"policy_version":1,"mode_revision":0,
        "visibility_revision":0,"ability_state_revision":0});
    user_app.save(&mut run).await.unwrap();
    let status = crate::agents_api::status_value(&app.store, "shared_default")
        .await
        .unwrap();
    let response = router(app.clone())
        .oneshot(request(
            &admin_token,
            "admin-csrf",
            "PUT",
            "/api/v3/admin/access/agents/shared_default/status",
            json!({"expected_revision":status["revision"],"status":"disabled"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(user_app.run(run_id).await.unwrap()["status"], "stopped");
    let response = router(app.clone())
        .oneshot(request(
            &user_token,
            "user-csrf",
            "POST",
            "/api/v3/approvals/tier-stale-approval/decision",
            json!({"client_request_id":"stale","decision":"approve"}),
        ))
        .await
        .unwrap();
    assert_ne!(response.status(), StatusCode::OK);
    assert_eq!(
        router(app)
            .oneshot(request(
                &user_token,
                "user-csrf",
                "GET",
                &format!("/api/v3/sessions/{sid}"),
                Value::Null
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn rolling_compaction_uses_explicit_system_model_and_persists_checkpoint() {
    use axum::routing::post;
    let (root, mut app) = fixture().await;
    let mut catalog = app.store.model_catalog().await.unwrap();
    let standard_id = catalog.agent_models["shared_default"].roles
        [&model_selection::ModelRole::Standard]
        .entry_id
        .clone();
    // The HTTP fixture must observe System for summarization and Standard for chat.
    let mut system = catalog.entries[&standard_id].clone();
    system.id = "system-summary".into();
    system.upstream_model = "system-summary-model".into();
    system.revision = 1;
    catalog.entries.insert(system.id.clone(), system);
    let roster = catalog.agent_models.get_mut("shared_default").unwrap();
    roster.entry_ids.push("system-summary".into());
    roster.roles.insert(
        model_selection::ModelRole::System,
        model_selection::RoleAssignment {
            entry_id: "system-summary".into(),
            backup_entry_ids: vec![],
        },
    );
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
    let config_path = app.compaction_config_path.clone();
    let mut config = context_compaction::Config::read(&config_path).unwrap();
    config.trigger_tokens = 20_000;
    config.tail_tokens = 100;
    config.summary_tokens = 100;
    config.persist(&config_path).unwrap();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let mock = Router::new().route("/responses", post(move |Json(body): Json<Value>| {
        let captured = captured.clone();
        async move {
            let mut requests = captured.lock().await;
            requests.push(body);
            let text = if requests.len() == 1 { "Remember original goal" } else { "continued" };
            let event = json!({"type":"response.completed","response":{"status":"completed",
                "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}]}});
            ([("content-type","text/event-stream")], format!("data: {event}\n\n"))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    run["input"] = json!([{"role":"user","content":"old".repeat(30_000)},
        {"role":"assistant","content":"done"},{"role":"user","content":"latest"}]);
    app.save(&mut run).await.unwrap();
    running(&app, &id, "rolling-summary").await;
    execute_run(
        app.clone(),
        id.clone(),
        "rolling-summary".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let done = app.run(&id).await.unwrap();
    assert_eq!(done["status"], "completed");
    assert_eq!(done["compactions"], 1);
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["model"], "system-summary-model");
    assert_eq!(
        requests[1]["model"],
        catalog.entries[&standard_id].upstream_model
    );
    assert!(requests[0]["tools"]
        .as_array()
        .is_none_or(|tools| tools.is_empty()));
    assert!(requests[0]["input"]
        .to_string()
        .contains("Fold the previous checkpoint"));
    let events = app.store.events(&sid, 0).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e["kind"] == "context_compacted")
            .count(),
        1
    );
    let session = app.store.get(&key("session", &sid)).await.unwrap().unwrap();
    assert!(session["checkpoint_through"].as_u64().unwrap() > 0);
    assert!(app.store.context(&sid).await.unwrap()[0]
        .to_string()
        .contains("Remember original goal"));
    server.abort();
}
#[tokio::test]
async fn parallel_batch_overlaps_reads_denies_write_by_policy_and_preserves_result_order() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    let (_root, app) = fixture().await;
    let mut policy = policy_default();
    policy["rules"]["file_write"] = json!("deny");
    app.store.put("policy", &policy).await.unwrap();
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["tool_execution"] = json!({"file_list":{
        "mode":"foreground","max_concurrency":2,"on_run_end":"stop"}});
    visibility["revision"] = json!(revision + 1);
    app.store
        .save_agent_abilities(&visibility, revision)
        .await
        .unwrap();
    let sid = conversation(&app).await;
    activate_codebase(&app, &sid).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    run["phase"] = json!("tools");
    run["loaded_abilities"] = json!(["codebase_admin"]);
    run["calls"] = json!([
        {"type":"function_call","call_id":"read-one","name":"file_list","arguments":"{\"path\":\"one\"}"},
        {"type":"function_call","call_id":"read-two","name":"file_list","arguments":"{\"path\":\"two\"}"},
        {"type":"function_call","call_id":"write-rejected","name":"file_write","arguments":"{\"path\":\"x\",\"content\":\"y\"}"}
    ]);
    for call in run["calls"].as_array().unwrap().clone() {
        run["input"].as_array_mut().unwrap().push(call);
    }
    app.save(&mut run).await.unwrap();
    running(&app, &id, "batch-generation").await;
    let listener = UnixListener::bind(&app.tool_socket).unwrap();
    // Neither connection gets a reply until both arrive: serial dispatch times out.
    let worker = tokio::spawn(async move {
        let (first, _) = listener.accept().await.unwrap();
        let (second, _) = listener.accept().await.unwrap();
        let mut requests = Vec::new();
        for stream in [first, second] {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            requests.push((reader, serde_json::from_str::<Value>(&line).unwrap()));
        }
        for (mut reader, request) in requests.into_iter().rev() {
            let reply = json!({"id":request["id"],"status":"success",
                "output":{"path":request["arguments"]["path"]}});
            let reply = test_rpc_reply(&request, reply);
            reader
                .get_mut()
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .unwrap();
        }
    });
    let policy = current_workflow_policy(&app).await.unwrap();
    let access = app
        .store
        .live_access(app.store.current_user_id(), "shared_default")
        .await
        .unwrap();
    let guard = app.gate.lock().await;
    // Hang guard only: the assertions check dispatch order, so a generous bound
    // absorbs build-server load without hiding a stalled batch.
    tokio::time::timeout(
        Duration::from_secs(30),
        parallel_batch::execute_parallel_batch(
            &app,
            &id,
            "batch-generation",
            &CancellationToken::new(),
            &policy,
            &access,
            app.run(&id).await.unwrap(),
            guard,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    worker.await.unwrap();
    let after = app.run(&id).await.unwrap();
    assert_eq!(after["batch"][0]["state"], "done");
    assert_eq!(after["batch"][1]["state"], "done");
    assert_eq!(after["batch"][2]["result"]["status"], "denied");
    let guard = app.gate.lock().await;
    parallel_batch::execute_parallel_batch(
        &app,
        &id,
        "batch-generation",
        &CancellationToken::new(),
        &policy,
        &access,
        after,
        guard,
    )
    .await
    .unwrap();
    let finished = app.run(&id).await.unwrap();
    assert_eq!(finished["phase"], "model");
    assert_eq!(finished["call_index"], 3);
    let outputs = finished["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| item["call_id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(outputs, vec!["read-one", "read-two", "write-rejected"]);
    let history = app.store.context(&sid).await.unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|item| item["type"] == "function_call_output")
            .count(),
        3
    );
}

#[tokio::test]
async fn parallel_batch_rejects_duplicate_provider_call_ids_before_dispatch() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    run["phase"] = json!("tools");
    run["calls"] = json!([
        {"type":"function_call","call_id":"duplicate","name":"file_list","arguments":"{}"},
        {"type":"function_call","call_id":"duplicate","name":"file_list","arguments":"{}"}
    ]);
    app.save(&mut run).await.unwrap();
    running(&app, &id, "batch-duplicate").await;
    let policy = current_workflow_policy(&app).await.unwrap();
    let access = app
        .store
        .live_access(app.store.current_user_id(), "shared_default")
        .await
        .unwrap();
    let guard = app.gate.lock().await;
    parallel_batch::execute_parallel_batch(
        &app,
        &id,
        "batch-duplicate",
        &CancellationToken::new(),
        &policy,
        &access,
        app.run(&id).await.unwrap(),
        guard,
    )
    .await
    .unwrap();
    let failed = app.run(&id).await.unwrap();
    assert_eq!(failed["status"], "failed");
    assert!(failed["error"].as_str().unwrap().contains("duplicate"));
}
#[tokio::test]
async fn parallel_writes_need_independent_approvals_and_reconcile_uncertain_call() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    let (_root, app) = fixture().await;
    let mut config = loop_runtime::Config::read(&app.runtime_path).unwrap();
    config
        .tools
        .parallel_modes
        .insert("file_write".into(), "concurrent".into());
    config.persist(&app.runtime_path).unwrap();
    let sid = conversation(&app).await;
    session_mode_set(
        State(app.clone()),
        Path(sid.clone()),
        Json(SessionModeBody {
            mode: "write".into(),
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
    activate_codebase(&app, &sid).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    run["phase"] = json!("tools");
    run["loaded_abilities"] = json!(["codebase_admin"]);
    run["calls"] = json!([
        {"type":"function_call","call_id":"write-one","name":"file_write",
            "arguments":"{\"path\":\"/workspaces/parallel-approval/one\",\"content\":\"first\"}"},
        {"type":"function_call","call_id":"write-two","name":"file_write",
            "arguments":"{\"path\":\"/workspaces/parallel-approval/two\",\"content\":\"second\"}"}
    ]);
    for call in run["calls"].as_array().unwrap().clone() {
        run["input"].as_array_mut().unwrap().push(call);
    }
    app.save(&mut run).await.unwrap();
    let policy = current_workflow_policy(&app).await.unwrap();
    let access = app
        .store
        .live_access(app.store.current_user_id(), "shared_default")
        .await
        .unwrap();
    let listener = UnixListener::bind(&app.tool_socket).unwrap();
    let worker = tokio::spawn(async move {
        for _ in 0..2 {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            let status = if request["arguments"]["path"] == "/workspaces/parallel-approval/two" {
                "uncertain"
            } else {
                "success"
            };
            let reply = json!({"id":request["id"],"status":status,
                "output":{"path":request["arguments"]["path"]}});
            let reply = test_rpc_reply(&request, reply);
            reader
                .get_mut()
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .unwrap();
        }
    });
    running(&app, &id, "first").await;
    for generation in ["first", "second"] {
        let guard = app.gate.lock().await;
        parallel_batch::execute_parallel_batch(
            &app,
            &id,
            generation,
            &CancellationToken::new(),
            &policy,
            &access,
            app.run(&id).await.unwrap(),
            guard,
        )
        .await
        .unwrap();
        let waiting = app.run(&id).await.unwrap();
        assert_eq!(waiting["status"], "waiting_approval");
        if generation == "second" {
            assert_eq!(waiting["batch"][0]["state"], "done");
            assert_eq!(waiting["batch"][0]["result"]["status"], "success");
            assert_eq!(waiting["batch"][0]["pending"]["human_approved"], true);
            assert_eq!(waiting["batch"][1]["state"], "waiting");
        }
        let approval_id = waiting["pending"]["id"].as_str().unwrap().to_owned();
        approval(
            State(app.clone()),
            Path(approval_id),
            Json(json!({
                "client_request_id":format!("approve-{generation}"),"decision":"approve"
            })),
        )
        .await
        .unwrap();
        if generation == "first" {
            running(&app, &id, "second").await;
        }
    }
    running(&app, &id, "third").await;
    let guard = app.gate.lock().await;
    tokio::time::timeout(
        Duration::from_secs(5),
        parallel_batch::execute_parallel_batch(
            &app,
            &id,
            "third",
            &CancellationToken::new(),
            &policy,
            &access,
            app.run(&id).await.unwrap(),
            guard,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .expect("Fake worker did not receive both approved workspace writes")
        .unwrap();
    let recovering = app.run(&id).await.unwrap();
    assert_eq!(recovering["status"], "recovery");
    assert_eq!(recovering["batch"][0]["state"], "done");
    assert_eq!(recovering["batch"][1]["state"], "uncertain");
    command(
        State(app.clone()),
        Path((id.clone(), "recovery".into())),
        Json(json!({"client_request_id":"accept-two","decision":"accept",
            "call_id":"write-two","output":"verified effect"})),
    )
    .await
    .unwrap();
    running(&app, &id, "fourth").await;
    let guard = app.gate.lock().await;
    parallel_batch::execute_parallel_batch(
        &app,
        &id,
        "fourth",
        &CancellationToken::new(),
        &policy,
        &access,
        app.run(&id).await.unwrap(),
        guard,
    )
    .await
    .unwrap();
    let done = app.run(&id).await.unwrap();
    assert_eq!(done["phase"], "model");
    assert_eq!(done["call_index"], 2);
    let outputs = done["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| item["call_id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(outputs, vec!["write-one", "write-two"]);
    assert!(done["workflow"]["batch_receipts"]["write-one"].is_object());
    assert!(done["workflow"]["batch_receipts"]["write-two"].is_object());
    assert_eq!(
        app.store
            .context(&sid)
            .await
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "function_call_output")
            .count(),
        2
    );
}
#[tokio::test]
async fn existing_auto_approval_is_requeued_without_reusing_its_decision() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    session_mode_set(
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
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    run["phase"] = json!("tools");
    run["status"] = json!("waiting_approval");
    run["pending"] = json!({"id":"old-auto-approval","name":"file_write",
        "state":"waiting","decision":null});
    app.save(&mut run).await.unwrap();
    requeue_auto_approval(&app, &id).await.unwrap();
    let queued = app.run(&id).await.unwrap();
    assert_eq!(queued["status"], "queued");
    assert_eq!(queued["pending"]["state"], "requested");
    assert_ne!(queued["pending"]["id"], "old-auto-approval");
    assert!(queued["pending"]["decision"].is_null());
}
#[tokio::test]
async fn switching_waiting_parallel_writes_to_auto_runs_without_approval() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    let (_root, app) = fixture().await;
    let mut config = loop_runtime::Config::read(&app.runtime_path).unwrap();
    config
        .tools
        .parallel_modes
        .insert("file_write".into(), "concurrent".into());
    config.persist(&app.runtime_path).unwrap();
    let sid = conversation(&app).await;
    session_mode_set(
        State(app.clone()),
        Path(sid.clone()),
        Json(SessionModeBody {
            mode: "write".into(),
            expected_revision: Some(0),
            confirm_write: true,
        }),
    )
    .await
    .unwrap();
    activate_codebase(&app, &sid).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    run["phase"] = json!("tools");
    run["loaded_abilities"] = json!(["codebase_admin"]);
    run["calls"] = json!([
        {"type":"function_call","call_id":"auto-one","name":"file_write",
            "arguments":"{\"path\":\"/workspaces/parallel-auto/one\",\"content\":\"first\"}"},
        {"type":"function_call","call_id":"auto-two","name":"file_write",
            "arguments":"{\"path\":\"/workspaces/parallel-auto/two\",\"content\":\"second\"}"}
    ]);
    app.save(&mut run).await.unwrap();
    let policy = current_workflow_policy(&app).await.unwrap();
    let access = app
        .store
        .live_access(app.store.current_user_id(), "shared_default")
        .await
        .unwrap();
    running(&app, &id, "before-auto").await;
    let guard = app.gate.lock().await;
    parallel_batch::execute_parallel_batch(
        &app,
        &id,
        "before-auto",
        &CancellationToken::new(),
        &policy,
        &access,
        app.run(&id).await.unwrap(),
        guard,
    )
    .await
    .unwrap();
    assert_eq!(app.run(&id).await.unwrap()["status"], "waiting_approval");
    let old_approval = app.run(&id).await.unwrap()["pending"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let revision = session_mode(&app, &sid).await.unwrap()["revision"]
        .as_u64()
        .unwrap();
    session_mode_set(
        State(app.clone()),
        Path(sid.clone()),
        Json(SessionModeBody {
            mode: "auto".into(),
            expected_revision: Some(revision),
            confirm_write: true,
        }),
    )
    .await
    .unwrap();
    assert_eq!(app.run(&id).await.unwrap()["status"], "queued");
    assert!(approval(
        State(app.clone()),
        Path(old_approval),
        Json(json!({
            "client_request_id":"old-mode-approval","decision":"approve"
        }))
    )
    .await
    .is_err());
    running(&app, &id, "after-auto").await;
    let listener = UnixListener::bind(&app.tool_socket).unwrap();
    let worker = tokio::spawn(async move {
        for _ in 0..2 {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            let reply = json!({"id":request["id"],"status":"success","output":"written"});
            let reply = test_rpc_reply(&request, reply);
            reader
                .get_mut()
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .unwrap();
        }
    });
    let guard = app.gate.lock().await;
    tokio::time::timeout(
        Duration::from_secs(5),
        parallel_batch::execute_parallel_batch(
            &app,
            &id,
            "after-auto",
            &CancellationToken::new(),
            &policy,
            &access,
            app.run(&id).await.unwrap(),
            guard,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let after = app.run(&id).await.unwrap();
    for item in after["batch"].as_array().unwrap() {
        assert_eq!(
            item["result"]["status"], "success",
            "Parallel write failed before worker dispatch: {}",
            item["result"]
        );
    }
    tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .expect("Fake worker did not receive both parallel writes")
        .unwrap();
    assert_ne!(after["status"], "waiting_approval");
    assert_eq!(after["batch"][0]["state"], "done");
    assert_eq!(after["batch"][1]["state"], "done");
}

#[tokio::test]
async fn parallel_batch_overflow_queues_reads_despite_legacy_reject() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    let (_root, app) = fixture().await;
    let mut config = loop_runtime::Config::read(&app.runtime_path).unwrap();
    config.tools.max_parallel_calls = 1;
    config
        .tools
        .parallel_modes
        .insert("file_list".into(), "reject".into());
    config.persist(&app.runtime_path).unwrap();
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["tool_execution"] = json!({"file_list":{
        "mode":"foreground","max_concurrency":1,"on_run_end":"stop"}});
    visibility["revision"] = json!(revision + 1);
    app.store
        .save_agent_abilities(&visibility, revision)
        .await
        .unwrap();
    let sid = conversation(&app).await;
    activate_codebase(&app, &sid).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    run["phase"] = json!("tools");
    run["loaded_abilities"] = json!(["codebase_admin"]);
    run["calls"] = json!((0..3)
        .map(|index| json!({
            "type":"function_call","call_id":format!("queued-read-{index}"),
            "name":"file_list","arguments":json!({"path":format!("read-{index}")}).to_string()
        }))
        .collect::<Vec<_>>());
    for call in run["calls"].as_array().unwrap().clone() {
        run["input"].as_array_mut().unwrap().push(call);
    }
    app.save(&mut run).await.unwrap();
    running(&app, &id, "overflow").await;
    let listener = UnixListener::bind(&app.tool_socket).unwrap();
    let worker = tokio::spawn(async move {
        let mut paths = std::collections::BTreeSet::new();
        for _ in 0..3 {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert!(paths.insert(request["arguments"]["path"].as_str().unwrap().to_owned()));
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
            let reply = json!({"id":request["id"],"status":"success","output":request["arguments"]["path"]});
            let reply = test_rpc_reply(&request, reply);
            reader
                .get_mut()
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .unwrap();
        }
        assert_eq!(paths.len(), 3);
    });
    let policy = current_workflow_policy(&app).await.unwrap();
    let access = app
        .store
        .live_access(app.store.current_user_id(), "shared_default")
        .await
        .unwrap();
    let guard = app.gate.lock().await;
    // Hang guard only: the assertions check dispatch order, so a generous bound
    // absorbs build-server load without hiding a stalled batch.
    tokio::time::timeout(
        Duration::from_secs(30),
        parallel_batch::execute_parallel_batch(
            &app,
            &id,
            "overflow",
            &CancellationToken::new(),
            &policy,
            &access,
            app.run(&id).await.unwrap(),
            guard,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    worker.await.unwrap();
    let after = app.run(&id).await.unwrap();
    assert!(after["batch"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["state"] == "done" && item["result"]["status"] == "success"));
    let guard = app.gate.lock().await;
    parallel_batch::execute_parallel_batch(
        &app,
        &id,
        "overflow",
        &CancellationToken::new(),
        &policy,
        &access,
        after,
        guard,
    )
    .await
    .unwrap();
    let finished = app.run(&id).await.unwrap();
    assert_eq!(finished["call_index"], 3);
    for index in 0..3 {
        assert!(finished["workflow"]["batch_receipts"][format!("queued-read-{index}")].is_object());
    }
}

#[tokio::test]
async fn parallel_batch_read_completes_before_independent_write_approval() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    let (_root, app) = fixture().await;
    let mut config = loop_runtime::Config::read(&app.runtime_path).unwrap();
    config.tools.parallel_modes.remove("file_list");
    config.persist(&app.runtime_path).unwrap();
    let sid = conversation(&app).await;
    session_mode_set(
        State(app.clone()),
        Path(sid.clone()),
        Json(SessionModeBody {
            mode: "write".into(),
            expected_revision: Some(0),
            confirm_write: true,
        }),
    )
    .await
    .unwrap();
    activate_codebase(&app, &sid).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    run["phase"] = json!("tools");
    run["loaded_abilities"] = json!(["codebase_admin"]);
    run["calls"] = json!([
        {"type":"function_call","call_id":"approval-write","name":"file_write",
            "arguments":json!({"path":"write","content":"value"}).to_string()},
        {"type":"function_call","call_id":"permitted-read","name":"file_list",
            "arguments":json!({"path":"read"}).to_string()}
    ]);
    for call in run["calls"].as_array().unwrap().clone() {
        run["input"].as_array_mut().unwrap().push(call);
    }
    app.save(&mut run).await.unwrap();
    running(&app, &id, "independent").await;
    let listener = UnixListener::bind(&app.tool_socket).unwrap();
    let worker_app = app.clone();
    let worker_id = id.clone();
    let worker_sid = sid.clone();
    let worker = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["arguments"]["path"], "read");
        let executing = worker_app.run(&worker_id).await.unwrap();
        assert_eq!(executing["status"], "running");
        assert!(executing["pending"].is_null());
        assert_eq!(executing["batch"][0]["state"], "waiting");
        assert_eq!(executing["batch"][1]["state"], "executing");
        assert!(!worker_app
            .store
            .events(&worker_sid, 0)
            .await
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "approval_required"));
        let reply = json!({"id":request["id"],"status":"success","output":"read completed"});
        let reply = test_rpc_reply(&request, reply);
        reader
            .get_mut()
            .write_all(format!("{reply}\n").as_bytes())
            .await
            .unwrap();
    });
    let policy = current_workflow_policy(&app).await.unwrap();
    let access = app
        .store
        .live_access(app.store.current_user_id(), "shared_default")
        .await
        .unwrap();
    let guard = app.gate.lock().await;
    // Hang guard only: the assertions check dispatch order, so a generous bound
    // absorbs build-server load without hiding a stalled batch.
    tokio::time::timeout(
        Duration::from_secs(30),
        parallel_batch::execute_parallel_batch(
            &app,
            &id,
            "independent",
            &CancellationToken::new(),
            &policy,
            &access,
            app.run(&id).await.unwrap(),
            guard,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    worker.await.unwrap();
    let waiting = app.run(&id).await.unwrap();
    assert_eq!(waiting["status"], "waiting_approval");
    assert_eq!(waiting["pending"]["call_id"], "approval-write");
    assert_eq!(waiting["batch"][1]["state"], "done");
    assert_eq!(waiting["batch"][1]["result"]["output"], "read completed");
    let approval_id = waiting["pending"]["id"].as_str().unwrap().to_owned();
    approval(
        State(app.clone()),
        Path(approval_id),
        Json(json!({
            "client_request_id":"deny-independent-write","decision":"deny"
        })),
    )
    .await
    .unwrap();
    running(&app, &id, "independent-denied").await;
    let guard = app.gate.lock().await;
    parallel_batch::execute_parallel_batch(
        &app,
        &id,
        "independent-denied",
        &CancellationToken::new(),
        &policy,
        &access,
        app.run(&id).await.unwrap(),
        guard,
    )
    .await
    .unwrap();
    let done = app.run(&id).await.unwrap();
    assert_eq!(done["phase"], "model");
    assert_eq!(done["call_index"], 2);
    assert!(done["workflow"]["batch_receipts"]["approval-write"].is_object());
    assert!(done["workflow"]["batch_receipts"]["permitted-read"].is_object());
    assert_eq!(
        app.store
            .events(&sid, 0)
            .await
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == "approval_required")
            .count(),
        1
    );
}

#[tokio::test]
async fn web_access_api_support_auth_csrf_revision_and_persistence() {
    let (root, app) = fixture().await;
    let token = uuid();
    app.store
        .put(
            &format!("auth:{}", digest(&json!(token))),
            &json!({"csrf":"fixture","expires":now()+3600,"generation":"fixture-generation"}),
        )
        .await
        .unwrap();
    let cookie = format!("v3_session={token}");
    let uri = "/api/v3/agents/shared_default/abilities";
    let request = |method: &str, path: &str, auth: bool, csrf: bool, body: Value| {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if auth {
            b = b.header("cookie", &cookie);
        }
        if csrf {
            b = b.header("x-csrf-token", "fixture");
        }
        b.body(Body::from(body.to_string())).unwrap()
    };
    assert_eq!(
        router(app.clone())
            .oneshot(request("GET", uri, false, false, Value::Null))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let response = router(app.clone())
        .oneshot(request("GET", uri, true, false, Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let catalog: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    let web = catalog["abilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == "web_access")
        .unwrap();
    assert_eq!(web["readiness"], "ready");
    assert_eq!(web["available"], true);
    // get_weather and maps_geocode; web_search moved to the Web Search ability.
    assert_eq!(web["tools"].as_array().unwrap().len(), 2);
    assert!(web["tools"]
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v["supported"] == true));
    let path = format!("{uri}/web_access/tools/maps_geocode/mode");
    let body = json!({"visibility":"off","expected_revision":0});
    assert_eq!(
        router(app.clone())
            .oneshot(request("PUT", &path, true, false, body.clone()))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        router(app.clone())
            .oneshot(request("PUT", &path, true, true, body.clone()))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let stale = json!({"visibility":"visible","expected_revision":0});
    assert_eq!(
        router(app.clone())
            .oneshot(request("PUT", &path, true, true, stale))
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        app.store.agent_abilities().await.unwrap()["tool_modes"]["maps_geocode"],
        "off"
    );
    app.store.app_pool.close().await;
    app.store.pool.close().await;
    app.store.agent_pool.close().await;
    let reopened = Store::open(root.path()).await.unwrap();
    assert_eq!(
        reopened.agent_abilities().await.unwrap()["tool_modes"]["maps_geocode"],
        "off"
    );
}

#[tokio::test]
async fn web_access_policy_migration_preserves_denial_and_approval() {
    let (_root, app) = fixture().await;
    let mut p = policy_default();
    p["rules"]["web_search"] = json!("deny");
    p["rules"]["get_weather"] = json!("require_approval");
    p["rules"].as_object_mut().unwrap().remove("maps_geocode");
    app.store.put("policy", &p).await.unwrap();
    migrate_tool_policy_defaults(&app.store).await.unwrap();
    migrate_tool_policy_defaults(&app.store).await.unwrap();
    let migrated = app.store.get("policy").await.unwrap().unwrap();
    assert_eq!(migrated["rules"]["web_search"], "deny");
    assert_eq!(migrated["rules"]["get_weather"], "require_approval");
    assert_eq!(migrated["rules"]["maps_geocode"], "deny");
    assert_eq!(migrated["version"], 2);
}

#[tokio::test]
async fn web_access_native_dispatch_needs_no_worker_and_cancels() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let args = json!({"action":"distance","lat":0,"lon":0,"lat2":0,"lon2":1});
    assert!(!app.tool_socket.exists());
    let result = tools::call_raw(
        app.clone(),
        sid.clone(),
        uuid(),
        "fixture-generation".into(),
        "maps_geocode".into(),
        args.clone(),
        uuid(),
        1,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result["status"], "success");
    assert_eq!(result["output"]["km"], 111.195);
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(tools::call_raw(
        app,
        sid,
        uuid(),
        "fixture-generation".into(),
        "maps_geocode".into(),
        args,
        uuid(),
        1,
        cancel
    )
    .await
    .is_err());
    for name in ["web_search", "get_weather", "maps_geocode"] {
        assert!(tools::is_read(name));
    }
}

#[tokio::test]
async fn tenant_ability_http_levels_effective_views_and_stale_updates() {
    let (_root, app) = fixture().await;
    selection_fixture(&app).await;
    let account = app
        .store
        .create_user_account("ability_member", "fixture-only-password-hash")
        .await
        .unwrap();
    let user = account["user_id"].as_str().unwrap().to_owned();
    app.store
        .update_account_access(
            &user,
            1,
            access::AccountStatus::Active,
            access::Role::User,
            "operator",
        )
        .await
        .unwrap();
    app.store
        .set_agent_membership(&user, "agent-a", access::Relationship::Member, 0)
        .await
        .unwrap();
    app.store
        .set_agent_membership(&user, "shared_default", access::Relationship::Member, 0)
        .await
        .unwrap();
    let token = uuid();
    let member_token = uuid();
    for (user, token, csrf) in [
        ("admin", token.as_str(), "a"),
        (user.as_str(), member_token.as_str(), "m"),
    ] {
        let generation =
            app.store.account_by_id(user).await.unwrap().unwrap()["generation"].clone();
        app.store
            .put(
                &format!("auth:{}", digest(&json!(token))),
                &json!({"user_id":user,"generation":generation,"csrf":csrf,"expires":now()+3600}),
            )
            .await
            .unwrap();
    }
    async fn call(
        app: &App,
        token: &str,
        csrf: &str,
        method: &str,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("cookie", format!("v3_session={token}"))
            .header("x-csrf-token", csrf)
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = router(app.clone()).oneshot(req).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    let path = "/api/v3/agents/agent-a/abilities";
    let (status, view) = call(&app, &token, "a", "GET", path, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["authority"]["can_manage"], true);
    // Unsaved levels fail closed: every ready ability reports the distinct cause and notice.
    assert_eq!(view["notice"], json!(crate::capabilities::NO_TOOLS_NOTICE));
    let ready: Vec<_> = view["abilities"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["ready"] == true)
        .collect();
    assert!(
        !ready.is_empty()
            && ready.iter().all(|a| a["effective"]["permission"] == "deny"
                && a["effective"]["reason"] == "capability_levels_unset")
    );
    let (_, summary) = call(
        &app,
        &token,
        "a",
        "GET",
        "/api/v3/agents/agent-a/access-summary",
        Value::Null,
    )
    .await;
    assert_eq!(summary["tools_notice"], view["notice"]);
    crate::runtime_tests::grant_fixture_levels(&app.store, "agent-a").await;
    let (_, view) = call(&app, &token, "a", "GET", path, Value::Null).await;
    assert!(view["notice"].is_null());
    let (_, summary) = call(
        &app,
        &token,
        "a",
        "GET",
        "/api/v3/agents/agent-a/access-summary",
        Value::Null,
    )
    .await;
    assert!(summary["tools_notice"].is_null());
    let (_, member_view) = call(&app, &member_token, "m", "GET", path, Value::Null).await;
    assert_eq!(member_view["authority"]["can_manage"], false);
    assert_eq!(member_view["permissions"], json!({}));
    assert_eq!(member_view["tool_modes"], json!({}));
    assert!(!member_view["abilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["readiness"] != "ready"));
    let level = json!({"id":"limited","name":"Limited","revision":1,"ability_permissions":{"web_access":"auto"},"tool_permissions":{}});
    let update = json!({"expected_revision":3,"level":level});
    let level_path = "/api/v3/agents/agent-a/capability-levels/limited";
    assert_eq!(
        call(&app, &member_token, "m", "PUT", level_path, update.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, &token, "wrong", "PUT", level_path, update.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, &token, "a", "PUT", level_path, update.clone())
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, &token, "a", "PUT", level_path, update).await.0,
        StatusCode::CONFLICT
    );
    let assign = format!("/api/v3/agents/agent-a/members/{user}/capability-level");
    assert_eq!(
        call(
            &app,
            &token,
            "a",
            "PUT",
            &assign,
            json!({"expected_revision":1,"capability_level_id":"limited"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, &member_token, "m", "GET", path, Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    let (_, preview) = call(
        &app,
        &token,
        "a",
        "GET",
        &format!("{path}/effective?user_id={user}"),
        Value::Null,
    )
    .await;
    let codebase = preview["abilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "codebase_admin")
        .unwrap();
    assert_eq!(codebase["effective"]["permission"], "deny");
    assert_eq!(codebase["effective"]["reason"], "capability_denied");
    assert_eq!(
        app.store
            .live_access(&user, "shared_default")
            .await
            .unwrap()
            .ability_permission("codebase_admin"),
        access::Permission::Auto
    );
    assert_eq!(
        app.store
            .live_access("admin", "agent-a")
            .await
            .unwrap()
            .ability_permission("codebase_admin"),
        access::Permission::Auto
    );
    let visibility_path = format!("{path}/codebase_admin/visibility");
    assert_eq!(
        call(
            &app,
            &token,
            "a",
            "PUT",
            &visibility_path,
            json!({"expected_revision":0,"visibility":"off"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, view) = call(&app, &token, "a", "GET", path, Value::Null).await;
    let codebase = view["abilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "codebase_admin")
        .unwrap();
    assert_eq!(codebase["available"], false);
    assert_eq!(codebase["effective"]["permission"], "deny");
}

#[tokio::test]
async fn ability_revocation_keeps_parallel_write_outcomes_uncertain() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    running(&app, &id, "in-flight").await;
    let mut run = app.run(&id).await.unwrap();
    run["batch"] = json!([{"state":"executing","pending":{"name":"file_write","call_id":"write"}},
        {"state":"executing","pending":{"name":"file_read","call_id":"read"}}]);
    app.save(&mut run).await.unwrap();
    let _guard = app.gate.lock().await;
    phase3_api::quiesce_agent(&app, "shared_default")
        .await
        .unwrap();
    let after = app.run(&id).await.unwrap();
    assert_eq!(after["status"], "recovery");
    assert_eq!(after["uncertain"], true);
    assert_eq!(after["batch"][0]["state"], "uncertain");
    assert_eq!(after["batch"][1]["state"], "executing");
    assert!(!app
        .store
        .context(&sid)
        .await
        .unwrap()
        .iter()
        .any(|row| row["role"] == "assistant"));
}

#[tokio::test]
/// Verify manifest removal withdraws its schemas and projected guidance during an active run without persisting guidance in history.
async fn removed_manifest_withdraws_cached_schemas_and_guidance_within_run() {
    use axum::routing::post;
    let (root, mut app) = fixture().await;
    let catalog_root = root.path().join("abilities");
    let directory = catalog_root.join("Administrator/codebase_admin");
    std::fs::create_dir_all(&directory).unwrap();
    let manifest = directory.join("codebase_admin.json");
    std::fs::write(
        &manifest,
        serde_json::to_vec(&json!({
        "display_name":"Codebase Admin","description":"Isolated cache fixture",
        "kind":"ability","readiness":"ready","tools":["state_inspect"]}))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        directory.join("codebase_admin.skill.md"),
        "CACHE_WITHDRAWAL_GUIDANCE_SENTINEL",
    )
    .unwrap();
    app.abilities = Arc::new(abilities::Catalog::load(&catalog_root).unwrap());
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["visibility"]["codebase_admin"] = json!("visible");
    app.store
        .save_agent_abilities(&visibility, revision)
        .await
        .unwrap();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let mock = Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let captured = captured.clone();
            let manifest = manifest.clone();
            async move {
                let mut captured = captured.lock().await;
                captured.push(body);
                let output = if captured.len() == 1 {
                    std::fs::remove_file(manifest).unwrap();
                    json!([{"type":"function_call","call_id":"removed-call",
                    "name":"state_inspect","arguments":"{\"resource\":\"catalog\"}"}])
                } else {
                    json!([{"type":"message","role":"assistant",
                    "content":[{"type":"output_text","text":"Done."}]}])
                };
                let event = json!({"type":"response.completed","response":{
                "status":"completed","output":output}});
                (
                    [("content-type", "text/event-stream")],
                    format!("data: {event}\n\n"),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    running(&app, &id, "manifest-removal").await;
    execute_run(
        app.clone(),
        id.clone(),
        "manifest-removal".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(app.run(&id).await.unwrap()["status"], "completed");
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert!(requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|schema| schema["name"] == "state_inspect"));
    let first_guidance = native_ability_frames(&requests[0], "codebase_admin");
    assert_eq!(first_guidance.len(), 1);
    assert!(first_guidance[0].contains("CACHE_WITHDRAWAL_GUIDANCE_SENTINEL"));
    assert!(!requests[1]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|schema| schema["name"] == "state_inspect"));
    assert!(native_ability_frames(&requests[1], "codebase_admin").is_empty());
    for request in requests.iter() {
        assert!(!request["instructions"]
            .as_str()
            .unwrap()
            .contains("CACHE_WITHDRAWAL_GUIDANCE_SENTINEL"));
    }
    assert!(!app.run(&id).await.unwrap()["input"]
        .to_string()
        .contains("CACHE_WITHDRAWAL_GUIDANCE_SENTINEL"));
    assert!(!json!(app.store.context(&sid).await.unwrap())
        .to_string()
        .contains("CACHE_WITHDRAWAL_GUIDANCE_SENTINEL"));
    server.abort();
}

#[tokio::test]
/// Verify withdrawal during credential retrieval rebuilds a safe request, while cancellation prevents dispatch and final publication.
async fn bundle_withdrawal_during_credentials_rebuilds_before_provider_dispatch() {
    use axum::routing::post;
    for cancelled in [false, true] {
        let (root, mut app) = fixture().await;
        let catalog_root = root.path().join("dispatch-abilities");
        let directory = catalog_root.join("Administrator/codebase_admin");
        std::fs::create_dir_all(&directory).unwrap();
        let manifest = directory.join("codebase_admin.json");
        std::fs::write(
            &manifest,
            serde_json::to_vec(&json!({
        "display_name":"Codebase Admin","description":"Dispatch race fixture",
        "kind":"ability","readiness":"ready","tools":["state_inspect"]}))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            directory.join("codebase_admin.skill.md"),
            "DISPATCH_RACE_GUIDANCE",
        )
        .unwrap();
        app.abilities = Arc::new(abilities::Catalog::load(&catalog_root).unwrap());
        let mut visibility = app.store.agent_abilities().await.unwrap();
        let revision = visibility["revision"].as_u64().unwrap();
        visibility["visibility"]["codebase_admin"] = json!("visible");
        app.store
            .save_agent_abilities(&visibility, revision)
            .await
            .unwrap();
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let captured = requests.clone();
        let mock = Router::new().route(
            "/responses",
            post(move |Json(body): Json<Value>| {
                let captured = captured.clone();
                async move {
                    captured.lock().await.push(body);
                    let event = json!({"type":"response.completed","response":{"status":"completed",
                "output":[{"type":"message","role":"assistant",
                    "content":[{"type":"output_text","text":"Done."}]}]}});
                    (
                        [("content-type", "text/event-stream")],
                        format!("data: {event}\n\n"),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
        app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
        let sid = conversation(&app).await;
        let id = queued(&app, &sid).await;
        running(&app, &id, "dispatch-race").await;
        // tokens_at cannot proceed until this lock is released.
        let credentials = app.refresh.lock().await;
        let task_app = app.clone();
        let task_id = id.clone();
        let task = tokio::spawn(async move {
            execute_run(
                task_app,
                task_id,
                "dispatch-race".into(),
                CancellationToken::new(),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if !app.run(&id).await.unwrap()["context_assembly_report"].is_null() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        if cancelled {
            let _guard = app.gate.lock().await;
            let mut run = app.run(&id).await.unwrap();
            run["status"] = json!("cancelled");
            app.save(&mut run).await.unwrap();
        }
        std::fs::remove_file(manifest).unwrap();
        drop(credentials);
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let run = app.run(&id).await.unwrap();
        let requests = requests.lock().await;
        if cancelled {
            assert_eq!(run["status"], "cancelled");
            assert!(requests.is_empty());
            assert!(!app
                .store
                .context(&sid)
                .await
                .unwrap()
                .iter()
                .any(|item| item["role"] == "assistant"));
        } else {
            assert_eq!(run["status"], "completed", "{}", run["error"]);
            assert_eq!(requests.len(), 1);
            assert!(!requests[0]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|schema| schema["name"] == "state_inspect"));
            assert!(native_ability_frames(&requests[0], "codebase_admin").is_empty());
            assert_eq!(
                app.store
                    .context(&sid)
                    .await
                    .unwrap()
                    .iter()
                    .filter(|item| item["role"] == "assistant")
                    .count(),
                1
            );
        }
        server.abort();
    }
}

#[tokio::test]
async fn session_read_routes_enforce_auth_csrf_ownership_and_monotonicity() {
    use axum::http::Method;
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let other = conversation(&app).await;
    app.store
        .atomic_docs_event(
            vec![],
            &sid,
            "message",
            &json!({"role":"user","run_id":"read-run","content":"hello"}),
        )
        .await
        .unwrap();
    app.store
        .atomic_docs_event(
            vec![],
            &other,
            "message",
            &json!({"role":"user","run_id":"foreign-read-run","content":"other"}),
        )
        .await
        .unwrap();
    let cursor = app.store.chat_snapshot(&sid).await.unwrap().1;
    let foreign = app.store.chat_snapshot(&other).await.unwrap().1;
    let cookie = ability_auth(&app).await;
    let uri = format!("/api/v3/sessions/{sid}/read");
    let body = json!({"through_seq":cursor});
    assert_eq!(
        ability_request(&app, Method::PUT, &uri, Some(body.clone()), None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        ability_request(
            &app,
            Method::PUT,
            &uri,
            Some(body.clone()),
            Some(&cookie),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        ability_request(
            &app,
            Method::PUT,
            &uri,
            Some(json!({"through_seq":foreign})),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (status, result) = ability_request(
        &app,
        Method::PUT,
        &uri,
        Some(body.clone()),
        Some(&cookie),
        Some("ability-fixture"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["session"]["read_through_seq"], cursor);
    assert_eq!(result["session"]["metadata_revision"], 0);
    let (_, retry) = ability_request(
        &app,
        Method::PUT,
        &uri,
        Some(body),
        Some(&cookie),
        Some("ability-fixture"),
    )
    .await;
    assert_eq!(result, retry);
    let (_, older) = ability_request(
        &app,
        Method::PUT,
        &uri,
        Some(json!({"through_seq":0})),
        Some(&cookie),
        Some("ability-fixture"),
    )
    .await;
    assert_eq!(older["session"]["read_through_seq"], cursor);
    assert_eq!(
        ability_request(
            &app,
            Method::PUT,
            "/api/v3/sessions/missing/read",
            Some(json!({"through_seq":0})),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let (_, snapshot) = ability_request(
        &app,
        Method::GET,
        &format!("/api/v3/sessions/{sid}"),
        None,
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(snapshot["session"], result["session"]);
}

#[tokio::test]
async fn session_search_routes_are_private_authenticated_and_access_filtered() {
    use axum::http::Method;
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let cookie = ability_auth(&app).await;
    let mut doc = app.store.get(&key("session", &sid)).await.unwrap().unwrap();
    doc["title"] = json!("Private Needle");
    doc["checkpoint_input"] = json!([{"content":"hidden-only"}]);
    app.store.put(&key("session", &sid), &doc).await.unwrap();
    app.store.atomic_docs_event(vec![],&sid,"message",&json!({"role":"user","run_id":"search-r","content":"needle <img src=x onerror=alert(1)>"})).await.unwrap();
    app.store
        .put(
            "session:denied-search",
            &json!({"id":"denied-search","title":"needle denied","agent_id":"missing-agent"}),
        )
        .await
        .unwrap();
    sqlx::query("INSERT INTO user_accounts(user_id,username,value) VALUES('searchforeign','searchforeign',?)").bind(app.store.seal(&json!({"user_id":"searchforeign","username":"searchforeign"})).unwrap()).execute(&app.store.app_pool).await.unwrap();
    let other = app.store.for_user("searchforeign").await.unwrap();
    other
        .put(
            "session:foreign-search",
            &json!({"id":"foreign-search","title":"needle foreign"}),
        )
        .await
        .unwrap();
    let uri = "/api/v3/sessions/search?q=needle";
    assert_eq!(
        ability_request(&app, Method::GET, uri, None, None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, page) = ability_request(&app, Method::GET, uri, None, Some(&cookie), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["results"].as_array().unwrap().len(), 2);
    assert!(page["results"]
        .as_array()
        .unwrap()
        .iter()
        .all(|h| h["session_id"] == sid));
    assert!(page["results"][1]["snippet"]
        .as_str()
        .unwrap()
        .contains("<img"));
    let (_, hidden) = ability_request(
        &app,
        Method::GET,
        "/api/v3/sessions/search?q=hidden-only",
        None,
        Some(&cookie),
        None,
    )
    .await;
    assert!(hidden["results"].as_array().unwrap().is_empty());
    assert_eq!(
        ability_request(
            &app,
            Method::GET,
            "/api/v3/sessions/search?q=needle&continuation=ffff",
            None,
            Some(&cookie),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        ability_request(
            &app,
            Method::GET,
            "/api/v3/sessions/search?q=needle&limit=51",
            None,
            Some(&cookie),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn session_lifecycle_routes_enforce_auth_csrf_history_and_conflicts() {
    use axum::http::Method;
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let uri = format!("/api/v3/sessions/{sid}");
    let archive = format!("{uri}/archive");
    let cookie = ability_auth(&app).await;
    let body = json!({"client_request_id":"archive","expected_revision":0});
    assert_eq!(
        ability_request(&app, Method::POST, &archive, Some(body.clone()), None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        ability_request(
            &app,
            Method::POST,
            &archive,
            Some(body.clone()),
            Some(&cookie),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, archived) = ability_request(
        &app,
        Method::POST,
        &archive,
        Some(body.clone()),
        Some(&cookie),
        Some("ability-fixture"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(archived["session"]["status"], "archived");
    assert_eq!(
        ability_request(
            &app,
            Method::POST,
            &archive,
            Some(body),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .1,
        archived
    );
    let (_, list) = ability_request(
        &app,
        Method::GET,
        "/api/v3/sessions",
        None,
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(list["sessions"].as_array().unwrap().len(), 0);
    let (_, list) = ability_request(
        &app,
        Method::GET,
        "/api/v3/sessions?status=archived",
        None,
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(list["sessions"][0]["id"], sid);
    assert_eq!(
        ability_request(
            &app,
            Method::GET,
            "/api/v3/sessions?status=invalid",
            None,
            Some(&cookie),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        ability_request(&app, Method::GET, &uri, None, Some(&cookie), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let message = json!({"text":"blocked","client_message_id":"blocked"});
    assert_eq!(
        ability_request(
            &app,
            Method::POST,
            &format!("{uri}/messages"),
            Some(message),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let unarchive = json!({"client_request_id":"unarchive","expected_revision":1});
    assert_eq!(
        ability_request(
            &app,
            Method::POST,
            &format!("{uri}/unarchive"),
            Some(unarchive.clone()),
            Some(&cookie),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, result) = ability_request(
        &app,
        Method::POST,
        &format!("{uri}/unarchive"),
        Some(unarchive.clone()),
        Some(&cookie),
        Some("ability-fixture"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["session"]["status"], "active");
    assert_eq!(
        ability_request(
            &app,
            Method::POST,
            &format!("{uri}/unarchive"),
            Some(unarchive),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .1,
        result
    );
    assert_eq!(
        ability_request(
            &app,
            Method::POST,
            "/api/v3/sessions/missing/archive",
            Some(json!({"client_request_id":"missing","expected_revision":0})),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    // Lifecycle cannot cross the user authority or an inaccessible agent binding.
    sqlx::query("INSERT INTO user_accounts(user_id,username,value) VALUES('lifecycleother','lifecycleother',?)")
        .bind(app.store.seal(&json!({"user_id":"lifecycleother","username":"lifecycleother"})).unwrap())
        .execute(&app.store.app_pool).await.unwrap();
    let other = app.store.for_user("lifecycleother").await.unwrap();
    other
        .put(
            "session:foreign-lifecycle",
            &json!({"id":"foreign-lifecycle","agent_id":"shared_default"}),
        )
        .await
        .unwrap();
    for path in [
        "/api/v3/sessions/foreign-lifecycle/archive",
        "/api/v3/sessions/foreign-lifecycle/unarchive",
    ] {
        assert_eq!(
            ability_request(
                &app,
                Method::POST,
                path,
                Some(json!({"client_request_id":"foreign","expected_revision":0})),
                Some(&cookie),
                Some("ability-fixture")
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }
    app.store
        .put(
            "session:inaccessible-lifecycle",
            &json!({"id":"inaccessible-lifecycle","agent_id":"missing-agent"}),
        )
        .await
        .unwrap();
    assert_eq!(
        ability_request(
            &app,
            Method::POST,
            "/api/v3/sessions/inaccessible-lifecycle/archive",
            Some(json!({"client_request_id":"inaccessible","expected_revision":0})),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    // Archiving cancels pending approval; deletion keeps the session unavailable.
    app.store
        .put(
            "run:lifecycle-blocker",
            &json!({"id":"lifecycle-blocker","session_id":sid,"status":"waiting_approval"}),
        )
        .await
        .unwrap();
    assert_eq!(
        ability_request(
            &app,
            Method::POST,
            &archive,
            Some(json!({"client_request_id":"with-run","expected_revision":2})),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        app.store
            .get("run:lifecycle-blocker")
            .await
            .unwrap()
            .unwrap()["status"],
        "cancelled"
    );
    let delete = json!({"client_request_id":"delete","expected_revision":3});
    assert_eq!(
        ability_request(
            &app,
            Method::DELETE,
            &uri,
            Some(delete.clone()),
            Some(&cookie),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        ability_request(
            &app,
            Method::DELETE,
            &uri,
            Some(delete.clone()),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        ability_request(
            &app,
            Method::DELETE,
            &uri,
            Some(delete),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        ability_request(&app, Method::GET, &uri, None, Some(&cookie), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        ability_request(
            &app,
            Method::POST,
            &format!("{uri}/unarchive"),
            Some(json!({"client_request_id":"after-delete","expected_revision":0})),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn platform_adoption_pins_independent_credentials_and_denies_rotation() {
    use crate::account_catalog::{platform_id, Adoption, Origin, Source};
    use crate::platform_catalog::{Entry, Mutation};
    use std::collections::{BTreeMap, BTreeSet};
    let (_root, app) = fixture().await;
    let user = app
        .store
        .create_user_account("platform_runner", "fixture-only-password-hash")
        .await
        .unwrap();
    let user = user["user_id"].as_str().unwrap();
    app.store
        .update_account_access(
            user,
            1,
            access::AccountStatus::Active,
            access::Role::User,
            "basic",
        )
        .await
        .unwrap();
    app.store
        .assign_agent_membership(
            "admin",
            user,
            "shared_default",
            access::Relationship::Member,
            true,
            0,
        )
        .await
        .unwrap();
    let entry = Entry {
        id: "model".into(),
        display_name: "Platform".into(),
        upstream_model: "fixture".into(),
        transport: model_selection::Transport::Deepseek,
        capabilities: BTreeSet::from(["text".into()]),
    };
    app.store
        .mutate_platform_roster(
            "team",
            0,
            Mutation::Draft {
                name: "Team".into(),
                entries: BTreeMap::from([("model".into(), entry)]),
            },
            "admin",
        )
        .await
        .unwrap();
    app.store
        .platform_credential_set("team", "model", 1, Some("platform-fixture-key"), "admin")
        .await
        .unwrap();
    app.store
        .mutate_platform_roster("team", 2, Mutation::Publish { rollback: None }, "admin")
        .await
        .unwrap();
    let mut tier = app.store.entitlement_tier("basic").await.unwrap().unwrap();
    let rev = tier.revision;
    tier.model_policy = Some(access::ModelPolicy {
        roster_id: Some("team".into()),
        allowed_entry_ids: BTreeSet::from(["model".into()]),
        allow_byo: false,
        max_byo_entries: 0,
    });
    let mut ceiling = app
        .store
        .entitlement_tier("operator")
        .await
        .unwrap()
        .unwrap();
    let upper_revision = ceiling.revision;
    ceiling.model_policy = tier.model_policy.clone();
    app.store
        .save_entitlement_tier(ceiling, upper_revision)
        .await
        .unwrap();
    app.store.save_entitlement_tier(tier, rev).await.unwrap();
    let before = app.store.model_catalog().await.unwrap();
    let adopted = app
        .store
        .adopt_byo_source(
            user,
            "shared_default",
            &Adoption {
                client_request_id: "platform_once".into(),
                expected_roster_revision: before.agent_models["shared_default"].revision,
                source: Source {
                    origin: Origin::Platform,
                    entry_id: platform_id("team", "model"),
                },
                alias: "My catalog alias".into(),
            },
        )
        .await
        .unwrap();
    let mut catalog = app.store.model_catalog().await.unwrap();
    assert_eq!(
        catalog.agent_models["shared_default"].roles,
        before.agent_models["shared_default"].roles
    );
    assert!(app
        .store
        .account_model_preferences(user)
        .await
        .unwrap()
        .selected_platform_ids
        .contains(&platform_id("team", "model")));
    let id = adopted["entry_id"].as_str().unwrap();
    assert_ne!(
        id,
        before.agent_models["shared_default"].roles[&model_selection::ModelRole::Standard].entry_id
    );
    catalog
        .agent_models
        .get_mut("shared_default")
        .unwrap()
        .roles
        .insert(
            model_selection::ModelRole::Standard,
            model_selection::RoleAssignment {
                entry_id: id.into(),
                backup_entry_ids: vec![],
            },
        );
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
    let mut caller = app.clone();
    caller.store = app.store.for_user(user).await.unwrap();
    let mut plan = catalog
        .resolve_role("shared_default", model_selection::ModelRole::Standard)
        .unwrap();
    caller.pin_model_plan(&mut plan).await.unwrap();
    let run = json!({"agent_id":"shared_default","selection_plan":plan,"failover_state":model_selection::FailoverState::default()});
    let selection = provider::ProviderSelection {
        limits: None,
        provider: provider::ProviderId::Deepseek,
        model: "fixture".into(),
    };
    assert_eq!(
        caller.tokens_for(&selection, &run).await.unwrap()["api_key"],
        "platform-fixture-key"
    );
    assert!(!serde_json::to_string(&run)
        .unwrap()
        .contains("platform-fixture-key"));
    app.store
        .platform_credential_set("team", "model", 3, Some("rotated-fixture-key"), "admin")
        .await
        .unwrap();
    assert!(caller.tokens_for(&selection, &run).await.is_err());
    assert!(!caller.run_model_authorized(&run).await);
}
#[tokio::test]
async fn run_logs_route_is_admin_authenticated_and_run_scoped() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let other = conversation(&app).await;
    let rid = queued(&app, &sid).await;
    let other_rid = queued(&app, &other).await;
    let mut tx = app.store.pool.begin().await.unwrap();
    app.store
        .insert_interaction(
            &mut tx,
            &sid,
            &rid,
            "step:1",
            &json!({"type":"message","content":"selected private step"}),
        )
        .await
        .unwrap();
    app.store
        .insert_interaction(
            &mut tx,
            &other,
            &other_rid,
            "step:1",
            &json!({"type":"message","content":"foreign private step"}),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let path = format!("/api/v3/sessions/{sid}/runs/{rid}/logs");
    let request = |uri: &str, cookie: Option<&str>| {
        let mut builder = Request::builder().uri(uri);
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", cookie);
        }
        builder.body(Body::empty()).unwrap()
    };
    assert_eq!(
        router(app.clone())
            .oneshot(request(&path, None))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let cookie = ability_auth(&app).await;
    assert_eq!(
        router(app.clone())
            .oneshot(request(
                &format!("/api/v3/sessions/{other}/runs/{rid}/logs"),
                Some(&cookie)
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        router(app.clone())
            .oneshot(request(&format!("{path}?after=-1"), Some(&cookie)))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    let response = router(app.clone())
        .oneshot(request(&path, Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["run_id"], rid);
    assert!(body["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["payload"]["content"] == "selected private step"));
    assert!(body["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|event| event["run_id"] == rid));
    assert!(!body.to_string().contains("foreign private step"));
    let next = router(app)
        .oneshot(request(
            &format!("{path}?after={}", body["cursor"]),
            Some(&cookie),
        ))
        .await
        .unwrap();
    let page: Value =
        serde_json::from_slice(&axum::body::to_bytes(next.into_body(), 65536).await.unwrap())
            .unwrap();
    assert!(page["events"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn wake_reads_require_fresh_approval_in_auto_serial_and_batch() {
    for count in [1, 2] {
        let (_root, app) = fixture().await;
        let sid = conversation(&app).await;
        app.store
            .put(
                &key("session_mode", &sid),
                &json!({
                    "mode":"write","execution_mode":"auto","revision":1
                }),
            )
            .await
            .unwrap();
        activate_codebase(&app, &sid).await;
        let mut policy = policy_default();
        policy["rules"]["file_read"] = json!("require_approval");
        app.store.put("policy", &policy).await.unwrap();
        let id = queued(&app, &sid).await;
        let mut run = app.run(&id).await.unwrap();
        run["post_run_followup"] = json!(true);
        run["phase"] = json!("tools");
        run["loaded_abilities"] = json!(["codebase_admin"]);
        let calls = (0..count)
            .map(|index| {
                json!({
                    "type":"function_call","call_id":format!("wake-read-{index}"),
                    "name":"file_read","arguments":json!({"path":"fixture"}).to_string()
                })
            })
            .collect::<Vec<_>>();
        for call in &calls {
            run["input"].as_array_mut().unwrap().push(call.clone());
        }
        run["calls"] = json!(calls);
        app.save(&mut run).await.unwrap();
        running(&app, &id, "wake-ask").await;
        tokio::time::timeout(
            Duration::from_secs(5),
            execute_run(
                app.clone(),
                id.clone(),
                "wake-ask".into(),
                CancellationToken::new(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let waiting = app.run(&id).await.unwrap();
        assert_eq!(waiting["status"], "waiting_approval");
        assert_eq!(waiting["pending"]["call_id"], "wake-read-0");
        assert_ne!(waiting["pending"]["human_approved"], true);
        assert_eq!(waiting["post_run_followup"], true);
        assert_eq!(
            session_mode(&app, &sid).await.unwrap()["execution_mode"],
            "auto"
        );
        if count == 2 {
            assert!(waiting["batch"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["state"] == "waiting"));
        }
    }
}

#[tokio::test]
async fn background_approved_reads_pin_authority_and_claim_identity() {
    for batched in [false, true] {
        let (_root, app) = fixture().await;
        let sid = conversation(&app).await;
        activate_codebase(&app, &sid).await;
        let mut policy = policy_default();
        policy["rules"]["file_read"] = json!("require_approval");
        app.store.put("policy", &policy).await.unwrap();
        let rid = queued(&app, &sid).await;
        running(&app, &rid, "background-origin").await;
        let mut run = app.run(&rid).await.unwrap();
        run["loaded_abilities"] = json!(["codebase_admin"]);
        let (policy, mode_revision, visibility, state) =
            ability_controls(&app, &sid).await.unwrap();
        let access = app
            .store
            .live_access(app.store.current_user_id(), "shared_default")
            .await
            .unwrap();
        let args = json!({"path":"fixture"});
        let mut pending = json!({
            "id":"background-pending","name":"file_read","call_id":"background-call",
            "arguments":args,"digest":digest(&args),"policy_version":policy["version"],
            "mode_revision":mode_revision,"visibility_revision":visibility["revision"],
            "ability_state_revision":state["revision"],"access_revision":access.revision,
            "state":"executing","decision":"consumed","human_approved":false
        });
        if batched {
            run["pending"] = Value::Null;
            run["batch"] = json!([{"state":"executing","pending":pending}]);
        } else {
            run["pending"] = pending.clone();
        }
        app.save(&mut run).await.unwrap();
        let settings = tool_execution::resolve(&visibility, "file_read").unwrap();
        let version = policy["version"].as_u64().unwrap_or(0);
        assert!(background_tasks::submit(
            &app,
            &sid,
            &rid,
            "background-origin",
            "file_read",
            &args,
            "background-pending",
            version,
            &settings
        )
        .await
        .is_err());
        pending["human_approved"] = json!(true);
        if batched {
            run["batch"][0]["pending"] = pending;
        } else {
            run["pending"] = pending;
        }
        app.save(&mut run).await.unwrap();
        let handle = background_tasks::submit(
            &app,
            &sid,
            &rid,
            "background-origin",
            "file_read",
            &args,
            "background-pending",
            version,
            &settings,
        )
        .await
        .unwrap();
        assert_eq!(
            background_tasks::submit(
                &app,
                &sid,
                &rid,
                "background-origin",
                "file_read",
                &args,
                "background-pending",
                version,
                &settings
            )
            .await
            .unwrap(),
            handle
        );
        assert_eq!(
            app.store.background_scan(Some(&sid)).await.unwrap().len(),
            1
        );
        let task_id = handle["task_id"].as_str().unwrap();
        let mut task = app.store.background_get(task_id).await.unwrap().unwrap();
        assert!(background_tasks::permitted(&app, &task).await.unwrap());
        for field in [
            "policy_version",
            "mode_revision",
            "visibility_revision",
            "ability_state_revision",
            "access_revision",
        ] {
            let mut stale = task.clone();
            stale["authorization"][field] = json!("stale");
            assert!(!background_tasks::permitted(&app, &stale).await.unwrap());
        }
        let mut unapproved = task.clone();
        unapproved["authorization"]["human_approved"] = json!(false);
        assert!(!background_tasks::permitted(&app, &unapproved)
            .await
            .unwrap());
        task["status"] = json!("running");
        task["claim_id"] = json!("claim-one");
        task["lease_until"] = json!(now() + 60);
        assert!(app.store.background_cas(&task, Some(0)).await.unwrap());
        assert_eq!(
            background_tasks::dispatch_authorized(
                &app,
                &sid,
                &rid,
                "claim-one",
                task_id,
                "file_read",
                &args
            )
            .await
            .unwrap(),
            Some(true)
        );
        task["claim_id"] = json!("claim-two");
        assert!(app.store.background_cas(&task, Some(1)).await.unwrap());
        assert_eq!(
            background_tasks::dispatch_authorized(
                &app,
                &sid,
                &rid,
                "claim-one",
                task_id,
                "file_read",
                &args
            )
            .await
            .unwrap(),
            Some(false)
        );
        assert_eq!(
            background_tasks::dispatch_authorized(
                &app,
                &sid,
                &rid,
                "claim-two",
                task_id,
                "file_read",
                &args
            )
            .await
            .unwrap(),
            Some(true)
        );
        let mut recovered = app.run(&rid).await.unwrap();
        recovered["generation"] = json!("recovered-source-generation");
        recovered["loaded_abilities"] = json!(["codebase_admin", "later-activation"]);
        app.store.put(&key("run", &rid), &recovered).await.unwrap();
        assert!(background_tasks::permitted(&app, &task).await.unwrap());
        for source_status in ["running", "waiting_approval", "waiting_external", "queued"] {
            recovered["status"] = json!(source_status);
            app.store.put(&key("run", &rid), &recovered).await.unwrap();
            assert_eq!(
                background_tasks::dispatch_authorized(
                    &app,
                    &sid,
                    &rid,
                    "claim-two",
                    task_id,
                    "file_read",
                    &args
                )
                .await
                .unwrap(),
                Some(true)
            );
        }
        for source_status in ["paused", "stopped", "cancelled", "failed"] {
            recovered["status"] = json!(source_status);
            app.store.put(&key("run", &rid), &recovered).await.unwrap();
            assert!(!background_tasks::permitted(&app, &task).await.unwrap());
            assert_eq!(
                background_tasks::dispatch_authorized(
                    &app,
                    &sid,
                    &rid,
                    "claim-two",
                    task_id,
                    "file_read",
                    &args
                )
                .await
                .unwrap(),
                Some(false)
            );
        }
        recovered["status"] = json!("completed");
        app.store.put(&key("run", &rid), &recovered).await.unwrap();
        assert_eq!(
            background_tasks::dispatch_authorized(
                &app,
                &sid,
                &rid,
                "claim-two",
                task_id,
                "file_read",
                &args
            )
            .await
            .unwrap(),
            Some(false)
        );
        recovered["status"] = json!("running");
        app.store.put(&key("run", &rid), &recovered).await.unwrap();
        let mut revoked = policy.clone();
        revoked["version"] = json!(version + 1);
        revoked["rules"]["file_read"] = json!("deny");
        app.store.put("policy", &revoked).await.unwrap();
        assert!(!background_tasks::permitted(&app, &task).await.unwrap());
    }
}

#[tokio::test]
async fn foreground_queue_revocation_denies_before_worker_dispatch() {
    let (_root, app) = fixture().await;
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["tool_execution"] = json!({"file_list":{
        "mode":"foreground","max_concurrency":1,"on_run_end":"stop"
    }});
    visibility["revision"] = json!(revision + 1);
    app.store
        .save_agent_abilities(&visibility, revision)
        .await
        .unwrap();
    let sid = conversation(&app).await;
    activate_codebase(&app, &sid).await;
    let rid = queued(&app, &sid).await;
    running(&app, &rid, "queued-revocation").await;
    let mut run = app.run(&rid).await.unwrap();
    run["loaded_abilities"] = json!(["codebase_admin"]);
    let args = json!({"path":"fixture"});
    let (policy, mode_revision, visibility, state) = ability_controls(&app, &sid).await.unwrap();
    let access = app
        .store
        .live_access(app.store.current_user_id(), "shared_default")
        .await
        .unwrap();
    run["pending"] = json!({
        "id":"queued-revocation-pending","name":"file_list","call_id":"queued-revocation-call",
        "arguments":args,"digest":digest(&args),"policy_version":policy["version"],
        "mode_revision":mode_revision,"visibility_revision":visibility["revision"],
        "ability_state_revision":state["revision"],"access_revision":access.revision,
        "state":"executing","decision":"consumed","human_approved":false
    });
    app.save(&mut run).await.unwrap();
    let settings = tool_execution::resolve(&visibility, "file_list").unwrap();
    let cancel = CancellationToken::new();
    let blocker = tokio::time::timeout(
        Duration::from_secs(5),
        tool_execution::acquire(
            app.store.current_user_id(),
            "shared_default",
            "file_list",
            &settings,
            &cancel,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let listener = tokio::net::UnixListener::bind(&app.tool_socket).unwrap();
    let operation = tools::call(
        app.clone(),
        sid,
        rid,
        "queued-revocation".into(),
        "file_list".into(),
        args,
        "queued-revocation-pending".into(),
        policy["version"].as_u64().unwrap_or(0),
        cancel,
    );
    tokio::pin!(operation);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut operation)
            .await
            .is_err()
    );
    let mut revoked = policy.clone();
    revoked["version"] = json!(policy["version"].as_u64().unwrap_or(0) + 1);
    revoked["rules"]["file_list"] = json!("deny");
    app.store.put("policy", &revoked).await.unwrap();
    drop(blocker);
    let result = tokio::time::timeout(Duration::from_secs(5), &mut operation)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result["status"], "denied");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn background_auto_reads_reauthorize_live_grants_without_inheriting_approval() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    activate_codebase(&app, &sid).await;
    let rid = queued(&app, &sid).await;
    running(&app, &rid, "auto-background").await;
    let mut run = app.run(&rid).await.unwrap();
    run["loaded_abilities"] = json!(["codebase_admin"]);
    let (policy, mode_revision, visibility, state) = ability_controls(&app, &sid).await.unwrap();
    let access = app
        .store
        .live_access(app.store.current_user_id(), "shared_default")
        .await
        .unwrap();
    let args = json!({"path":"fixture"});
    run["pending"] = json!({
        "id":"auto-background-pending","name":"file_read","call_id":"auto-background-call",
        "arguments":args,"digest":digest(&args),"policy_version":policy["version"],
        "mode_revision":mode_revision,"visibility_revision":visibility["revision"],
        "ability_state_revision":state["revision"],"access_revision":access.revision,
        "state":"executing","decision":"consumed","human_approved":false
    });
    app.save(&mut run).await.unwrap();
    let settings = tool_execution::resolve(&visibility, "file_read").unwrap();
    let handle = background_tasks::submit(
        &app,
        &sid,
        &rid,
        "auto-background",
        "file_read",
        &args,
        "auto-background-pending",
        policy["version"].as_u64().unwrap_or(0),
        &settings,
    )
    .await
    .unwrap();
    let task = app
        .store
        .background_get(handle["task_id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    let mut changed_state = state.clone();
    changed_state["revision"] = json!(state["revision"].as_u64().unwrap() + 1);
    app.store
        .put(&key("session_ability_state", &sid), &changed_state)
        .await
        .unwrap();
    let mut current_policy = policy.clone();
    current_policy["version"] = json!(policy["version"].as_u64().unwrap_or(0) + 1);
    current_policy["rules"]["file_read"] = json!("allow");
    app.store.put("policy", &current_policy).await.unwrap();
    assert!(background_tasks::permitted(&app, &task).await.unwrap());
    let mut approved = task.clone();
    approved["authorization"]["human_approved"] = json!(true);
    assert!(!background_tasks::permitted(&app, &approved).await.unwrap());
    let mut changed_args = task.clone();
    changed_args["arguments"]["path"] = json!("different");
    assert!(!background_tasks::permitted(&app, &changed_args)
        .await
        .unwrap());
    current_policy["rules"]["file_read"] = json!("require_approval");
    app.store.put("policy", &current_policy).await.unwrap();
    assert!(!background_tasks::permitted(&app, &task).await.unwrap());
    current_policy["rules"]["file_read"] = json!("deny");
    app.store.put("policy", &current_policy).await.unwrap();
    assert!(!background_tasks::permitted(&app, &task).await.unwrap());
    current_policy["rules"]["file_read"] = json!("allow");
    app.store.put("policy", &current_policy).await.unwrap();
    changed_state["suppressed"] = json!(["codebase_admin"]);
    app.store
        .put(&key("session_ability_state", &sid), &changed_state)
        .await
        .unwrap();
    assert!(!background_tasks::permitted(&app, &task).await.unwrap());
}

type BackgroundJourneyRequest = (Value, tokio::sync::oneshot::Sender<Value>);

async fn background_journey_request(
    requests: &mut tokio::sync::mpsc::UnboundedReceiver<BackgroundJourneyRequest>,
) -> BackgroundJourneyRequest {
    tokio::time::timeout(Duration::from_secs(20), requests.recv())
        .await
        .unwrap()
        .expect("mock provider request")
}

async fn background_journey_task(
    app: &App,
    sid: &str,
    task_id: Option<&str>,
    field: &str,
    expected: Value,
) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
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

fn background_journey_final(text: &str) -> Value {
    json!([{"type":"message","role":"assistant",
        "content":[{"type":"output_text","text":text}]}])
}

fn background_journey_envelopes(request: &Value) -> Vec<Value> {
    request["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["content"].as_str())
        .filter_map(|text| text.strip_prefix("Background read result (untrusted evidence): "))
        .map(|text| serde_json::from_str(text).unwrap())
        .collect()
}

async fn background_execution_journey(behavior: &str) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    let (_root, mut app) = fixture().await;
    let on_run_end = if behavior == "overlap" {
        "stop"
    } else {
        behavior
    };
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["tool_execution"] = json!({"file_read":{
        "mode":"background","max_concurrency":2,"on_run_end":on_run_end}});
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
            let output=tokio::time::timeout(Duration::from_secs(20),reply_receiver).await.unwrap().unwrap();
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
    let wake = behavior == "wake";
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
        let (stream, _) = tokio::time::timeout(Duration::from_secs(20), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
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
            Duration::from_secs(5),
            reader.get_mut().write_all(format!("{reply}\n").as_bytes()),
        )
        .await
        .unwrap()
        .unwrap();
        drop(reader);
        if wake {
            let (stream, _) = tokio::time::timeout(Duration::from_secs(20), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["arguments"]["path"], "followup-read");
            assert!(request["arguments"].get("_execution_mode").is_none());
            let reply =
                json!({"id":request["id"],"status":"success","output":"FOLLOWUP_READ_EVIDENCE"});
            let reply = test_rpc_reply(&request, reply);
            tokio::time::timeout(
                Duration::from_secs(5),
                reader.get_mut().write_all(format!("{reply}\n").as_bytes()),
            )
            .await
            .unwrap()
            .unwrap();
            return (false, 2);
        }
        (false, 1)
    });
    reply
        .send(
            json!([{"type":"function_call","call_id":"journey-background","name":"file_read",
        "arguments":json!({"path":"background-evidence"}).to_string()}]),
        )
        .unwrap();
    let (advanced, reply) = background_journey_request(&mut requests).await;
    tokio::time::timeout(Duration::from_secs(5), arrived_receiver)
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
    assert_eq!(handles[0]["task_id"], task_id);
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
        tokio::time::timeout(Duration::from_secs(5), origin)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), worker)
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
        tokio::time::timeout(Duration::from_secs(5), origin)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(app.run(&rid).await.unwrap()["status"], "completed");
        if behavior == "stop" {
            background_journey_task(&app, &sid, Some(&task_id), "status", json!("cancelled")).await;
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), worker)
                    .await
                    .unwrap()
                    .unwrap(),
                (true, 1)
            );
            drop(release_sender);
            let task = app.store.background_get(&task_id).await.unwrap().unwrap();
            assert!(task["wake_run_id"].is_null());
            assert!(task["delivery_run_id"].is_null());
            let mut finished = app.run(&rid).await.unwrap();
            assert!(!background_tasks::collect(&app, &mut finished)
                .await
                .unwrap());
        } else {
            assert_eq!(
                app.store.background_get(&task_id).await.unwrap().unwrap()["status"],
                "running"
            );
            release_sender.send(()).unwrap();
            background_journey_task(&app, &sid, Some(&task_id), "status", json!("completed")).await;
            if behavior == "silent" {
                let task =
                    background_journey_task(&app, &sid, Some(&task_id), "notified", json!(true))
                        .await;
                assert!(task["wake_run_id"].is_null());
                assert!(task["delivery_run_id"].is_null());
                assert!(
                    requests.try_recv().is_err(),
                    "silent completion must not call the provider"
                );
                assert_eq!(
                    tokio::time::timeout(Duration::from_secs(5), worker)
                        .await
                        .unwrap()
                        .unwrap(),
                    (false, 1)
                );
                let next = queued(&app, &sid).await;
                running(&app, &next, "silent-next-turn").await;
                let next_app = app.clone();
                let next_id = next.clone();
                let next_run = tokio::spawn(async move {
                    execute_fixture_journey(
                        next_app,
                        next_id,
                        "silent-next-turn".into(),
                        CancellationToken::new(),
                    )
                    .await;
                });
                let (request, reply) = background_journey_request(&mut requests).await;
                let envelopes = background_journey_envelopes(&request);
                assert_eq!(envelopes.len(), 1);
                assert_eq!(envelopes[0]["background_task_id"], task_id);
                assert_eq!(envelopes[0]["origin_run_id"], rid);
                assert_eq!(envelopes[0]["post_run_result"], true);
                assert_eq!(
                    envelopes[0]["untrusted_tool_result"]["output"],
                    "BACKGROUND_JOURNEY_EVIDENCE"
                );
                reply
                    .send(background_journey_final("Silent result acknowledged."))
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(5), next_run)
                    .await
                    .unwrap()
                    .unwrap();
                let mut next_finished = app.run(&next).await.unwrap();
                assert_eq!(next_finished["status"], "completed");
                assert!(!background_tasks::collect(&app, &mut next_finished)
                    .await
                    .unwrap());
            } else {
                let task = tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let task = app.store.background_get(&task_id).await.unwrap().unwrap();
                        if task["wake_run_id"].is_string() {
                            break task;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                let follow_id = task["wake_run_id"].as_str().unwrap().to_owned();
                let follow = app.run(&follow_id).await.unwrap();
                assert_eq!(follow["post_run_followup"], true);
                assert!(follow["pending"].is_null());
                assert_eq!(follow["source_background_task"], task_id);
                let evidence: Value =
                    serde_json::from_str(follow["input"][0]["content"].as_str().unwrap()).unwrap();
                assert_eq!(evidence["source_kind"], "untrusted_delayed_tool_result");
                assert_eq!(evidence["result"]["output"], "BACKGROUND_JOURNEY_EVIDENCE");
                running(&app, &follow_id, "wake-followup").await;
                let follow_app = app.clone();
                let follow_run_id = follow_id.clone();
                let follow_run = tokio::spawn(async move {
                    execute_fixture_journey(
                        follow_app,
                        follow_run_id,
                        "wake-followup".into(),
                        CancellationToken::new(),
                    )
                    .await;
                });
                let (_, reply) = background_journey_request(&mut requests).await;
                reply.send(json!([
                    {"type":"function_call","call_id":"forbidden-followup-write","name":"file_write",
                        "arguments":json!({"path":"forbidden","content":"no"}).to_string()},
                    {"type":"function_call","call_id":"foreground-followup-read","name":"file_read",
                        "arguments":json!({"path":"followup-read","_execution_mode":"background"}).to_string()}
                ])).unwrap();
                let (request, reply) = background_journey_request(&mut requests).await;
                for (call_id, expected) in [
                    ("forbidden-followup-write", "denied"),
                    ("foreground-followup-read", "success"),
                ] {
                    let output = request["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|item| {
                            item["type"] == "function_call_output" && item["call_id"] == call_id
                        })
                        .unwrap();
                    let result: Value =
                        serde_json::from_str(output["output"].as_str().unwrap()).unwrap();
                    assert_eq!(result["status"], expected);
                }
                let source_task = app.store.background_get(&task_id).await.unwrap().unwrap();
                assert_eq!(source_task["status"], "completed");
                assert_eq!(source_task["delivery_run_id"], follow_id);
                assert_eq!(source_task["wake_run_id"], follow_id);
                let task_count: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM background_task_index WHERE session_id=?",
                )
                .bind(&sid)
                .fetch_one(&app.store.pool)
                .await
                .unwrap();
                assert_eq!(
                    task_count, 1,
                    "follow-up must not create any additional task"
                );
                reply
                    .send(background_journey_final("Wake result acknowledged."))
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(5), follow_run)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    tokio::time::timeout(Duration::from_secs(5), worker)
                        .await
                        .unwrap()
                        .unwrap(),
                    (false, 2)
                );
                assert_eq!(app.run(&follow_id).await.unwrap()["status"], "completed");
                let task = app.store.background_get(&task_id).await.unwrap().unwrap();
                background_followup::process(&app, &task).await.unwrap();
                assert_eq!(
                    app.store.background_get(&task_id).await.unwrap().unwrap()["wake_run_id"],
                    follow_id
                );
            }
            assert_eq!(
                app.store
                    .chat_events(&sid, 0)
                    .await
                    .unwrap()
                    .iter()
                    .filter(|event| event["id"] == format!("working:{rid}:background:{task_id}"))
                    .count(),
                1
            );
        }
        assert_eq!(app.run(&rid).await.unwrap()["status"], "completed");
    }
    let expected_replies = match behavior {
        "overlap" => vec!["Overlap completed."],
        "silent" => vec!["Origin completed.", "Silent result acknowledged."],
        "wake" => vec!["Origin completed.", "Wake result acknowledged."],
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
    tokio::time::timeout(Duration::from_secs(5), service)
        .await
        .unwrap()
        .unwrap();
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn background_read_overlaps_model_and_delivers_at_next_boundary() {
    background_execution_journey("overlap").await;
}

#[tokio::test]
async fn background_stop_cancels_outstanding_worker_after_final_reply() {
    background_execution_journey("stop").await;
}

#[tokio::test]
async fn background_silent_continues_then_delivers_on_next_user_turn() {
    background_execution_journey("silent").await;
}

#[tokio::test]
async fn background_wake_runs_one_restricted_followup() {
    background_execution_journey("wake").await;
}

#[tokio::test]
async fn background_reclaimed_submission_preserves_original_task_and_receipt() {
    for batched in [false, true] {
        let (_root, app) = fixture().await;
        let sid = conversation(&app).await;
        activate_codebase(&app, &sid).await;
        let rid = queued(&app, &sid).await;
        running(&app, &rid, "first-background-generation").await;
        let mut run = app.run(&rid).await.unwrap();
        run["loaded_abilities"] = json!(["codebase_admin"]);
        let args = json!({"path":"fixture"});
        let (policy, mode_revision, visibility, state) =
            ability_controls(&app, &sid).await.unwrap();
        let access = app
            .store
            .live_access(app.store.current_user_id(), "shared_default")
            .await
            .unwrap();
        let pending = json!({
            "id":"first-pending","name":"file_read","call_id":"stable-background-call",
            "arguments":args,"digest":digest(&args),"policy_version":policy["version"],
            "mode_revision":mode_revision,"visibility_revision":visibility["revision"],
            "ability_state_revision":state["revision"],"access_revision":access.revision,
            "state":"executing","decision":"consumed","human_approved":false
        });
        if batched {
            run["pending"] = Value::Null;
            run["batch"] = json!([{"state":"executing","pending":pending}]);
        } else {
            run["pending"] = pending;
        }
        app.save(&mut run).await.unwrap();
        let settings = tool_execution::resolve(&visibility, "file_read").unwrap();
        let handle = background_tasks::submit(
            &app,
            &sid,
            &rid,
            "first-background-generation",
            "file_read",
            &args,
            "first-pending",
            policy["version"].as_u64().unwrap_or(0),
            &settings,
        )
        .await
        .unwrap();
        let task_id = handle["task_id"].as_str().unwrap();
        let original = app.store.background_get(task_id).await.unwrap().unwrap();
        let mut current_policy = policy.clone();
        current_policy["version"] = json!(policy["version"].as_u64().unwrap_or(0) + 1);
        current_policy["rules"]["file_read"] = json!("require_approval");
        app.store.put("policy", &current_policy).await.unwrap();
        let mut visibility = app.store.agent_abilities().await.unwrap();
        let revision = visibility["revision"].as_u64().unwrap();
        visibility["revision"] = json!(revision + 1);
        visibility["tool_execution"]["file_read"] = json!({
            "mode":"model","max_concurrency":1,"on_run_end":"wake"
        });
        app.store
            .save_agent_abilities(&visibility, revision)
            .await
            .unwrap();
        let (policy, mode_revision, visibility, state) =
            ability_controls(&app, &sid).await.unwrap();
        let access = app
            .store
            .live_access(app.store.current_user_id(), "shared_default")
            .await
            .unwrap();
        run["generation"] = json!("reclaimed-background-generation");
        run["loaded_abilities"] = json!(["codebase_admin", "later-activation"]);
        let mut pending = json!({
            "id":"reclaimed-pending","name":"file_read","call_id":"stable-background-call",
            "arguments":args,"digest":digest(&args),"policy_version":policy["version"],
            "mode_revision":mode_revision,"visibility_revision":visibility["revision"],
            "ability_state_revision":state["revision"],"access_revision":access.revision,
            "state":"executing","decision":"consumed","human_approved":true
        });
        if batched {
            run["batch"][0]["pending"] = pending.clone();
        } else {
            run["pending"] = pending.clone();
        }
        app.store.put(&key("run", &rid), &run).await.unwrap();
        let settings = tool_execution::resolve(&visibility, "file_read").unwrap();
        let version = policy["version"].as_u64().unwrap_or(0);
        assert_eq!(
            background_tasks::submit(
                &app,
                &sid,
                &rid,
                "reclaimed-background-generation",
                "file_read",
                &args,
                "reclaimed-pending",
                version,
                &settings
            )
            .await
            .unwrap(),
            handle
        );
        assert_eq!(
            app.store.background_get(task_id).await.unwrap().unwrap(),
            original
        );
        assert!(!background_tasks::permitted(&app, &original).await.unwrap());
        let mut completed = original.clone();
        completed["status"] = json!("completed");
        completed["result"] = json!({"status":"success","output":"original read result"});
        assert!(app.store.background_cas(&completed, Some(0)).await.unwrap());
        let completed = app.store.background_get(task_id).await.unwrap().unwrap();
        assert_eq!(
            background_tasks::submit(
                &app,
                &sid,
                &rid,
                "reclaimed-background-generation",
                "file_read",
                &args,
                "reclaimed-pending",
                version,
                &settings
            )
            .await
            .unwrap(),
            handle
        );
        assert_eq!(
            app.store.background_get(task_id).await.unwrap().unwrap(),
            completed
        );
        assert_eq!(
            app.store.background_scan(Some(&sid)).await.unwrap().len(),
            1
        );
        let different_args = json!({"path":"different"});
        pending["arguments"] = different_args.clone();
        pending["digest"] = json!(digest(&different_args));
        if batched {
            run["batch"][0]["pending"] = pending;
        } else {
            run["pending"] = pending;
        }
        app.store.put(&key("run", &rid), &run).await.unwrap();
        assert!(background_tasks::submit(
            &app,
            &sid,
            &rid,
            "reclaimed-background-generation",
            "file_read",
            &different_args,
            "reclaimed-pending",
            version,
            &settings
        )
        .await
        .is_err());
        if batched {
            run["batch"][0]["pending"]["arguments"] = args.clone();
            run["batch"][0]["pending"]["digest"] = json!(digest(&args));
        } else {
            run["pending"]["arguments"] = args.clone();
            run["pending"]["digest"] = json!(digest(&args));
        }
        app.store.put(&key("run", &rid), &run).await.unwrap();
        current_policy["rules"]["file_read"] = json!("deny");
        app.store.put("policy", &current_policy).await.unwrap();
        assert!(background_tasks::submit(
            &app,
            &sid,
            &rid,
            "reclaimed-background-generation",
            "file_read",
            &args,
            "reclaimed-pending",
            version,
            &settings
        )
        .await
        .is_err());
        assert_eq!(
            app.store.background_get(task_id).await.unwrap().unwrap(),
            completed
        );
    }
}

/// Keep the Tokio test on its ordinary stack; production schedules each run as a task.
#[tokio::test]
async fn normal_dispatch_reloads_live_prompt_slots_and_selected_skill_on_default_stack() {
    Box::pin(live_context_default_stack_journey()).await;
}

/// Copy only native Base source into this fixture; shared repository inputs are read-only.
fn copy_live_context_fixture(source: &std::path::Path, target: &std::path::Path) {
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
fn live_context_fixture_skill(body: &str) -> String {
    format!("---\nid: live_refresh\ntitle: Live refresh fixture\nwhen_to_use: Verify current live context\ndefault_mode: discoverable\n---\n{body}")
}

/// Drive native load_skill and a second tool round through the actual execute_run loop.
/// Current source prompts, saved overrides, encrypted slots and selected skill files
/// must replace old text without copying guidance into durable history or selectors.
async fn live_context_default_stack_journey() {
    use axum::routing::post;
    let (root, mut app) = Box::pin(fixture()).await;
    Box::pin(visibility_fixture_base_access(&app)).await;
    Box::pin(visibility_fixture_ability_access(&app, "web_access")).await;
    let source = root.path().join("live-source");
    let abilities = source.join("plugins/abilities");
    // Base is built in, so the live selectable skill lives in a folder ability.
    copy_live_context_fixture(
        &app.abilities.snapshot_root().join("Web/web_access"),
        &abilities.join("Web/web_access"),
    );
    let skill = abilities.join("Web/web_access/skills/live_refresh.skill.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    std::fs::write(&skill, live_context_fixture_skill("LIVE_SKILL_FIRST")).unwrap();
    // The catalog source carries its own prompt files; edits to them are adopted live.
    let prompt = source.join(webagent_configuration::prompts::PROMPTS);
    copy_live_context_fixture(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(webagent_configuration::prompts::PROMPTS),
        &prompt,
    );
    for (name, text) in [
        ("base_instructions", "LIVE_MAIN_FIRST"),
        ("closing_reporting", "LIVE_CLOSING"),
    ] {
        std::fs::write(prompt.join(format!("{name}.md")), text).unwrap();
    }
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
        let captured=captured.clone();let store=callback_store.clone();let prompt=prompt.clone();let skill=skill.clone();
        Box::pin(async move {
            let mut captured=captured.lock().await;
            captured.push(body);
            let output=match captured.len() {
                1=>json!([{"type":"function_call","call_id":"select-live-skill","name":"load_skill",
                    "arguments":"{\"ability_id\":\"web_access\",\"kind\":\"selectable\",\"skill_id\":\"live_refresh\"}"}]),
                2=>{
                    std::fs::write(prompt.join("base_instructions.md"),"LIVE_MAIN_CURRENT").unwrap();
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
    tokio::time::timeout(Duration::from_secs(30), execution)
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
        assert!(instructions.contains("LIVE_MAIN_FIRST"));
        assert!(instructions.contains("LIVE_SLOT_FIRST"));
        assert!(!instructions.contains("LIVE_MAIN_CURRENT"));
        assert!(!instructions.contains("LIVE_SLOT_CURRENT"));
    }
    let instructions = requests[2]["instructions"].as_str().unwrap();
    assert!(instructions.contains("LIVE_MAIN_CURRENT"));
    assert!(instructions.contains("LIVE_SLOT_CURRENT"));
    assert!(!instructions.contains("LIVE_MAIN_FIRST"));
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
        let instructions = request["instructions"].as_str().unwrap();
        assert!(instructions.contains("LIVE_CLOSING"));
    }
    assert!(!requests[0]["input"]
        .to_string()
        .contains("LIVE_SKILL_FIRST"));
    assert!(requests[1]["input"]
        .to_string()
        .contains("LIVE_SKILL_FIRST"));
    assert!(requests[2]["input"]
        .to_string()
        .contains("LIVE_SKILL_CURRENT"));
    assert!(!requests[2]["input"]
        .to_string()
        .contains("LIVE_SKILL_FIRST"));
    for marker in ["LIVE_SKILL_FIRST", "LIVE_SKILL_CURRENT"] {
        assert!(!durable["input"].to_string().contains(marker));
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

#[tokio::test]
async fn experimental_lifetime_controls_stay_disabled_without_an_extra_call() {
    use axum::routing::post;
    let (root, mut app) = fixture().await;
    std::fs::create_dir_all(root.path().join("config")).unwrap();
    let policy = crate::tool_output::Policy {
        enabled: true,
        scope: crate::tool_output::Scope::AllEligible,
        ..Default::default()
    };
    std::fs::write(
        root.path().join("config/tool-output-retention.json"),
        serde_json::to_vec(&policy).unwrap(),
    )
    .unwrap();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let mock=Router::new().route("/responses",post(
        |AxumState(requests): AxumState<Arc<Mutex<Vec<Value>>>>,Json(body):Json<Value>| async move {
            requests.lock().await.push(body);
            let event=json!({"type":"response.completed","response":{"status":"completed","output":[
                {"type":"message","role":"assistant","content":[{"type":"output_text","text":"Kept the relevant source."}]}
            ]}});
            ([("content-type","text/event-stream")],format!("data: {event}\n\n"))
        })).with_state(captured);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    run["input"].as_array_mut().unwrap().extend([
        json!({"type":"function_call","call_id":"read-important","name":"file_read",
            "arguments":json!({"path":"src/example.rs"}).to_string()}),
        json!({"type":"function_call_output","call_id":"read-important",
            "output":json!({"status":"success","path":"src/example.rs","content":"fn important() {}"}).to_string()})
    ]);
    app.save(&mut run).await.unwrap();
    running(&app, &id, "retention-final").await;
    execute_run(
        app.clone(),
        id.clone(),
        "retention-final".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let run = app.run(&id).await.unwrap();
    assert_eq!(run["status"], "completed");
    assert!(run["tool_context_state"]["entries"]
        .as_array()
        .unwrap()
        .is_empty());
    let session = app
        .store
        .get(&format!("session:{sid}"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session["tool_context_state"], run["tool_context_state"]);
    let (messages, _) = app.store.chat_snapshot(&sid).await.unwrap();
    assert_eq!(
        messages.iter().filter(|m| m["role"] == "assistant").count(),
        1
    );
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 1);
    assert!(!requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "manage_tool_context"));
    server.abort();
}
#[tokio::test]
async fn retained_read_recall_resolves_only_verified_same_session_origin() {
    use sha2::Digest;
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let origin_id = queued(&app, &sid).await;
    let foreign_sid = conversation(&app).await;
    let foreign_origin = queued(&app, &foreign_sid).await;
    let mut policy = app.store.get("policy").await.unwrap().unwrap();
    policy["rules"]["file_read"] = json!("allow");
    policy["rules"]["recall_compacted"] = json!("allow");
    app.store.put("policy", &policy).await.unwrap();
    let mut tier = app
        .store
        .entitlement_tier("operator")
        .await
        .unwrap()
        .unwrap();
    let tier_revision = tier.revision;
    tier.ability_permissions
        .insert("base".into(), access::Permission::Auto);
    app.store
        .save_entitlement_tier(tier, tier_revision)
        .await
        .unwrap();
    let mut level = app
        .store
        .live_access("admin", "shared_default")
        .await
        .unwrap()
        .capability_level;
    level
        .ability_permissions
        .insert("base".into(), access::Permission::Auto);
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
    activate_codebase(&app, &sid).await;
    session_ability_set(
        State(app.clone()),
        Path((sid.clone(), "base".into())),
        Json(SessionAbilityBody {
            state: "active".into(),
            expected_revision: 1,
        }),
    )
    .await
    .unwrap();
    let current_permissions = app
        .store
        .agent_tool_permissions("shared_default")
        .await
        .unwrap();
    app.store
        .set_agent_tool_permissions(
            "shared_default",
            current_permissions["revision"].as_u64().unwrap(),
            [("file_read".to_owned(), access::Permission::Auto)].into(),
        )
        .await
        .unwrap();
    let raw = "retained source excerpt";
    let call_id = "cross-run-read";
    let call =
        json!({"type":"function_call","call_id":call_id,"name":"file_read","arguments":"{}"});
    let mut origin = app.run(&origin_id).await.unwrap();
    app.store
        .save_run(&mut origin, "model_output", &json!({"output":[call]}))
        .await
        .unwrap();
    origin["input"].as_array_mut().unwrap().push(json!({
        "type":"function_call_output","call_id":call_id,"output":raw
    }));
    app.store
        .save_run(
            &mut origin,
            "tool_result",
            &json!({"run_id":origin_id,
        "name":"file_read","result":{"status":"success","output":"retained source excerpt"}}),
        )
        .await
        .unwrap();
    let mut origin = app.run(&origin_id).await.unwrap();
    origin["status"] = json!("completed");
    origin["turns"] = json!(1);
    origin["pending"] = Value::Null;
    app.save(&mut origin).await.unwrap();
    let recall_id = queued(&app, &sid).await;
    let mut recall = app.run(&recall_id).await.unwrap();
    recall["loaded_abilities"] = json!(["base", "codebase_admin"]);
    recall["tool_context_state"] = json!({"clock":1,"entries":[{
        "call_id":call_id,"origin_run_id":origin_id,"tool_name":"file_read",
        "output_sha256":hex::encode(sha2::Sha256::digest(raw.as_bytes())),
        "output_chars":raw.chars().count(),"promoted":false,"first_seen":1,"last_used":1
    }]});
    app.save(&mut recall).await.unwrap();
    let request = json!({"tool_call_id":call_id,"offset_chars":0,"limit_chars":4096});
    let result = crate::tools::native::recall::execute(
        &crate::tools::native_host::Host(&app),
        crate::tools::native::Call {
            sid: &sid,
            run_id: &recall_id,
            generation: "",
            pending_id: "",
        },
        "recall_compacted",
        &request,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result["status"], "success", "{result}");
    assert_eq!(result["output"]["content"], raw);
    let mut tampered = app.run(&recall_id).await.unwrap();
    tampered["tool_context_state"]["entries"][0]["output_sha256"] = json!("0".repeat(64));
    app.save(&mut tampered).await.unwrap();
    let rejected = crate::tools::native::recall::execute(
        &crate::tools::native_host::Host(&app),
        crate::tools::native::Call {
            sid: &sid,
            run_id: &recall_id,
            generation: "",
            pending_id: "",
        },
        "recall_compacted",
        &request,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_ne!(rejected["output"]["content"], raw);
    let mut foreign = app.run(&recall_id).await.unwrap();
    foreign["tool_context_state"]["entries"][0]["output_sha256"] =
        json!(hex::encode(sha2::Sha256::digest(raw.as_bytes())));
    foreign["tool_context_state"]["entries"][0]["origin_run_id"] = json!(foreign_origin);
    app.save(&mut foreign).await.unwrap();
    let rejected = crate::tools::native::recall::execute(
        &crate::tools::native_host::Host(&app),
        crate::tools::native::Call {
            sid: &sid,
            run_id: &recall_id,
            generation: "",
            pending_id: "",
        },
        "recall_compacted",
        &request,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_ne!(rejected["output"]["content"], raw);
    let current_permissions = app
        .store
        .agent_tool_permissions("shared_default")
        .await
        .unwrap();
    app.store
        .set_agent_tool_permissions(
            "shared_default",
            current_permissions["revision"].as_u64().unwrap(),
            [("file_read".to_owned(), access::Permission::Deny)].into(),
        )
        .await
        .unwrap();
    let mut permitted_origin = app.run(&recall_id).await.unwrap();
    permitted_origin["tool_context_state"]["entries"][0]["origin_run_id"] = json!(origin_id);
    app.save(&mut permitted_origin).await.unwrap();
    let rejected = crate::tools::native::recall::execute(
        &crate::tools::native_host::Host(&app),
        crate::tools::native::Call {
            sid: &sid,
            run_id: &recall_id,
            generation: "",
            pending_id: "",
        },
        "recall_compacted",
        &request,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_ne!(rejected["output"]["content"], raw);
}

#[tokio::test]
async fn persisted_code_index_status_survives_snapshot_and_legacy_view_hydration() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;

    let attached_id = queued(&app, &sid).await;
    let mut attached = app.run(&attached_id).await.unwrap();
    attached["code_index_retrieval_attempted"] = json!(true);
    attached["code_index_retrieval_status"] = json!("attached");
    attached["code_index_retrieval_stage"] = json!("complete");
    attached["code_index_query"] = json!("private retrieval query");
    attached["code_index_source"] = json!("private source excerpt");
    app.store
        .save_run(&mut attached, "model_dispatch", &json!({}))
        .await
        .unwrap();
    attached["status"] = json!("failed");
    attached["error"] = json!("fixture terminal");
    app.store
        .save_run(&mut attached, "run_status", &json!({}))
        .await
        .unwrap();
    let attached_view = app
        .store
        .get(&format!("run_view:{attached_id}"))
        .await
        .unwrap()
        .unwrap();
    assert!(attached_view["model"].is_object());
    assert_eq!(attached_view["code_index_retrieval_attempted"], true);
    assert_eq!(attached_view["code_index_retrieval_status"], "attached");
    assert_eq!(attached_view["code_index_retrieval_stage"], "complete");

    let legacy_id = queued(&app, &sid).await;
    let mut legacy_run = app.run(&legacy_id).await.unwrap();
    legacy_run["code_index_retrieval_attempted"] = json!(true);
    legacy_run["code_index_retrieval_status"] = json!("attached_candidates");
    legacy_run["code_index_retrieval_stage"] = json!("complete");
    legacy_run["code_index_query"] = json!("second private query");
    legacy_run["code_index_source"] = json!("second private source");
    app.store
        .save_run(&mut legacy_run, "model_dispatch", &json!({}))
        .await
        .unwrap();
    legacy_run["status"] = json!("failed");
    legacy_run["error"] = json!("fixture terminal");
    app.store
        .save_run(&mut legacy_run, "run_status", &json!({}))
        .await
        .unwrap();
    let mut legacy_view = app
        .store
        .get(&format!("run_view:{legacy_id}"))
        .await
        .unwrap()
        .unwrap();
    assert!(legacy_view["model"].is_object());
    for key in [
        "code_index_retrieval_attempted",
        "code_index_retrieval_status",
        "code_index_retrieval_stage",
    ] {
        legacy_view.as_object_mut().unwrap().remove(key);
    }
    app.store
        .put(&format!("run_view:{legacy_id}"), &legacy_view)
        .await
        .unwrap();

    let preflight_id = queued(&app, &sid).await;
    let preflight_view = app
        .store
        .get(&format!("run_view:{preflight_id}"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(preflight_view["code_index_retrieval_attempted"], false);
    assert_eq!(
        preflight_view["code_index_retrieval_status"],
        "not_attempted"
    );
    assert_eq!(preflight_view["code_index_retrieval_stage"], "not_started");

    let token = uuid();
    app.store
        .put(
            &format!("auth:{}", digest(&json!(token))),
            &json!({"csrf":"fixture","expires":now()+3600,"generation":"fixture-generation"}),
        )
        .await
        .unwrap();
    let request = Request::builder()
        .uri(format!("/api/v3/sessions/{sid}"))
        .header("cookie", format!("v3_session={token}"))
        .body(Body::empty())
        .unwrap();
    let response = router(app.clone()).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let snapshot: Value = serde_json::from_slice(&bytes).unwrap();
    let runs = snapshot["runs"].as_array().unwrap();
    let attached_public = runs.iter().find(|run| run["id"] == attached_id).unwrap();
    let legacy_public = runs.iter().find(|run| run["id"] == legacy_id).unwrap();
    let preflight_public = runs.iter().find(|run| run["id"] == preflight_id).unwrap();
    assert_eq!(
        attached_public["code_index_retrieval"],
        json!({"status":"attached","stage":"complete"})
    );
    assert_eq!(
        legacy_public["code_index_retrieval"],
        json!({"status":"attached_candidates","stage":"complete"})
    );
    assert_eq!(
        preflight_public["code_index_retrieval"],
        json!({"status":"not_attempted","stage":"not_started"})
    );
    let rendered = snapshot.to_string();
    for secret in [
        "private retrieval query",
        "private source excerpt",
        "second private query",
        "second private source",
    ] {
        assert!(!rendered.contains(secret));
    }
    let unchanged_legacy = app
        .store
        .get(&format!("run_view:{legacy_id}"))
        .await
        .unwrap()
        .unwrap();
    assert!(unchanged_legacy
        .get("code_index_retrieval_attempted")
        .is_none());
}

#[tokio::test]
async fn provider_recovery_exhaustion_persists_and_explicit_resume_is_idempotent() {
    let (root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    let error = provider::typed_failure(
        model_selection::FailureKind::Transient,
        false,
        "Provider connection failed",
    );
    model_failure(&mut run, &error).unwrap();
    assert_eq!(run["status"], "queued");
    assert_eq!(run["failover_state"]["retries"], 1);
    app.save(&mut run).await.unwrap();
    assert!(!app
        .store
        .claim_run(&mut run, "other-loop", "too-early")
        .await
        .unwrap());
    model_failure(&mut run, &error).unwrap();
    assert_eq!(run["status"], "stopped");
    assert_eq!(run["provider_recovery"]["safe_to_resume"], true);
    app.save(&mut run).await.unwrap();
    let mut reopened = app.clone();
    reopened.store = Store::open(root.path()).await.unwrap();
    let restored = reopened.run(&id).await.unwrap();
    assert_eq!(restored["provider_recovery"], run["provider_recovery"]);
    recover(&reopened, true).await.unwrap();
    assert_eq!(reopened.run(&id).await.unwrap()["status"], "stopped");
    let body = json!({"client_request_id":"provider-resume"});
    command(
        State(reopened.clone()),
        Path((id.clone(), "resume".into())),
        Json(body.clone()),
    )
    .await
    .unwrap();
    let mut resumed = reopened.run(&id).await.unwrap();
    assert_eq!(resumed["failover_state"]["retries"], 0);
    assert_eq!(resumed["selection_plan"], run["selection_plan"]);
    model_failure(&mut resumed, &error).unwrap();
    reopened.save(&mut resumed).await.unwrap();
    command(
        State(reopened.clone()),
        Path((id.clone(), "resume".into())),
        Json(body),
    )
    .await
    .unwrap();
    assert_eq!(
        reopened.run(&id).await.unwrap()["failover_state"]["retries"],
        1
    );
    control(
        State(reopened.clone()),
        Json(json!({"client_request_id":uuid(),"shutoff":true})),
    )
    .await
    .unwrap();
    assert!(command(
        State(reopened.clone()),
        Path((id, "resume".into())),
        Json(json!({"client_request_id":uuid()}))
    )
    .await
    .is_err());
}

#[tokio::test]
async fn provider_recovery_never_marks_partial_or_uncertain_or_contract_failures_safe() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let baseline = app.run(&id).await.unwrap();
    for kind in [
        model_selection::FailureKind::MalformedRequest,
        model_selection::FailureKind::Cancelled,
        model_selection::FailureKind::PolicyRejected,
        model_selection::FailureKind::Protocol,
    ] {
        let mut run = baseline.clone();
        model_failure(
            &mut run,
            &provider::typed_failure(kind, false, "fixture failure"),
        )
        .unwrap();
        assert_eq!(run["status"], "failed");
        assert!(run["provider_recovery"].is_null());
    }
    for (started, uncertain) in [(true, false), (false, true)] {
        let mut run = baseline.clone();
        run["uncertain"] = json!(uncertain);
        model_failure(
            &mut run,
            &provider::typed_failure(
                model_selection::FailureKind::Transient,
                started,
                "fixture failure",
            ),
        )
        .unwrap();
        assert_eq!(run["status"], "failed");
        assert!(run["provider_recovery"].is_null());
    }
    let mut run = baseline.clone();
    let chat_state = run["failover_state"].clone();
    let error = provider::typed_failure(
        model_selection::FailureKind::Transient,
        false,
        "summary connection failure",
    );
    system_model_failure(&mut run, &error).unwrap();
    system_model_failure(&mut run, &error).unwrap();
    assert_eq!(run["status"], "stopped");
    assert_eq!(run["provider_recovery"]["system"], true);
    app.save(&mut run).await.unwrap();
    command(
        State(app.clone()),
        Path((id.clone(), "resume".into())),
        Json(json!({"client_request_id":uuid()})),
    )
    .await
    .unwrap();
    let resumed = app.run(&id).await.unwrap();
    assert_eq!(resumed["system_failover_state"]["retries"], 0);
    assert_eq!(resumed["failover_state"], chat_state);
}

#[tokio::test]
async fn provider_recovery_retries_stream_failure_and_publishes_one_final_reply() {
    use axum::{routing::post, Router};
    use std::sync::atomic::AtomicUsize;
    let (_root, mut app) = fixture().await;
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let mock = Router::new().route("/responses", post(move || {
        let seen = seen.clone();
        async move {
            let event = if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                json!({"type":"response.failed","response":{"status":"failed","error":{"code":"upstream_connection_failed","message":"private provider details"}}})
            } else {
                json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Recovered reply"}]}]}})
            };
            ([("content-type","text/event-stream")], format!("data: {event}\n\n"))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    running(&app, &id, "first-attempt").await;
    execute_run(
        app.clone(),
        id.clone(),
        "first-attempt".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let retry = app.run(&id).await.unwrap();
    assert_eq!(retry["status"], "queued");
    assert_eq!(retry["failover_state"]["retries"], 1);
    assert!(!retry["error"]
        .as_str()
        .unwrap()
        .contains("private provider details"));
    // execute_run's existing bounded delay has elapsed; the durable claim still
    // enforces cooldown independently of that process-local sleep.
    running(&app, &id, "second-attempt").await;
    execute_run(
        app.clone(),
        id.clone(),
        "second-attempt".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(app.run(&id).await.unwrap()["status"], "completed");
    assert_eq!(count.load(Ordering::SeqCst), 2);
    let (chat, _) = app.store.chat_snapshot(&sid).await.unwrap();
    assert_eq!(
        chat.iter().filter(|row| row["role"] == "assistant").count(),
        1
    );
    server.abort();
}

#[test]
fn consolidated_codebase_schema_and_guidance_contract() {
    let catalog = native_catalog();
    let schema = tools::schemas();
    for name in [
        "file_read",
        "file_list",
        "file_search",
        "file_edit",
        "file_write",
    ] {
        let tool = schema.iter().find(|s| s["name"] == name).unwrap();
        assert_eq!(
            tool["parameters"]["properties"]["task_id"]["type"],
            json!(["string", "null"])
        );
        for (key, property) in tool["parameters"]["properties"].as_object().unwrap() {
            if !tool["parameters"]["required"]
                .as_array()
                .unwrap()
                .contains(&json!(key))
            {
                assert!(
                    property["type"]
                        .as_array()
                        .unwrap()
                        .contains(&json!("null")),
                    "{name}.{key} must allow omission under strict normalization"
                );
            }
        }
        assert!(tools::administrator::owns(name));
    }
    assert!(schema.iter().any(|s| s["name"] == "build_work"));
    let visibility = json!({"visibility":{"codebase_admin":"visible"}});
    let state = json!({"active":["codebase_admin"]});
    assert!(
        catalog
            .tool_gate("build_work", &visibility, &state, &json!([]))
            .1
    );
    for name in [
        "v3_source_apply",
        "v3_workspace_action",
        "workspace_seed",
        "source_patch",
    ] {
        assert!(!catalog.tool_gate(name, &visibility, &state, &json!([])).0);
    }
    assert!(provider::DEFAULT_INSTRUCTIONS.contains("without task_id"));
    assert!(provider::DEFAULT_INSTRUCTIONS.contains("automatically create"));
}

#[tokio::test]
async fn consolidated_build_finish_waits_for_receipt_without_replaying_write() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    running(&app, &id, "build-generation").await;
    let mut run = app.run(&id).await.unwrap();
    run["phase"] = json!("tools");
    run["pending"] = json!({"name":"build_work","state":"executing",
        "call_id":"original-build-call","arguments":{"action":"finish",
            "task_id":"build-recovery-test","expected_inputs_sha256":"0".repeat(64)}});
    app.save(&mut run).await.unwrap();
    recover(&app, true).await.unwrap();
    let waiting = app.run(&id).await.unwrap();
    assert_eq!(waiting["status"], "waiting_external");
    assert_eq!(waiting["uncertain"], false);
    assert_eq!(waiting["pending"]["call_id"], "original-build-call");
    assert_eq!(waiting["pending"]["external_job"]["kind"], "v3_build");
    recover(&app, false).await.unwrap();
    let repeated = app.run(&id).await.unwrap();
    assert_eq!(repeated["status"], "waiting_external");
    assert_eq!(repeated["pending"]["call_id"], "original-build-call");
    assert_eq!(repeated["call_index"], waiting["call_index"]);
    let mut stopped = repeated.clone();
    stopped["status"] = json!("stopped");
    app.save_override(&mut stopped).await.unwrap();
    let _ = command(
        State(app.clone()),
        Path((id.clone(), "resume".into())),
        Json(json!({"client_request_id":"resume-build-result"})),
    )
    .await
    .unwrap();
    let resumed = app.run(&id).await.unwrap();
    assert_eq!(resumed["status"], "waiting_external");
    assert_eq!(resumed["pending"]["state"], "executing");
    let result = json!({"status":"success","output":{"task_id":"build-recovery-test",
        "job_id":"existing-job","status":"completed","verified_healthy":true}});
    let mut settled = resumed.clone();
    agent_loop::attach_external_build_result(&mut settled, &result).unwrap();
    assert_eq!(settled["status"], "queued");
    assert!(agent_loop::attach_external_build_result(&mut settled, &result).is_err());
    app.store
        .save_run_override(&mut settled, "tool_result", &result)
        .await
        .unwrap();
    let durable = app.run(&id).await.unwrap();
    assert_eq!(durable["call_index"], json!(1));
    assert_eq!(
        durable["input"].as_array().unwrap().last().unwrap()["call_id"],
        "original-build-call"
    );
    assert_eq!(durable["pending"], Value::Null);
}

// Native catalog journeys use isolated storage and exact dispatch, without a
// Decision model or external worker; grants remain independent of discovery.
async fn visibility_fixture_base_access(app: &App) {
    visibility_fixture_ability_access(app, "base").await;
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["visibility"]["codebase_admin"] = json!("visible");
    app.store
        .save_agent_abilities(&visibility, revision)
        .await
        .unwrap();
}
/// Grant one ability to the admin on shared_default and make it and its skills visible.
async fn visibility_fixture_ability_access(app: &App, id: &str) {
    let mut tier = app
        .store
        .entitlement_tier("operator")
        .await
        .unwrap()
        .unwrap();
    let revision = tier.revision;
    tier.ability_permissions
        .insert(id.into(), access::Permission::Auto);
    app.store
        .save_entitlement_tier(tier, revision)
        .await
        .unwrap();
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
    visibility["visibility"][id] = json!("visible");
    visibility["skill_visibility"][id] = json!("visible");
    app.store
        .save_agent_abilities(&visibility, revision)
        .await
        .unwrap();
}
async fn visibility_native_call(app: &App, sid: &str, id: &str, name: &str, args: Value) -> Value {
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
    let result = if tools::core_skills::owns(name) {
        tools::core_skills::execute(app, sid, id, name, &args, CancellationToken::new())
            .await
            .unwrap()
    } else {
        let call = tools::native::Call {
            sid,
            run_id: id,
            generation: "",
            pending_id: "",
        };
        tools::native::tool_search::execute(
            &tools::native_host::Host(app),
            call,
            name,
            &args,
            CancellationToken::new(),
        )
        .await
        .unwrap()
    };
    let mut run = app.run(id).await.unwrap();
    append_tool(&mut run, &result).unwrap();
    app.save(&mut run).await.unwrap();
    result
}
#[tokio::test]
/// Verify native skill discovery identifies supplied guidance and selected skill text refreshes after simulated compaction and edits.
async fn native_skill_catalog_marks_supplied_guidance_and_rebuilds_selected_after_compaction() {
    let (_root, app) = fixture().await;
    visibility_fixture_base_access(&app).await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    running(&app, &id, "skill-visibility").await;
    let full = visibility_native_call(
        &app,
        &sid,
        &id,
        "list_skills",
        json!({"ability_id":"base","include_available":true}),
    )
    .await;
    assert_eq!(full["status"], "success", "{full}");
    let core = full["output"]["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["kind"] == "ability")
        .unwrap();
    assert_eq!(core["availability"], "visible");
    let core_id = core["skill_id"].clone();
    let default =
        visibility_native_call(&app, &sid, &id, "list_skills", json!({"ability_id":"base"})).await;
    assert!(default["output"]["skills"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["requires_activation"] == true && row["availability"] == "discoverable"));
    let ack = visibility_native_call(
        &app,
        &sid,
        &id,
        "load_skill",
        json!({
        "ability_id":"base","kind":"ability","skill_id":core_id}),
    )
    .await;
    assert_eq!(ack["output"]["status"], "already_available", "{ack}");
    assert!(ack["output"].get("text").is_none());
    assert!(app.run(&id).await.unwrap()["loaded_skills"]
        .as_array()
        .is_none_or(|items| items.is_empty()));
    let sentinel = "DURABLE_SELECTED_SKILL_GUIDANCE";
    app.store
        .mutate_own_skill(
            "journey_steps",
            Some("when testing visibility"),
            Some(sentinel),
            Some("selectable"),
            0,
            false,
            "visibility-create",
            &id,
        )
        .await
        .unwrap();
    let result = visibility_native_call(
        &app,
        &sid,
        &id,
        "load_skill",
        json!({
        "ability_id":"base","kind":"own","skill_id":"journey_steps"}),
    )
    .await;
    assert_eq!(result["status"], "success", "{result}");
    assert_eq!(result["output"]["status"], "loaded");
    assert!(result["output"].get("text").is_none());
    assert!(!result.to_string().contains(sentinel));
    let stored = app.run(&id).await.unwrap();
    assert!(!stored["input"].to_string().contains(sentinel));
    assert!(tools::core_skills::active_guidance(&app, &sid, &id)
        .await
        .unwrap()
        .contains(sentinel));
    let mut run = app.run(&id).await.unwrap();
    assert_eq!(run["loaded_skills"].as_array().unwrap().len(), 1);
    assert!(!run["loaded_skills"].to_string().contains(sentinel));
    // Simulate replacement of tool history with compacted history; selector
    // authority persists independently and rehydrates the complete fresh body.
    run["input"] = json!([{"role":"assistant","content":"Compacted conversation."}]);
    app.save(&mut run).await.unwrap();
    assert!(tools::core_skills::active_guidance(&app, &sid, &id)
        .await
        .unwrap()
        .contains(sentinel));
    assert_eq!(
        session_ability_state(&app, &sid).await.unwrap()["active"],
        json!([])
    );
    let default =
        visibility_native_call(&app, &sid, &id, "list_skills", json!({"ability_id":"base"})).await;
    assert!(!default["output"]["skills"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["skill_id"] == "journey_steps"));
    let full = visibility_native_call(
        &app,
        &sid,
        &id,
        "list_skills",
        json!({
        "ability_id":"base","include_available":true}),
    )
    .await;
    assert_eq!(
        full["output"]["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["skill_id"] == "journey_steps")
            .unwrap()["availability"],
        "activated"
    );
    let ack = visibility_native_call(
        &app,
        &sid,
        &id,
        "load_skill",
        json!({
        "ability_id":"base","kind":"own","skill_id":"journey_steps"}),
    )
    .await;
    assert_eq!(ack["output"]["status"], "already_available");
    assert!(ack["output"].get("text").is_none());
    app.store
        .mutate_own_skill(
            "journey_steps",
            None,
            Some("REPLACEMENT"),
            None,
            1,
            false,
            "visibility-edit",
            &id,
        )
        .await
        .unwrap();
    let guidance = tools::core_skills::active_guidance(&app, &sid, &id)
        .await
        .unwrap();
    assert!(!guidance.contains(sentinel) && guidance.contains("REPLACEMENT"));
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["visibility"]["base"] = json!("off");
    app.store
        .save_agent_abilities(&visibility, revision)
        .await
        .unwrap();
    assert!(tools::core_skills::active_guidance(&app, &sid, &id)
        .await
        .unwrap()
        .is_empty());
}
#[tokio::test]
async fn exact_native_tool_selection_obeys_visibility_mode_denial_and_suppression() {
    let (_root, app) = fixture().await;
    visibility_fixture_base_access(&app).await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    running(&app, &id, "tool-visibility").await;
    app.store
        .put(
            &key("session_mode", &sid),
            &json!({"mode":"read_only","revision":1}),
        )
        .await
        .unwrap();
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["tool_modes"]["state_inspect"] = json!("discoverable");
    app.store
        .save_agent_abilities(&visibility, revision)
        .await
        .unwrap();
    let listed = visibility_native_call(&app, &sid, &id, "list_tools", json!({})).await;
    assert_eq!(listed["status"], "success", "{listed}");
    assert!(listed["output"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["name"] == "state_inspect" && row["availability"] == "discoverable"));
    let loaded = visibility_native_call(
        &app,
        &sid,
        &id,
        "load_tool",
        json!({"tool_name":"state_inspect"}),
    )
    .await;
    assert_eq!(loaded["status"], "success", "{loaded}");
    assert_eq!(loaded["output"]["activated"], true);
    assert_eq!(loaded["output"]["tool_name"], "state_inspect");
    assert!(loaded["output"].get("tool").is_none());
    assert!(loaded["output"].get("description").is_none());
    let run = app.run(&id).await.unwrap();
    assert!(run["loaded_abilities"]
        .as_array()
        .unwrap()
        .contains(&json!("tool:state_inspect")));
    assert_eq!(
        session_ability_state(&app, &sid).await.unwrap()["active"],
        json!([])
    );
    let ack = visibility_native_call(
        &app,
        &sid,
        &id,
        "load_tool",
        json!({"tool_name":"state_inspect"}),
    )
    .await;
    assert_eq!(ack["status"], "already_available", "{ack}");
    let listed = visibility_native_call(&app, &sid, &id, "list_tools", json!({})).await;
    assert!(!listed["output"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["name"] == "state_inspect"));
    let full = visibility_native_call(
        &app,
        &sid,
        &id,
        "list_tools",
        json!({"include_available":true}),
    )
    .await;
    assert_eq!(
        full["output"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == "state_inspect")
            .unwrap()["availability"],
        "activated"
    );
    let visible = visibility_native_call(
        &app,
        &sid,
        &id,
        "load_tool",
        json!({"tool_name":"file_read"}),
    )
    .await;
    assert_eq!(visible["status"], "already_available", "{visible}");
    assert!(!app.run(&id).await.unwrap()["loaded_abilities"]
        .as_array()
        .unwrap()
        .contains(&json!("tool:file_read")));
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["tool_modes"]["state_inspect"] = json!("off");
    app.store
        .save_agent_abilities(&visibility, revision)
        .await
        .unwrap();
    let off = visibility_native_call(
        &app,
        &sid,
        &id,
        "load_tool",
        json!({"tool_name":"state_inspect"}),
    )
    .await;
    assert_eq!(off["status"], "denied", "{off}");
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["tool_modes"]["state_inspect"] = json!("discoverable");
    app.store
        .save_agent_abilities(&visibility, revision)
        .await
        .unwrap();
    let mut policy = app.store.get("policy").await.unwrap().unwrap();
    policy["rules"]["state_inspect"] = json!("deny");
    app.store.put("policy", &policy).await.unwrap();
    let denied = visibility_native_call(
        &app,
        &sid,
        &id,
        "load_tool",
        json!({"tool_name":"state_inspect"}),
    )
    .await;
    assert_eq!(denied["status"], "denied", "{denied}");
    policy["rules"]["state_inspect"] = json!("allow");
    app.store.put("policy", &policy).await.unwrap();
    app.store
        .put(
            &key("session_ability_state", &sid),
            &json!({
        "revision":1,"active":[],"suppressed":["codebase_admin"]}),
        )
        .await
        .unwrap();
    let suppressed = visibility_native_call(
        &app,
        &sid,
        &id,
        "load_tool",
        json!({"tool_name":"state_inspect"}),
    )
    .await;
    assert_eq!(suppressed["status"], "denied", "{suppressed}");
}

#[tokio::test]
async fn workspace_host_uses_scoped_host_socket_and_rejects_missing_envelope() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    let (_root, app) = fixture().await;
    let listener = UnixListener::bind(&app.build_socket).unwrap();
    let sid = conversation(&app).await;
    let expected = sid.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let call: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(call["workspace"], expected);
        assert_eq!(call["trusted"]["session_id"], expected);
        assert!(call["trusted"]["lease_until"].as_u64().unwrap() > now());
        assert_eq!(call["name"], "source_review");
        let reply = json!({"id":call["id"],"status":"success","diff":"unwrapped"});
        let reply = test_rpc_reply(&call, reply);
        reader
            .get_mut()
            .write_all(format!("{reply}\n").as_bytes())
            .await
            .unwrap();
    });
    let result = tools::administrator::workspace_host(
        &app,
        &sid,
        "source_review",
        &json!({}),
        None,
        CancellationToken::new(),
    )
    .await;
    assert!(result.is_err());
    assert!(!app.tool_socket.exists());
    server.await.unwrap();
}

/// API-only replacement must restore persisted enablement without clearing an intentional global stop.
#[tokio::test]
async fn worker_control_restores_saved_state_after_launcher_reset() {
    let (_root, app) = fixture().await;
    for shutoff in [false, true] {
        app.store
            .put("control", &json!({"shutoff":shutoff}))
            .await
            .unwrap();
        std::fs::write(app.control_dir.join("shutoff"), b"1").unwrap();
        restore_worker_control(&app).await.unwrap();
        assert_eq!(
            std::fs::read(app.control_dir.join("shutoff")).unwrap(),
            if shutoff { b"1" } else { b"0" }
        );
    }
}

/// Exercise normal encrypted run-view storage before bounded public pause projection.
/// Modern views omit private markers; all registered component kinds and manual pauses stay distinct.
#[tokio::test]
async fn background_component_pause_survives_modern_persisted_view() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    for component in engine_components::WORKING_COMPONENTS
        .into_iter()
        .map(Some)
        .chain([None])
    {
        let mut run = app.run(&id).await.unwrap();
        run["status"] = json!("paused");
        run["component_pause"] = component
            .map(|id| json!({"version":1,"component":id,"private_field":"must-not-project"}))
            .unwrap_or(Value::Null);
        run["code_index_retrieval_attempted"] = json!(false);
        run["code_index_retrieval_status"] = json!("not_attempted");
        run["code_index_retrieval_stage"] = json!("not_started");
        app.save(&mut run).await.unwrap();
        let views = app.store.recent_runs(Some(&sid)).await.unwrap();
        assert_eq!(views.len(), 1);
        assert!(views[0]["model"].is_object());
        assert!(views[0]["code_index_retrieval_attempted"].is_boolean());
        assert!(views[0]["code_index_retrieval_status"].is_string());
        assert!(views[0]["code_index_retrieval_stage"].is_string());
        assert!(
            views[0]["component_pause"].is_null(),
            "fixture must traverse a modern stored view without its private marker"
        );
        let projected = public_runs(&app, views).await.unwrap();
        assert_eq!(projected.len(), 1);
        let expected = component
            .map(|id| json!({"version":1,"component":id,"reason":"component_unavailable"}))
            .unwrap_or(Value::Null);
        assert_eq!(projected[0]["component_pause"], expected);
        assert_eq!(projected[0]["status"], "paused");
    }
}

/// Synthetic peers distinguish the protected host's existing protocol from worker protocol 2.
fn test_rpc_reply(request: &Value, mut reply: Value) -> Value {
    if request["version"] == 2 {
        let id = reply.as_object_mut().unwrap().remove("id").unwrap();
        json!({"version":2,"id":id,"result":reply})
    } else {
        reply
    }
}

/// Acknowledge only normal resource-shutdown controls while the effect peer is held.
async fn late_single_control_reply(stream: tokio::net::UnixStream) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let request: Value = serde_json::from_str(&line).unwrap();
    assert!(matches!(
        request["name"].as_str(),
        Some("administrator_quiesce" | "administrator_host_revoke")
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
async fn late_single_tool_case(name: &str, action: &str, mutation: &str, success: bool) {
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
        let mut policy = policy_default();
        for tool in ["build_work", "v3_build_work", "v3_worker_submit"] {
            policy["rules"][tool] = json!("allow");
        }
        app.store.put("policy", &policy).await.unwrap();
        // The protected host retains the legacy build-ability ceilings.
        let mut tier = app
            .store
            .entitlement_tier("operator")
            .await
            .unwrap()
            .unwrap();
        let revision = tier.revision;
        tier.ability_permissions
            .insert("v3_build".into(), access::Permission::Auto);
        app.store
            .save_entitlement_tier(tier, revision)
            .await
            .unwrap();
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
            .insert("v3_build".into(), access::Permission::Auto);
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
    let quiesce = if name == "build_work" {
        let listener = tokio::net::UnixListener::bind(&app.tool_socket).unwrap();
        Some(tokio::spawn(async move {
            late_single_control_reply(listener.accept().await.unwrap().0).await;
        }))
    } else {
        None
    };
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
    let request = tokio::time::timeout(Duration::from_secs(10), received)
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
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    worker.await.unwrap();
    if let Some(quiesce) = quiesce {
        quiesce.await.unwrap();
    }
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
        assert_eq!(
            after["input"]
                .as_array()
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

#[tokio::test]
async fn late_single_tool_failure_survives_stop_pause_and_reopen() {
    for name in ["file_list", "file_write"] {
        for action in ["cancel", "pause"] {
            late_single_tool_case(name, action, "none", false).await;
        }
    }
}

#[tokio::test]
async fn late_single_tool_rejects_stale_attempts_and_keeps_known_write_result() {
    for mutation in ["generation", "pending", "requested"] {
        late_single_tool_case("file_write", "pause", mutation, false).await;
        late_single_tool_case("file_write", "pause", mutation, true).await;
    }
    late_single_tool_case("file_write", "cancel", "none", true).await;
}

#[tokio::test]
async fn late_single_tool_broker_finish_keeps_receipt_reconciliation() {
    for action in ["cancel", "pause"] {
        late_single_tool_case("build_work", action, "none", false).await;
    }
}

/// Terminal storage accepts a safe diagnostic attachment, never a state/result edit.
#[tokio::test]
async fn late_single_tool_terminal_storage_keeps_control_fences() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    running(&app, &id, "terminal-diagnostic").await;
    let mut run = app.run(&id).await.unwrap();
    run["pending"] = json!({"id":"terminal-attempt","call_id":"terminal-call","name":"file_list","state":"executing"});
    app.save(&mut run).await.unwrap();
    let _ = command(
        State(app.clone()),
        Path((id.clone(), "cancel".into())),
        Json(json!({"client_request_id":"terminal-diagnostic-control"})),
    )
    .await
    .unwrap();
    let previous = app.run(&id).await.unwrap();
    let mut next = previous.clone();
    let error = tools::failure::Failure::new(
        tools::failure::Stage::ResponseRead,
        tools::failure::Code::DisconnectedResponse,
        tools::failure::Delivery::PossiblySent,
    )
    .into();
    tools::failure::record(&mut next, &error, None);
    for (pointer, value) in [
        ("/status", json!("running")),
        ("/error", json!("changed")),
        ("/uncertain", json!(true)),
        ("/generation", json!("replacement")),
        ("/lease_epoch", json!(999)),
        ("/run_revision", json!(999)),
        ("/pending/state", json!("requested")),
        ("/pending/id", json!("replacement")),
        (
            "/input",
            json!([{"type":"function_call_output","call_id":"terminal-call","output":"invented"}]),
        ),
    ] {
        let mut invalid = next.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        assert!(
            app.store
                .save_run(&mut invalid, "run_status", &json!({}))
                .await
                .is_err(),
            "{pointer}"
        );
        assert_eq!(app.run(&id).await.unwrap(), previous);
    }
    let mut wrong_event = next.clone();
    assert!(app
        .store
        .save_run(&mut wrong_event, "tool_result", &json!({}))
        .await
        .is_err());
    app.store
        .save_run_observed(&mut next, "run_status", &json!({"run_id":id}))
        .await
        .unwrap();
    assert_eq!(
        app.run(&id).await.unwrap()["latest_tool_failure"],
        next["latest_tool_failure"]
    );
}

#[tokio::test]
async fn cancelling_a_finished_run_is_a_conflict_not_a_storage_failure() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    run["status"] = json!("completed");
    app.save(&mut run).await.unwrap();
    for action in ["cancel", "pause"] {
        let error = command(
            State(app.clone()),
            Path((id.clone(), action.into())),
            Json(json!({"client_request_id":uuid()})),
        )
        .await
        .unwrap_err();
        assert_eq!(error.0, StatusCode::CONFLICT, "{action}");
    }
    assert_eq!(app.run(&id).await.unwrap()["status"], "completed");
}
