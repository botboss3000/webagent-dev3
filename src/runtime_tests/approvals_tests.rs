//! Runtime acceptance tests: Approvals, saved approvals, tool policy gates, parallel batches and the critical tool guard.
use super::*;

#[tokio::test]
#[cfg(feature = "administrator")]
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

/// "Remember this approval": approving a call with `remember` saves it, and later calls of the
/// tool with the same short arguments run without asking, alone or in a parallel batch, each
/// with an activity row. A Deny or an ineligible call cannot save one.
#[tokio::test]
async fn saved_approval_lets_matching_calls_run_without_asking() {
    use axum::routing::post;
    let (_root, mut app) = fixture().await;
    settings(State(app.clone()), Json(json!({"model":"pinned-model"})))
        .await
        .unwrap();
    fixture_model(&app, "pinned-model").await;
    set_tool_permission(&app, "get_time", access::Permission::Ask).await;
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let mock = Router::new().route("/responses", post(|AxumState(requests): AxumState<Arc<Mutex<Vec<Value>>>>, Json(body): Json<Value>| async move {
        let mut requests = requests.lock().await;
        requests.push(body);
        let call = |id: &str| json!({"type":"function_call","call_id":id,"name":"get_time","arguments":"{}"});
        let output = match requests.len() {
            1 => json!([call("first")]),
            2 => json!([call("pair-a"), call("pair-b")]),
            3 => json!([call("single")]),
            _ => json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done."}]}]),
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
    assert_eq!(
        session_mode(&app, &sid).await.unwrap()["execution_mode"],
        "ask"
    );
    let id = queued(&app, &sid).await;
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
    assert_eq!(waiting["pending"]["name"], "get_time");
    assert_eq!(waiting["pending"]["remember"]["agent_wide"], true);
    let approval_id = waiting["pending"]["id"].as_str().unwrap().to_owned();
    let remember = json!({"match":"values","reach":"chat"});
    for refused in [
        json!({"client_request_id":"deny","decision":"deny","remember":remember}),
        json!({"client_request_id":"shape","decision":"approve","remember":{"match":"values"}}),
    ] {
        assert!(
            approval(State(app.clone()), Path(approval_id.clone()), Json(refused))
                .await
                .is_err()
        );
    }
    approval(
        State(app.clone()),
        Path(approval_id),
        Json(json!({"client_request_id":"save","decision":"approve","remember":remember})),
    )
    .await
    .unwrap();
    let saved = app
        .store
        .get(&access::saved_approvals::key("shared_default"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved["revision"], 1);
    assert_eq!(saved["entries"][0]["tool"], "get_time");
    assert_eq!(saved["entries"][0]["reach"], "chat");
    assert_eq!(saved["entries"][0]["fixed"], json!({}));
    running(&app, &id, "second").await;
    execute_run(
        app.clone(),
        id.clone(),
        "second".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let done = app.run(&id).await.unwrap();
    assert_eq!(done["status"], "completed", "{done}");
    assert_eq!(requests.lock().await.len(), 4);
    let activity = app
        .store
        .private_run_activity(&sid, &id, 0)
        .await
        .unwrap()
        .to_string();
    assert_eq!(activity.matches("\"saved_approval\"").count(), 3);
    // Activity-only rows never reach the model.
    assert!(!json!(app.store.context(&sid).await.unwrap())
        .to_string()
        .contains("saved_approval"));
    // The ended run added its tool use once: four calls, one approval asked, three saved.
    let agent = done["agent_id"].as_str().unwrap_or("shared_default");
    let counted = app.store.tool_use(agent).await.unwrap();
    assert_eq!(
        counted,
        vec![json!({"tool":"get_time","calls":4,"asked":1,"saved":3,
            "last_used_at":counted[0]["last_used_at"]})]
    );
    assert!(counted[0]["last_used_at"].as_i64().unwrap() > 0);
    server.abort();
}

/// Leaving Plan needs the agreed plan: without one the call is refused before any approval
/// card, even inside a multi-call step. With one, "Continue in Ask mode" approves the call
/// with Ask as the target, and the plan is saved as a session artifact.
#[tokio::test]
async fn plan_exit_needs_a_plan_and_can_continue_in_ask_mode() {
    use axum::routing::post;
    let (_root, mut app) = fixture().await;
    settings(State(app.clone()), Json(json!({"model":"pinned-model"})))
        .await
        .unwrap();
    fixture_model(&app, "pinned-model").await;
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let mock = Router::new().route("/responses", post(|AxumState(requests): AxumState<Arc<Mutex<Vec<Value>>>>, Json(body): Json<Value>| async move {
        let mut requests = requests.lock().await;
        requests.push(body);
        let plan = json!({"title":"Standup note","goal":"Write a daily note",
            "checklist":[{"label":"Write the note"},{"label":"Schedule it"}]});
        let output = match requests.len() {
            1 => json!([{"type":"function_call","call_id":"load","name":"get_time","arguments":"{}"}]),
            2 => json!([{"type":"function_call","call_id":"noplan","name":"set_execution_mode","arguments":"{\"mode\":\"auto\"}"},
                {"type":"function_call","call_id":"time","name":"get_time","arguments":"{}"}]),
            3 => json!([{"type":"function_call","call_id":"call","name":"set_execution_mode",
                "arguments":json!({"mode":"auto","plan":plan}).to_string()}]),
            _ => json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Executing in Ask mode."}]}]),
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
            mode: "plan".into(),
            expected_revision: Some(0),
            confirm_write: true,
        }),
    )
    .await
    .unwrap();
    let id = queued(&app, &sid).await;
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
    assert_eq!(waiting["pending"]["name"], "set_execution_mode");
    assert_eq!(waiting["pending"]["call_id"], "call");
    assert!(app
        .store
        .base_chat_components(&sid)
        .await
        .unwrap()
        .is_empty());
    let approval_id = waiting["pending"]["id"].as_str().unwrap().to_owned();
    // The Ask alternative only applies to approving the switch.
    assert!(approval(
        State(app.clone()),
        Path(approval_id.clone()),
        Json(json!({"client_request_id":"bad","decision":"deny","alternate_mode":"ask"}))
    )
    .await
    .is_err());
    approval(
        State(app.clone()),
        Path(approval_id),
        Json(json!({"client_request_id":"ask","decision":"approve","alternate_mode":"ask"})),
    )
    .await
    .unwrap();
    running(&app, &id, "second").await;
    execute_run(
        app.clone(),
        id.clone(),
        "second".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(app.run(&id).await.unwrap()["status"], "completed");
    let mode = session_mode(&app, &sid).await.unwrap();
    assert_eq!(
        (&mode["execution_mode"], &mode["revision"]),
        (&json!("ask"), &json!(2))
    );
    // The plan is a session artifact, not a transcript card, and goes with its session.
    assert!(app
        .store
        .base_chat_components(&sid)
        .await
        .unwrap()
        .is_empty());
    let artifacts = app.store.session_artifacts(&sid).await.unwrap();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(
        (
            &artifacts[0]["id"],
            &artifacts[0]["kind"],
            &artifacts[0]["title"]
        ),
        (&json!("plan"), &json!("plan"), &json!("Standup note"))
    );
    assert_eq!(artifacts[0]["sections"][0]["value"], "Write a daily note");
    // The approved plan's checklist became the session task list's first section.
    let tasks = app
        .store
        .get(&format!("base_tasks:{sid}"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tasks["list"]["sections"][0]["origin"], "user");
    assert_eq!(tasks["list"]["sections"][0]["plan"], true);
    let requests = requests.lock().await;
    // The final reply left the plan's steps open; that costs no extra call.
    assert_eq!(requests.len(), 4);
    let output = |n: usize, call: &str| {
        requests[n]["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["type"] == "function_call_output" && v["call_id"] == call)
            .map(|v| v["output"].as_str().unwrap().to_owned())
            .unwrap()
    };
    assert!(output(2, "noplan").contains("plan_required"));
    assert!(
        output(3, "call").contains("chose Ask mode"),
        "{}",
        output(3, "call")
    );
    assert!(
        output(3, "call").contains("task_list"),
        "{}",
        output(3, "call")
    );
    drop(requests);
    app.store.delete_agent_session(&sid).await.unwrap();
    assert!(app.store.session_artifacts(&sid).await.unwrap().is_empty());
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
            mode: "ask".into(),
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
    // The shell tool is denied by default; this journey needs one that waits for approval.
    set_tool_permission(&app, "shell", access::Permission::Ask).await;
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
        .find(|v| v["type"] == "function_call_output" && !mode_entry(v))
        .unwrap();
    assert_eq!(tool_output["call_id"], "call");
    assert!(tool_output["output"].as_str().unwrap().contains("denied"));
    server.abort();
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
        .find(|item| item["type"] == "function_call_output" && !mode_entry(item))
        .unwrap();
    let result: Value = serde_json::from_str(output["output"].as_str().unwrap()).unwrap();
    assert_eq!(result["status"], "success");
    assert_eq!(result["output"]["slot"]["revision"], 1);
    server.abort();
}

#[tokio::test]
async fn task_gate_files_the_request_before_the_first_change_without_an_extra_call() {
    use axum::routing::post;

    let (_root, mut app) = fixture().await;
    set_task_gate(&app, true).await;
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
    // Fixtures do not count messages as requests; count this one as the app would.
    let mut tx = app.store.pool.begin().await.unwrap();
    app.store
        .record_task_request(&mut tx, &sid, &format!("user:{id}"), "hello")
        .await
        .unwrap();
    tx.commit().await.unwrap();
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
    // The change ran: with no section open the gate opened one citing the user's message,
    // with no steps, and the turn cost no call beyond the tool call and the reply.
    let document = app.store.task_list_current(&sid).await.unwrap().unwrap();
    let section = &document["list"]["sections"][0];
    assert_eq!(section["requests"], json!(["u1"]));
    assert_eq!(section["title"], "hello");
    assert_eq!(section["steps"], json!([]));
    // The reply closed it: it had no unfinished step.
    assert_eq!(section["status"], "done");
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 2);
    // The second call carries the filing as a delivered list change.
    assert!(
        requests[1]["input"].to_string().contains("The app filed"),
        "the filing reaches the agent"
    );
    let output = requests[1]["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output" && !mode_entry(item))
        .unwrap();
    let result: Value = serde_json::from_str(output["output"].as_str().unwrap()).unwrap();
    assert_eq!(result["status"], "success");
    assert_eq!(result["output"]["slot"]["revision"], 1);
    server.abort();
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
        Some(json!({"visibility":"loaded","expected_revision":0})),
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
        "mode_revision":0,"visibility_revision":1,
        "ability_state_revision":0,"state":"waiting","decision":null});
    app.save(&mut run).await.unwrap();
    let (status, _) = ability_request(
        &app,
        Method::PUT,
        agent_uri,
        Some(json!({"visibility":"on_request","expected_revision":1})),
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
async fn phase3_http_tier_tools_and_agent_disable_reject_stale_approval() {
    let (_root, app) = fixture().await;
    let account = app
        .store
        .create_user_account("phase3_tier", "fixture-only-password-hash")
        .await
        .unwrap();
    let user_id = account["user_id"].as_str().unwrap().to_owned();
    app.store
        .update_account_access(&user_id, 1, access::AccountStatus::Active, "free")
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
    // Models follow the agent owner's tier (here the administrator's), not the caller's: the
    // refused side is `plus_tier_tests::the_owners_tier_decides_which_models_run_on_its_agent…`.
    let granted_model = app.selection().await.unwrap().model;
    let response = router(app.clone())
        .oneshot(request(
            &admin_token,
            "admin-csrf",
            "PUT",
            "/api/v3/admin/access/tiers/free",
            json!({"expected_revision":1,"tier":{"id":"free","revision":1,
            "allowed_models":[granted_model],"max_sessions":10,"max_active_runs":2}}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let access = app
        .store
        .live_access(&user_id, "shared_default")
        .await
        .unwrap();
    // Administrator abilities need an administrator caller, whatever the agent grants.
    for (name, is_read) in [("file_read", true), ("file_write", false), ("shell", false)] {
        let decision =
            access.tool_decision_for_abilities(name, &["codebase_admin".into()], true, true);
        assert_eq!(decision.permission, access::Permission::Deny, "{name}");
        // Hidden from non-administrators rather than offered with a tier error.
        assert_eq!(decision.reason, access::Reason::CallerDenied, "{name}");
    }
    let host = access.tool_decision("build_work", true, true);
    assert_eq!(host.permission, access::Permission::Deny);
    assert_eq!(host.reason, access::Reason::CallerDenied);
    // The runtime refuses every Administrator tool to a non-administrator, whatever the
    // tier grants above.
    for name in ["file_read", "file_write", "shell", "terminal_open"] {
        let decision = live_tool_decision(&app, &access, name, &json!({}), &json!({}), &json!({}));
        assert_eq!(decision.permission, access::Permission::Deny, "{name}");
        assert_eq!(decision.reason, access::Reason::CallerDenied, "{name}");
    }
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
        "access_revision":access.revision,"mode_revision":0,
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
async fn parallel_batch_overlaps_reads_denies_write_by_policy_and_preserves_result_order() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    let (_root, app) = fixture().await;
    set_tool_permission(&app, "file_write", access::Permission::Deny).await;
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["tool_execution"] = json!({"file_list":{
        "mode":"foreground","max_concurrency":2}});
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
        TEST_TIMEOUT,
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
async fn parallel_writes_need_independent_approvals_and_return_uncertain_call_to_agent() {
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
            mode: "ask".into(),
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
        TEST_TIMEOUT,
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
    tokio::time::timeout(TEST_TIMEOUT, worker)
        .await
        .expect("Fake worker did not receive both approved workspace writes")
        .unwrap();
    // The uncertain write settles as its result; the repeat guard remembers it.
    let done = app.run(&id).await.unwrap();
    assert_ne!(done["status"], "recovery");
    assert_eq!(done["batch"][1]["state"], "done");
    assert_eq!(done["batch"][1]["result"]["status"], "uncertain");
    let guarded = done["retry_guard"]["actions"].as_object().unwrap();
    assert!(guarded
        .values()
        .any(|entry| entry["call_id"] == "write-two" && entry["state"] == "uncertain"));
    // The run stays claimed; the same generation's next pass hands both results to the model.
    let guard = app.gate.lock().await;
    parallel_batch::execute_parallel_batch(
        &app,
        &id,
        "third",
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
async fn a_message_parks_a_waiting_approval_and_the_decided_call_reports_as_a_notice() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    run["phase"] = json!("tools");
    run["status"] = json!("waiting_approval");
    run["calls"] = json!([{"call_id":"c1","name":"file_write","arguments":"{}"},
        {"call_id":"c2","name":"file_read","arguments":"{}"}]);
    run["call_index"] = json!(0);
    run["input"] = json!([{"type":"function_call","call_id":"c1","name":"file_write","arguments":"{}"},
        {"type":"function_call","call_id":"c2","name":"file_read","arguments":"{}"}]);
    run["pending"] = json!({"id":"approval-1","name":"file_write","call_id":"c1",
        "arguments":{"path":"a.txt"},"state":"waiting","decision":null});
    app.save(&mut run).await.unwrap();
    // Without a waiting message nothing changes.
    requeue_auto_approval(&app, &id).await.unwrap();
    assert!(app.run(&id).await.unwrap()["parked"].is_null());
    let entry = json!({"id":"m1","message_id":"user:m1","session_id":sid,
        "agent_id":"shared_default","revision":0,"choice":"steer","status":"pending",
        "review_status":"acceptable","content":"meanwhile","context_content":"meanwhile",
        "attachments":[]});
    app.store
        .followup_submit(&sid, entry, vec![], "m1", "digest", &json!({}))
        .await
        .unwrap();
    requeue_auto_approval(&app, &id).await.unwrap();
    let parked = app.run(&id).await.unwrap();
    assert_eq!(parked["parked"], true);
    assert_eq!(parked["status"], "waiting_approval");
    assert_eq!(parked["pending"]["id"], "approval-1");
    let task_id = parked["parked_tasks"][0]["task_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let task = app.store.background_get(&task_id).await.unwrap().unwrap();
    assert_eq!(task["status"], "running");
    assert_eq!(task["parked_approval"], true);
    // Every call of the turn has a recorded result; the waiting one names its task.
    let outputs: Vec<&Value> = parked["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .collect();
    assert_eq!(outputs.len(), 2);
    assert!(outputs[0]["output"].as_str().unwrap().contains(&task_id));
    assert!(outputs[1]["output"].as_str().unwrap().contains("not_run"));
    // The conversation is free for its next run; the parked run takes no messages.
    let views = app.store.active_runs().await.unwrap();
    assert!(views
        .iter()
        .any(|view| view["id"] == id.as_str() && view["parked"] == true));
    assert!(!app.engine().message_pending(&parked).await.unwrap());
    // Cancelling the parked task declines its approval.
    let mut cancelled = task.clone();
    cancelled["status"] = json!("cancelled");
    let revision = task["revision"].as_u64();
    assert!(app
        .store
        .background_cas(&cancelled, revision)
        .await
        .unwrap());
    requeue_auto_approval(&app, &id).await.unwrap();
    let declined = app.run(&id).await.unwrap();
    assert_eq!(declined["status"], "queued");
    assert_eq!(declined["pending"]["decision"], "deny");
    // Once the declined call has its result the run ends and reports it as a notice.
    let mut done = declined.clone();
    done["status"] = json!("running");
    done["pending"] = Value::Null;
    done["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"function_call_output",
        "call_id":"c1","output":json!({"status":"denied","output":"Declined"}).to_string()}));
    app.engine().finish_parked(done).await.unwrap();
    assert_eq!(app.run(&id).await.unwrap()["status"], "completed");
    let notice = app
        .store
        .get(&format!("background_notice:{sid}:tool:{task_id}"))
        .await
        .unwrap()
        .expect("the decided call reports as a background notice");
    assert_eq!(notice["status"], "denied");
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
            mode: "ask".into(),
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
        TEST_TIMEOUT,
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
    tokio::time::timeout(TEST_TIMEOUT, worker)
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
        "mode":"foreground","max_concurrency":1}});
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
        TEST_TIMEOUT,
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
            mode: "ask".into(),
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
        TEST_TIMEOUT,
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
    phase3_api::stop_agent_runs(&app, "shared_default", "test change")
        .await
        .unwrap();
    let after = app.run(&id).await.unwrap();
    assert_eq!(after["status"], "recovery");
    assert_eq!(
        after["error"],
        "Access changed (test change) during an uncertain tool effect; review required"
    );
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
async fn auto_batch_keeps_an_approved_repeat_confirmation() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    app.store
        .put(
            &key("session_mode", &sid),
            &json!({"execution_mode":"auto","revision":1}),
        )
        .await
        .unwrap();
    activate_codebase(&app, &sid).await;
    let id = queued(&app, &sid).await;
    running(&app, &id, "repeat-approve").await;
    let mut run = app.run(&id).await.unwrap();
    run["phase"] = json!("tools");
    run["loaded_abilities"] = json!(["codebase_admin"]);
    let (mode_revision, visibility, state) = ability_controls(&app, &sid).await.unwrap();
    let access = app
        .store
        .live_access(app.store.current_user_id(), "shared_default")
        .await
        .unwrap();
    let args = json!({"path":"fixture"});
    let calls = (0..2)
        .map(|index| {
            json!({
                "type":"function_call","call_id":format!("repeat-read-{index}"),
                "name":"file_read","arguments":args.to_string()
            })
        })
        .collect::<Vec<_>>();
    for call in &calls {
        run["input"].as_array_mut().unwrap().push(call.clone());
    }
    run["calls"] = json!(calls);
    // The user approved a repeat confirmation; Auto mode never converts one.
    let pending = json!({
        "id":"repeat-pending","name":"file_read","call_id":"repeat-read-0","arguments":args,
        "digest":digest(&args),"mode_revision":mode_revision,
        "visibility_revision":visibility["revision"],"ability_state_revision":state["revision"],
        "access_revision":access.revision,"state":"waiting","decision":null,
        "new_intent_confirmation":true,"repeat_of":"earlier-call"
    });
    run["batch"] = json!([
        {"state":"waiting","pending":pending,"result":null,"pre_tool_receipt":null},
        {"state":"done","pending":{"call_id":"repeat-read-1","name":"file_read"},
         "result":{"status":"success","output":"earlier"},"pre_tool_receipt":null}
    ]);
    run["pending"] = pending;
    run["pending"]["decision"] = json!("approve");
    app.save(&mut run).await.unwrap();
    let _ = tokio::time::timeout(
        Duration::from_secs(5),
        execute_run(
            app.clone(),
            id.clone(),
            "repeat-approve".into(),
            CancellationToken::new(),
        ),
    )
    .await;
    let after = app.run(&id).await.unwrap();
    assert_ne!(after["pending"]["id"], "repeat-pending");
    assert_ne!(after["status"], "waiting_approval");
    // A finished run keeps no input copy; the result lives in the session history.
    let history = app.store.context(&sid).await.unwrap();
    assert!(history.iter().any(|item| {
        item["type"] == "function_call_output" && item["call_id"] == "repeat-read-0"
    }));
}

#[tokio::test]
async fn background_approved_reads_pin_authority_and_claim_identity() {
    for batched in [false, true] {
        let (_root, app) = fixture().await;
        let sid = conversation(&app).await;
        activate_codebase(&app, &sid).await;
        set_tool_permission(&app, "file_read", access::Permission::Ask).await;
        let rid = queued(&app, &sid).await;
        running(&app, &rid, "background-origin").await;
        let mut run = app.run(&rid).await.unwrap();
        run["loaded_abilities"] = json!(["codebase_admin"]);
        let (mode_revision, visibility, state) = ability_controls(&app, &sid).await.unwrap();
        let access = app
            .store
            .live_access(app.store.current_user_id(), "shared_default")
            .await
            .unwrap();
        let args = json!({"path":"fixture"});
        let mut pending = json!({
            "id":"background-pending","name":"file_read","call_id":"background-call",
            "arguments":args,"digest":digest(&args),
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
        let version = 0;
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
        let task_id = handle["call_id"].as_str().unwrap();
        let mut task = app.store.background_get(task_id).await.unwrap().unwrap();
        assert!(background_tasks::permitted(&app, &task).await.unwrap());
        for field in [
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
            // A finished run no longer stops its background reads; the session stop does.
            Some(true)
        );
        recovered["status"] = json!("running");
        app.store.put(&key("run", &rid), &recovered).await.unwrap();
        set_tool_permission(&app, "file_read", access::Permission::Deny).await;
        assert!(!background_tasks::permitted(&app, &task).await.unwrap());
    }
}

#[tokio::test]
async fn foreground_queue_revocation_denies_before_worker_dispatch() {
    let (_root, app) = fixture().await;
    let mut visibility = app.store.agent_abilities().await.unwrap();
    let revision = visibility["revision"].as_u64().unwrap();
    visibility["tool_execution"] = json!({"file_list":{
        "mode":"foreground","max_concurrency":1
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
    let (mode_revision, visibility, state) = ability_controls(&app, &sid).await.unwrap();
    let access = app
        .store
        .live_access(app.store.current_user_id(), "shared_default")
        .await
        .unwrap();
    run["pending"] = json!({
        "id":"queued-revocation-pending","name":"file_list","call_id":"queued-revocation-call",
        "arguments":args,"digest":digest(&args),
        "mode_revision":mode_revision,"visibility_revision":visibility["revision"],
        "ability_state_revision":state["revision"],"access_revision":access.revision,
        "state":"executing","decision":"consumed","human_approved":false
    });
    app.save(&mut run).await.unwrap();
    let settings = tool_execution::resolve(&visibility, "file_list").unwrap();
    let cancel = CancellationToken::new();
    let blocker = tokio::time::timeout(
        TEST_TIMEOUT,
        tool_execution::acquire(
            &app.runtime_path,
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
        0,
        cancel,
    );
    tokio::pin!(operation);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut operation)
            .await
            .is_err()
    );
    set_tool_permission(&app, "file_list", access::Permission::Deny).await;
    drop(blocker);
    let result = tokio::time::timeout(TEST_TIMEOUT, &mut operation)
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
    let (mode_revision, visibility, state) = ability_controls(&app, &sid).await.unwrap();
    let access = app
        .store
        .live_access(app.store.current_user_id(), "shared_default")
        .await
        .unwrap();
    let args = json!({"path":"fixture"});
    run["pending"] = json!({
        "id":"auto-background-pending","name":"file_read","call_id":"auto-background-call",
        "arguments":args,"digest":digest(&args),
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
        0,
        &settings,
    )
    .await
    .unwrap();
    let task = app
        .store
        .background_get(handle["call_id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    let mut changed_state = state.clone();
    changed_state["revision"] = json!(state["revision"].as_u64().unwrap() + 1);
    app.store
        .put(&key("session_ability_state", &sid), &changed_state)
        .await
        .unwrap();
    set_tool_permission(&app, "file_read", access::Permission::Auto).await;
    assert!(background_tasks::permitted(&app, &task).await.unwrap());
    let mut approved = task.clone();
    approved["authorization"]["human_approved"] = json!(true);
    assert!(!background_tasks::permitted(&app, &approved).await.unwrap());
    let mut changed_args = task.clone();
    changed_args["arguments"]["path"] = json!("different");
    assert!(!background_tasks::permitted(&app, &changed_args)
        .await
        .unwrap());
    set_tool_permission(&app, "file_read", access::Permission::Ask).await;
    assert!(!background_tasks::permitted(&app, &task).await.unwrap());
    set_tool_permission(&app, "file_read", access::Permission::Deny).await;
    assert!(!background_tasks::permitted(&app, &task).await.unwrap());
    set_tool_permission(&app, "file_read", access::Permission::Auto).await;
    changed_state["suppressed"] = json!(["codebase_admin"]);
    app.store
        .put(&key("session_ability_state", &sid), &changed_state)
        .await
        .unwrap();
    assert!(!background_tasks::permitted(&app, &task).await.unwrap());
}

#[tokio::test]
async fn the_critical_tool_guard_holds_an_unmatched_call_even_in_auto_mode() {
    let (_app, run, requests) = guarded_turn("unclear none").await;
    assert_eq!(requests.len(), 1, "{}", run["pending"]);
    // The guard sees the user's labelled message and the call, never the agent's reasoning.
    let prompt = requests[0].to_string();
    assert!(prompt.contains("u1") && prompt.contains("save_own_skill"));
    assert_eq!(run["status"], "waiting_approval");
    assert_eq!(run["pending"]["guard_hold"], true);
    assert_eq!(run["pending"]["guard"]["verdict"], "unclear");
    assert_eq!(run["pending"]["guard"]["passed"], false);
    assert_eq!(
        run["pending"]["guard"]["line"],
        "Guard: unclear whether you asked for this"
    );
}

#[tokio::test]
async fn the_critical_tool_guard_lets_a_matched_call_run_and_records_the_request() {
    let (app, run, requests) = guarded_turn("requested u1").await;
    assert_eq!(requests.len(), 1, "{}", run["pending"]);
    assert_ne!(run["status"], "waiting_approval");
    let actions = app.store.action_list("agent:shared_default").await.unwrap();
    let action = actions
        .iter()
        .find(|a| a["tool"] == "save_own_skill")
        .expect("critical call recorded");
    assert_eq!(action["guard"]["verdict"], "requested");
    assert_eq!(action["guard"]["passed"], true);
    assert_eq!(action["request_link"], "guard");
    assert_eq!(action["requests"][0]["label"], "u1");
}
