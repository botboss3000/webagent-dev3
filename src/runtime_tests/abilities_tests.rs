//! Runtime acceptance tests: Ability catalog, tool schemas, skills, prompt slots and their revocation.
use super::*;

/// The core catalog without the optional Administrator pack, as a non-coding installation
/// loads it: it validates, and no core ability offers checkout or Administrator tools.
#[test]
fn core_catalog_loads_without_the_administrator_pack() {
    let target = tempfile::tempdir().unwrap();
    copy_native(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("abilities"),
        target.path(),
        &["Administrator"],
    );
    let catalog = abilities::Catalog::load(target.path()).expect("core catalog without the pack");
    for id in [
        "codebase_admin",
        "diagnostics",
        "git_control",
        "terminal_control",
    ] {
        assert!(!catalog.known(id), "{id} needs the Administrator pack");
    }
    #[cfg(feature = "administrator")]
    let pack_tools = webagent_administration::tools::TOOLS
        .iter()
        .chain(&webagent_administration::tools::CHECKOUT_TOOLS);
    #[cfg(not(feature = "administrator"))]
    let pack_tools = [
        "file_list",
        "file_read",
        "file_search",
        "file_edit",
        "file_write",
        "checkout_edit",
    ]
    .iter();
    for name in pack_tools {
        for id in catalog.ids() {
            assert!(
                !catalog.has_tool(&id, name),
                "core ability {id} offers Administrator tool {name}"
            );
        }
    }
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
            visibility: "loaded".into(),
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
    assert!(!app.abilities.tool_available(
        "file_read",
        &result,
        &state,
        &json!(["codebase_admin"])
    ));
    assert!(!app
        .abilities
        .schemas_with(&json!(["codebase_admin"]), &result, &state)
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
async fn on_request_tool_is_sent_without_selection() {
    let (_root, app) = fixture().await;
    let result = agent_tool_mode_set(
        State(app.clone()),
        Path((
            "shared_default".into(),
            "codebase_admin".into(),
            "file_read".into(),
        )),
        Json(AbilityVisibilityBody {
            visibility: "on_request".into(),
            expected_revision: 0,
        }),
    )
    .await
    .unwrap()
    .0;
    let state = json!({"active":[],"suppressed":[]});
    assert!(app
        .abilities
        .tool_available("file_read", &result, &state, &json!([])));
    // Suppression still withholds it.
    let suppressed = json!({"active":[],"suppressed":["codebase_admin"]});
    assert!(!app
        .abilities
        .tool_available("file_read", &result, &suppressed, &json!([])));
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
    // Loading no longer gates a tool; switching it Off while the call waits does.
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["tool_modes"]["shell"] = json!("off");
    app.store
        .save_agent_abilities(&visibility, revision)
        .await
        .unwrap();
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
    // A finished run keeps no input copy; the denied result lives in the session history.
    let history = app.store.context(&sid).await.unwrap();
    let output = history
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    assert!(output["output"].as_str().unwrap().contains("denied"));
    server.abort();
}

#[tokio::test]
async fn inference_warm_up_embeds_the_catalog_once_and_tolerates_failure() {
    let (_root, app) = fixture().await;
    crate::api::inference::RANK_STUB.with(|cell| cell.set(Some(warm_stub)));
    let warmed = tools::core_skills::warm_inference(&app).await;
    let sent = WARM_ITEMS.load(std::sync::atomic::Ordering::SeqCst);
    assert!(sent > 0, "warm-up sent no texts");
    assert_eq!(warmed, Some(sent));
    crate::api::inference::RANK_STUB.with(|cell| cell.set(Some(warm_failure)));
    assert_eq!(tools::core_skills::warm_inference(&app).await, None);
    crate::api::inference::RANK_STUB.with(|cell| cell.set(None));
}

#[tokio::test]
/// Every enabled tool's schema is sent from the first request, and load_skill by tool name
/// returns that tool's skill text as its result.
async fn enabled_tools_are_sent_and_load_skill_returns_tool_skill_text() {
    use axum::routing::post;

    let (_root, mut app) = fixture().await;
    let sid = conversation(&app).await;
    let sent = send(
        State(app.clone()),
        Path(sid.clone()),
        Json(json!({"client_message_id":uuid(),"text":"read a file"})),
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
                    1 => json!([{"type":"function_call","call_id":"skill",
                        "name":"load_skill","arguments":"{\"tool_name\":\"file_read\"}"}]),
                    2 => json!([{"type":"message","role":"assistant",
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
    running(&app, &id, "tool-skill").await;
    execute_fixture_journey(
        app.clone(),
        id.clone(),
        "tool-skill".into(),
        CancellationToken::new(),
    )
    .await;

    assert_eq!(app.run(&id).await.unwrap()["status"], "completed");
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 2);
    let names = request_tools(&requests[0]);
    for tool in ["list_skills", "load_skill", "file_read", "file_search"] {
        assert!(names.contains(&tool.to_owned()), "{tool}: {names:?}");
    }
    for removed in ["list_tools", "load_tool"] {
        assert!(!names.contains(&removed.to_owned()), "{names:?}");
    }
    let item = requests[1]["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output" && item["call_id"] == "skill")
        .unwrap();
    let loaded: Value = serde_json::from_str(item["output"].as_str().unwrap()).unwrap();
    assert_eq!(loaded["status"], "success", "{loaded}");
    assert_eq!(loaded["output"]["kind"], "tool", "{loaded}");
    assert_eq!(loaded["output"]["skill_id"], "file_read", "{loaded}");
    let text = loaded["output"]["text"].as_str().unwrap();
    assert!(!text.is_empty());
    // The text is the tool result; it is not repeated in the next call's instructions.
    assert!(!requests[1]["instructions"]
        .as_str()
        .unwrap_or("")
        .contains(text));
    server.abort();
}

#[tokio::test]
/// Verify an enabled tool and its Visible ability guidance arrive without loading, guidance stays
/// in model-call frames, and state inspection stays scoped to the active session.
async fn enabled_tool_inspects_only_its_current_session() {
    use axum::routing::post;

    let (_root, mut app) = fixture().await;
    // Core skills default to discoverable; this journey checks Visible core guidance.
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["skill_visibility"]["codebase_admin"] = json!("loaded");
    app.store
        .save_agent_abilities(&visibility, revision)
        .await
        .unwrap();
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
                    1 => json!([{"type":"function_call","call_id":"inspect",
                        "name":"state_inspect",
                        "arguments":"{\"resource\":\"chat\",\"limit\":1}"}]),
                    2 => json!([{"type":"function_call","call_id":"cross-session",
                        "name":"state_inspect",
                        "arguments":json!({"resource":"chat","session_id":other_for_call}).to_string()}]),
                    3 => json!([{"type":"message","role":"assistant",
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
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 3);
    let names = request_tools(&requests[0]);
    assert!(names.contains(&"state_inspect".to_owned()), "{names:?}");
    assert!(!names.contains(&"list_tools".to_owned()), "{names:?}");
    let result_for = |request: &Value, call_id: &str| -> Value {
        let item = request["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "function_call_output" && item["call_id"] == call_id)
            .unwrap();
        serde_json::from_str(item["output"].as_str().unwrap()).unwrap()
    };
    let guidance = native_ability_frames(&requests[0], "codebase_admin");
    assert_eq!(guidance.len(), 1);
    assert!(guidance[0].contains("Codebase Admin"));
    // Skills sit in the instructions for every profile.
    assert!(requests[0]["instructions"]
        .as_str()
        .unwrap_or_default()
        .contains("## Untrusted live guidance: ability:codebase_admin\n"));
    assert!(!app.run(&id).await.unwrap()["input"]
        .to_string()
        .contains("## Untrusted live guidance: ability:codebase_admin\n"));
    assert!(!json!(app.store.context(&sid).await.unwrap())
        .to_string()
        .contains("## Untrusted live guidance: ability:codebase_admin\n"));
    let inspected = result_for(&requests[1], "inspect");
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
    // `session_id` is not in the schema, so the argument check refuses it before dispatch.
    let cross = result_for(&requests[2], "cross-session");
    assert_eq!(cross["status"], "rejected", "{cross}");
    assert_eq!(cross["error_kind"], "invalid_arguments", "{cross}");
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
        .unwrap_or_default()
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
    let visible = json!({"visibility":"loaded","expected_revision":0});
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
    assert_eq!(agent["abilities"][0]["visibility"], "on_request");
    // Each row carries its declared default for the viewed level, shown read-only.
    for ability in agent["abilities"].as_array().unwrap() {
        assert!(ability["default"].is_string(), "{}", ability["id"]);
        for tool in ability["tools"].as_array().into_iter().flatten() {
            assert!(tool["runtime_control"] == true || tool["default"].is_string());
        }
    }
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
    assert_eq!(result["visibility"]["codebase_admin"], "loaded");
    let (status, same) = ability_request(
        &app,
        Method::PUT,
        visibility_uri,
        Some(json!({"visibility":"loaded","expected_revision":1})),
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
            Some(json!({"visibility":"loaded","expected_revision":1})),
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
            Some(json!({"visibility":"loaded","expected_revision":1})),
            Some(&cookie),
            Some("ability-fixture")
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    let (_, visibility, state) = ability_controls(&app, &second).await.unwrap();
    // Tools are discoverable by default: exposed once selected.
    assert!(app
        .abilities
        .schemas_with(&json!(["tool:file_read"]), &visibility, &state)
        .iter()
        .any(|tool| tool["name"] == "file_read"));
    assert!(!app
        .abilities
        .visible_guidance(&visibility, &state)
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
    let (_, visibility, first_state) = ability_controls(&app, &first).await.unwrap();
    assert!(!app
        .abilities
        .schemas_with(&json!(["codebase_admin"]), &visibility, &first_state,)
        .iter()
        .any(|tool| tool["name"] == "file_read"));
    // Other On groups may still contribute; the suppressed group never does.
    assert!(!app
        .abilities
        .visible_guidance(&visibility, &first_state)
        .contains("Visible ability codebase_admin"));
    let (_, visibility, second_state) = ability_controls(&app, &second).await.unwrap();
    assert!(app
        .abilities
        .schemas_with(&json!(["tool:file_read"]), &visibility, &second_state,)
        .iter()
        .any(|tool| tool["name"] == "file_read"));

    let (status, changed) = ability_request(
        &app,
        Method::PUT,
        visibility_uri,
        Some(json!({"visibility":"on_request","expected_revision":1})),
        Some(&cookie),
        Some("ability-fixture"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(changed["revision"], 2);
    let (_, visibility, second_state) = ability_controls(&app, &second).await.unwrap();
    // On request still sends every enabled tool's schema.
    assert!(app
        .abilities
        .schemas_with(&json!([]), &visibility, &second_state)
        .iter()
        .any(|tool| tool["name"] == "file_read"));
    // Discoverable now means On: the group's guidance still follows its tools.
    assert!(app
        .abilities
        .visible_guidance(&visibility, &second_state)
        .contains("Visible ability codebase_admin"));
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
    let (_, visibility, second_state) = ability_controls(&app, &second).await.unwrap();
    // Session activation supplies the group's discoverable tools, as a selection does.
    assert!(app
        .abilities
        .schemas_with(&json!([]), &visibility, &second_state)
        .iter()
        .any(|tool| tool["name"] == "file_read"));
    assert!(app
        .abilities
        .schemas_with(&json!(["tool:file_read"]), &visibility, &second_state,)
        .iter()
        .any(|tool| tool["name"] == "file_read"));
    assert!(!app
        .abilities
        .visible_guidance(&visibility, &second_state)
        .is_empty());
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
    let stale = json!({"visibility":"loaded","expected_revision":0});
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
            app.store.full_user_tier().await,
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
    // Unsaved levels hold no overrides: tool.json decides, so nothing is denied for them.
    assert!(view["abilities"]
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a["effective"]["reason"] != "capability_denied"));
    crate::runtime_tests::grant_fixture_levels(&app.store, "agent-a").await;
    // The Abilities page is operator-only.
    assert_eq!(
        call(&app, &member_token, "m", "GET", path, Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let level = json!({"id":"limited","name":"Limited","revision":1,"ability_permissions":{"codebase_admin":"auto","web_access":"auto"},"tool_permissions":{}});
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
        StatusCode::FORBIDDEN
    );
    // Administrator abilities need an administrator caller, whatever the level saves.
    assert_eq!(
        app.store
            .live_access(&user, "shared_default")
            .await
            .unwrap()
            .ability_permission("codebase_admin"),
        access::Permission::Deny
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
/// Verify manifest removal withdraws its schemas and projected guidance during an active run without persisting guidance in history.
async fn removed_manifest_withdraws_cached_schemas_and_guidance_within_run() {
    use axum::routing::post;
    let (root, mut app) = fixture().await;
    // Not named `abilities`, so the catalog has no checkout source root and adopts no live edits.
    let catalog_root = root.path().join("catalog");
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
    std::fs::create_dir_all(directory.join("skills")).unwrap();
    std::fs::write(
        directory.join("skills/codebase_admin.skill.md"),
        "CACHE_WITHDRAWAL_GUIDANCE_SENTINEL",
    )
    .unwrap();
    app.abilities = Arc::new(abilities::Catalog::load(&catalog_root).unwrap());
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["visibility"]["codebase_admin"] = json!("loaded");
    visibility["tool_modes"]["state_inspect"] = json!("loaded");
    visibility["skill_visibility"]["codebase_admin"] = json!("loaded");
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
    // The withdrawn guidance left the instructions on the next call.
    assert!(!requests[1]["instructions"]
        .as_str()
        .unwrap_or_default()
        .contains("CACHE_WITHDRAWAL_GUIDANCE_SENTINEL"));
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
        std::fs::create_dir_all(directory.join("skills")).unwrap();
        std::fs::write(
            directory.join("skills/codebase_admin.skill.md"),
            "DISPATCH_RACE_GUIDANCE",
        )
        .unwrap();
        app.abilities = Arc::new(abilities::Catalog::load(&catalog_root).unwrap());
        let mut visibility = app.store.agent_abilities().await.unwrap();
        let revision = visibility["revision"].as_u64().unwrap();
        visibility["visibility"]["codebase_admin"] = json!("loaded");
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
        tokio::time::timeout(TEST_TIMEOUT, async {
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
        tokio::time::timeout(TEST_TIMEOUT, task)
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
            assert_eq!(requests.len(), 1, "{}", run["pending"]);
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

/// Keep the Tokio test on its ordinary stack; production schedules each run as a task.
#[tokio::test]
async fn normal_dispatch_reloads_live_prompt_slots_and_selected_skill_on_default_stack() {
    Box::pin(live_context_default_stack_journey()).await;
}

#[test]
#[cfg_attr(not(administrator_pack), ignore = "needs the Administrator pack")]
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
        assert!(tools::administrator::owns(name) || tools::administrator::plugin(name).is_some());
    }
    assert!(schema.iter().any(|s| s["name"] == "build_work"));
    let visibility = json!({"visibility":{"codebase_admin":"loaded"}});
    let state = json!({"active":["codebase_admin"]});
    assert!(
        catalog
            .tool_gate("build_work", &visibility, &state, &json!([]))
            .1
    );
    for name in ["workspace_seed", "source_patch"] {
        assert!(!catalog.tool_gate(name, &visibility, &state, &json!([])).0);
    }
    assert!(provider::DEFAULT_INSTRUCTIONS.contains("without task_id"));
    assert!(provider::DEFAULT_INSTRUCTIONS.contains("automatically create"));
}

#[tokio::test]
async fn tool_skill_loads_by_name_and_obeys_mode_denial_and_suppression() {
    let (_root, app) = fixture().await;
    visibility_fixture_base_access(&app).await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    running(&app, &id, "tool-visibility").await;
    app.store
        .put(
            &key("session_mode", &sid),
            &json!({"execution_mode":"ask","revision":1}),
        )
        .await
        .unwrap();
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["tool_modes"]["state_inspect"] = json!("on_request");
    visibility["tool_modes"]["file_read"] = json!("loaded");
    app.store
        .save_agent_abilities(&visibility, revision)
        .await
        .unwrap();
    let load = |name: &'static str| {
        let (app, sid, id) = (app.clone(), sid.clone(), id.clone());
        async move {
            visibility_native_call(&app, &sid, &id, "load_skill", json!({"tool_name":name})).await
        }
    };
    let loaded = load("state_inspect").await;
    assert_eq!(loaded["status"], "success", "{loaded}");
    assert_eq!(loaded["output"]["status"], "loaded", "{loaded}");
    assert_eq!(loaded["output"]["kind"], "tool");
    assert_eq!(loaded["output"]["ability_id"], "codebase_admin");
    assert!(!loaded["output"]["text"].as_str().unwrap().is_empty());
    let run = app.run(&id).await.unwrap();
    assert!(run["loaded_skills"].as_array().unwrap().contains(
        &json!({"ability_id":"codebase_admin","kind":"tool","skill_id":"state_inspect"})
    ));
    assert!(!run["loaded_skills"].to_string().contains("text"));
    let again = load("state_inspect").await;
    assert_eq!(again["output"]["status"], "already_available", "{again}");
    assert!(again["output"].get("text").is_none());
    // A Loaded tool's skill is already in the prompt.
    let visible = load("file_read").await;
    assert_eq!(
        visible["output"]["status"], "already_available",
        "{visible}"
    );
    assert_eq!(load("no_such_tool").await["status"], "error");
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["tool_modes"]["state_inspect"] = json!("off");
    app.store
        .save_agent_abilities(&visibility, revision)
        .await
        .unwrap();
    let off = load("state_inspect").await;
    assert_eq!(off["status"], "error", "{off}");
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["tool_modes"]["state_inspect"] = json!("on_request");
    app.store
        .save_agent_abilities(&visibility, revision)
        .await
        .unwrap();
    set_tool_permission(&app, "state_inspect", access::Permission::Deny).await;
    let denied = load("state_inspect").await;
    assert_eq!(denied["status"], "error", "{denied}");
    set_tool_permission(&app, "state_inspect", access::Permission::Auto).await;
    app.store
        .put(
            &key("session_ability_state", &sid),
            &json!({
        "revision":1,"active":[],"suppressed":["codebase_admin"]}),
        )
        .await
        .unwrap();
    let suppressed = load("state_inspect").await;
    assert_eq!(suppressed["status"], "error", "{suppressed}");
}
