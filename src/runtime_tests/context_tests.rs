//! Runtime acceptance tests: Context assembly, compaction, folds, mode guidance and lifetime-scoped memory.
use super::*;

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
    // The wire leaves empty instructions out; the logical envelope counts them as "".
    let envelope = json!({"instructions":request["instructions"].as_str().unwrap_or_default(),
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
    // The mode's per-turn guidance rides in this step's guidance message; the mode itself
    // never enters the instructions.
    assert!(native_call_suffix(request).contains("publish_main_message"));
    assert!(!request["instructions"]
        .as_str()
        .unwrap_or_default()
        .contains("Execution mode:"));
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
        .contains("Current session mode revision:"));
    assert!(!finished["input"]
        .to_string()
        .contains("Current session mode revision:"));
    assert!(!json!(context)
        .to_string()
        .contains("Current session mode revision:"));
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
    assert_eq!(
        without_mode_entries(&paused["input"]),
        without_mode_entries(&json!(original))
    );
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
/// Verify the full mode guidance enters the history once per mode and each step carries a one-line status.
async fn mode_guidance_enters_history_once_and_each_step_has_a_status() {
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
    // By default (mode_guidance.json "both") each step's guidance message carries the mode's
    // per-turn guidance, and the full guidance is a set_execution_mode result in the history:
    // the starting mode once, then the user's change once.
    let first_suffix = native_call_suffix(&requests[0]);
    let second_suffix = native_call_suffix(&requests[1]);
    assert!(first_suffix.starts_with("Harness guidance:"));
    assert!(second_suffix.contains("publish_main_message"));
    assert!(!second_suffix.contains("Plan mode is"));
    let results = |request: &Value| -> Vec<Value> {
        request["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| {
                item["type"] == "function_call_output"
                    && item["call_id"]
                        .as_str()
                        .is_some_and(|id| id.starts_with("set_execution_mode-"))
            })
            .map(|item| serde_json::from_str(item["output"].as_str().unwrap()).unwrap())
            .collect()
    };
    let starting = results(&requests[0]);
    assert_eq!(starting.len(), 1);
    assert_eq!(starting[0]["revision"], 0);
    let changed = results(&requests[1]);
    assert!(!changed.is_empty());
    assert_eq!(changed[changed.len() - 1]["mode"], "plan");
    // The saved history holds each entry once, its call before its result, so the compaction
    // planner sees a well-formed exchange.
    let saved = app.store.context(&sid).await.unwrap();
    let mode_items: Vec<&Value> = saved.iter().filter(|item| mode_entry(item)).collect();
    assert_eq!(mode_items.len() % 2, 0, "{}", json!(mode_items));
    for pair in mode_items.chunks(2) {
        assert_eq!(pair[0]["type"], "function_call");
        assert_eq!(pair[1]["type"], "function_call_output");
        assert_eq!(pair[0]["call_id"], pair[1]["call_id"]);
    }
    assert!(changed[changed.len() - 1]["guidance"]
        .as_str()
        .unwrap()
        .starts_with("Execution mode: Plan"));
    for request in requests.iter() {
        assert!(!request["instructions"]
            .as_str()
            .unwrap_or_default()
            .contains("Current session mode revision:"));
    }
    for id in [&first, &second] {
        let durable = app.run(id).await.unwrap();
        assert!(!durable["input"]
            .to_string()
            .contains("Current session mode revision:"));
    }
    assert!(!json!(app.store.context(&sid).await.unwrap())
        .to_string()
        .contains("Current session mode revision:"));
    server.abort();
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
        model_selection::RoleAssignment::new("system-summary"),
    );
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
    let config_path = app.compaction_config_path.clone();
    let mut config = context_compaction::Config::read(&config_path).unwrap();
    // Only Base stays on, so the sent tool schemas stay small beside the folded history.
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    for id in app.abilities.ids() {
        if id != "base" {
            visibility["visibility"][id] = json!("off");
        }
    }
    app.store
        .save_agent_abilities(&visibility, revision)
        .await
        .unwrap();
    config.trigger_tokens = 20_000;
    // No background window: the summary runs on System before the main call.
    config.ceiling_tokens = None;
    config.tail_tokens = 100;
    config.summary_tokens = 100;
    // The explicit System model writes the summary: the setting picks that path.
    config.summarizer = context_compaction::Summarizer::System;
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
    assert_eq!(done["status"], "completed", "{}", done["error"]);
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
        .contains("Write one summary block"));
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
    // The summary call and its result are activity rows, never model context.
    let activity = app.store.private_run_activity(&sid, &id, 0).await.unwrap();
    let events = activity["events"].as_array().unwrap();
    let call = events
        .iter()
        .find(|e| e["kind"] == "compaction_call")
        .unwrap();
    assert_eq!(call["payload"]["model"], "system-summary-model");
    assert!(call["payload"]["input_bytes"].as_u64().unwrap() > 0);
    assert_eq!(call["payload"]["reason"], "automatic");
    let result = events
        .iter()
        .find(|e| e["kind"] == "compaction_result")
        .unwrap();
    assert_eq!(result["payload"]["summary"], "Remember original goal");
    assert!(call["seq"].as_i64() < result["seq"].as_i64());
    assert_ne!(done["phase"], "compacting");
    // Rebuild from interactions alone: the compaction rows must not enter it.
    let mut session = app.store.get(&key("session", &sid)).await.unwrap().unwrap();
    session["checkpoint_through"] = json!(0);
    session["checkpoint_input"] = json!([]);
    app.store
        .put(&key("session", &sid), &session)
        .await
        .unwrap();
    let rebuilt = app.store.context(&sid).await.unwrap();
    assert!(!rebuilt.is_empty());
    assert!(rebuilt.iter().all(|item| !item["type"]
        .as_str()
        .unwrap_or("")
        .starts_with("compaction_")));
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
    assert_eq!(requests.len(), 1, "{}", run["pending"]);
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
    set_tool_permission(&app, "file_read", access::Permission::Auto).await;
    set_tool_permission(&app, "recall_compacted", access::Permission::Auto).await;
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

/// A final reply is followed by one private closing turn on the same model. Its request
/// repeats the final call's request (for the provider cache) plus the reply and the closer
/// message; the answer sets the session status and is never published. An invalid answer is
/// retried with its error, then the fallback applies. Each run publishes its reply once.
#[tokio::test]
async fn closing_turn_reuses_the_request_and_sets_the_status() {
    use axum::{routing::post, Router};
    use std::sync::atomic::AtomicUsize;
    let (_root, mut app) = fixture().await;
    let mut runtime: Value =
        serde_json::from_slice(&std::fs::read(&app.runtime_path).unwrap()).unwrap();
    runtime["workflow"]["modules"] = json!({"publication":["closing_status"]});
    std::fs::write(&app.runtime_path, serde_json::to_vec(&runtime).unwrap()).unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let bodies = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let (seen, kept) = (count.clone(), bodies.clone());
    let mock = Router::new().route(
        "/responses",
        post(move |body: String| {
            let (seen, kept) = (seen.clone(), kept.clone());
            async move {
                kept.lock()
                    .unwrap()
                    .push(serde_json::from_str(&body).unwrap_or(Value::Null));
                let text = match seen.fetch_add(1, Ordering::SeqCst) {
                    0 => "Final answer",
                    1 => "flag: complete\nnote: All done",
                    2 => "Second answer",
                    3 => "I think it went well",
                    _ => "flag: finished",
                };
                let output = json!({"type":"message","role":"assistant",
                "content":[{"type":"output_text","text":text}]});
                let event = json!({"type":"response.completed",
                "response":{"status":"completed","output":[output]}});
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
    // Both runs share one session: from its second run on, the request's opening items
    // are already placed, as in any ongoing conversation.
    let sid = conversation(&app).await;
    let mut sessions = Vec::new();
    for attempt in ["first", "second"] {
        let id = queued(&app, &sid).await;
        running(&app, &id, attempt).await;
        execute_run(
            app.clone(),
            id.clone(),
            attempt.into(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let run = app.run(&id).await.unwrap();
        assert_eq!(run["status"], "completed");
        assert!(run["closing_turn"].is_null());
        sessions.push((sid.clone(), id));
    }
    assert_eq!(
        count.load(Ordering::SeqCst),
        5,
        "reply, closer; reply, two closers"
    );
    let bodies = bodies.lock().unwrap();
    let input = |i: usize| bodies[i]["input"].as_array().unwrap().clone();
    // The second run's closing request repeats its final request's opening (instructions,
    // tools and history, which providers cache by prefix); the final request's call note is
    // not part of the history, so the closing request continues after the reply.
    let (final_input, closing_input) = (input(2), input(3));
    let note = final_input
        .last()
        .is_some_and(|item| item["role"] == "developer");
    let history = final_input.len() - usize::from(note);
    assert_eq!(final_input[..history], closing_input[..history]);
    let shared = history;
    assert_eq!(bodies[2]["instructions"], bodies[3]["instructions"]);
    assert_eq!(bodies[2]["tools"], bodies[3]["tools"]);
    assert!(closing_input[shared].to_string().contains("Second answer"));
    let message = closing_input.last().unwrap().to_string();
    assert!(
        message.contains("Your reply above is final") && message.contains("flag: <one flag key>")
    );
    assert!(!message.contains("previous answer was invalid"));
    assert!(input(4)
        .last()
        .unwrap()
        .to_string()
        .contains("previous answer was invalid"));
    assert!(!bodies
        .iter()
        .any(|body| body.to_string().contains("set_closer_status")));
    // Each run published its own reply once; closing answers were never published.
    let (chat, _) = app.store.chat_snapshot(&sid).await.unwrap();
    let replies = chat
        .iter()
        .filter(|row| row["role"] == "assistant")
        .map(|row| row["content"].to_string())
        .collect::<Vec<_>>();
    assert_eq!(replies.len(), 2);
    assert!(replies[0].contains("Final answer") && replies[1].contains("Second answer"));
    let first = app.run(&sessions[0].1).await.unwrap();
    assert_eq!(first["closing"]["flag"], "complete");
    assert_eq!(first["closing"]["notification"], "All done");
    let session = app
        .store
        .get(&format!("session:{sid}"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (session["flag"].as_str(), session["flag_source"].as_str()),
        (Some("unflagged"), Some("system"))
    );
    // Closing answers stay out of later context; their attempts are activity rows.
    let (sid, rid) = &sessions[1];
    let activity = app
        .store
        .private_run_activity(sid, rid, 0)
        .await
        .unwrap()
        .to_string();
    assert!(activity.contains("closing_status") && activity.contains("fallback"));
    let run = app.run(rid).await.unwrap();
    assert!(!run["input"].to_string().contains("went well"));
    server.abort();
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
    assert_eq!(core["availability"], "loaded");
    let core_id = core["skill_id"].clone();
    let default =
        visibility_native_call(&app, &sid, &id, "list_skills", json!({"ability_id":"base"})).await;
    assert!(default["output"]["skills"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["requires_activation"] == true && row["availability"] == "on_request"));
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
    // The skill text is the tool result itself.
    assert_eq!(result["output"]["text"], sentinel);
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
    // Base itself is always on; denying load_skill withdraws loaded guidance.
    set_tool_permission(&app, "load_skill", access::Permission::Deny).await;
    assert!(tools::core_skills::active_guidance(&app, &sid, &id)
        .await
        .unwrap()
        .is_empty());
}
/// A same-model summary that fails (a refused bridge fork) does not end a requested fold:
/// System retries it in the same compaction-only run.
#[tokio::test]
async fn failed_same_model_fold_retries_a_requested_fold_with_system() {
    use axum::routing::post;
    let (_root, mut app) = fixture().await;
    let config_path = app.compaction_config_path.clone();
    let mut config = context_compaction::Config::read(&config_path).unwrap();
    // Every enabled tool schema is sent, so the trigger leaves room for them.
    config.trigger_tokens = 120_000;
    config.ceiling_tokens = None;
    config.tail_tokens = 100;
    config.summary_tokens = 100;
    config.min_fold_tokens = 100;
    config.summarizer = context_compaction::Summarizer::SameModel;
    config.persist(&config_path).unwrap();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    // The same-model summary comes back empty (as a refused bridge fork fails); System answers.
    let mock = Router::new().route("/responses", post(move |Json(body): Json<Value>| {
        let captured = captured.clone();
        async move {
            let mut seen = captured.lock().await;
            seen.push(body);
            let text = if seen.len() == 1 { "" } else { "Folded by System" };
            let event = json!({"type":"response.completed","response":{"status":"completed",
                "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}]}});
            ([("content-type","text/event-stream")], format!("data: {event}\n\n"))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    run["compaction_only"] = json!(true);
    run["input"] = json!([{"role":"user","content":"old ".repeat(2_000)},
        {"role":"assistant","content":"done"},{"role":"user","content":"/compact"}]);
    app.save(&mut run).await.unwrap();
    app.store
        .put(
            &format!("compaction_request:{sid}"),
            &json!({"state":"pending","source":"command","focus":""}),
        )
        .await
        .unwrap();
    running(&app, &id, "compact-retry").await;
    execute_run(
        app.clone(),
        id.clone(),
        "compact-retry".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let done = app.run(&id).await.unwrap();
    assert_eq!(done["status"], "completed");
    assert_eq!(done["compactions"], 1);
    assert_eq!(
        requests.lock().await.len(),
        2,
        "same-model summary, then System"
    );
    let context = app.store.context(&sid).await.unwrap();
    assert!(context[0].to_string().contains("Folded by System"));
    // The run's closing reply reports the fold, not the same-model failure.
    let note = done["compaction_only_note"].to_string();
    assert!(!note.contains("Compaction failed"), "{note}");
}

/// `/compact` on an idle chat: the compaction-only run makes no model call. It folds the
/// earlier history in the background with the user's focus, applies the summary and ends
/// with a short reply.
#[tokio::test]
async fn compaction_only_run_folds_without_a_model_call() {
    use axum::routing::post;
    let (_root, mut app) = fixture().await;
    let config_path = app.compaction_config_path.clone();
    let mut config = context_compaction::Config::read(&config_path).unwrap();
    // Every enabled tool schema is sent, so the trigger leaves room for them.
    config.trigger_tokens = 120_000;
    config.ceiling_tokens = None;
    config.tail_tokens = 100;
    config.summary_tokens = 100;
    config.min_fold_tokens = 100;
    config.summarizer = context_compaction::Summarizer::System;
    config.persist(&config_path).unwrap();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let mock = Router::new().route("/responses", post(move |Json(body): Json<Value>| {
        let captured = captured.clone();
        async move {
            captured.lock().await.push(body);
            let event = json!({"type":"response.completed","response":{"status":"completed",
                "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Folded earlier work"}]}]}});
            ([("content-type","text/event-stream")], format!("data: {event}\n\n"))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    run["compaction_only"] = json!(true);
    run["input"] = json!([{"role":"user","content":"old ".repeat(2_000)},
        {"role":"assistant","content":"done"},{"role":"user","content":"/compact"}]);
    app.save(&mut run).await.unwrap();
    app.store
        .put(
            &format!("compaction_request:{sid}"),
            &json!({"state":"pending","source":"command","focus":"the API design"}),
        )
        .await
        .unwrap();
    running(&app, &id, "compact-only").await;
    execute_run(
        app.clone(),
        id.clone(),
        "compact-only".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let done = app.run(&id).await.unwrap();
    assert_eq!(done["status"], "completed");
    assert_eq!(done["compactions"], 1);
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 1, "only the summary call is made");
    assert!(requests[0]["input"].to_string().contains("the API design"));
    let context = app.store.context(&sid).await.unwrap();
    assert!(context[0].to_string().contains("Folded earlier work"));
    // The closing reply is shown in the chat but never enters the agent's context.
    assert!(!context.iter().any(|item| item["role"] == "assistant"));
    let replies: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM chat WHERE session_id=? AND role='assistant' AND run_id=?",
    )
    .bind(&sid)
    .bind(&id)
    .fetch_one(&app.store.pool)
    .await
    .unwrap();
    assert_eq!(replies, 1);
    let request = app
        .store
        .get(&format!("compaction_request:{sid}"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request["state"], "started");
}
