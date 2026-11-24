//! Runtime acceptance tests: Final publication, checkpoints, receipts and admission records.
use super::*;

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

/// A tool result answered in the same model response is saved after its call, so
/// the next turn's history pairs every tool result with an earlier call.
#[tokio::test]
async fn same_response_tool_result_is_saved_after_its_call() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    run["turns"] = json!(1);
    let call = json!({"type":"function_call","call_id":"closer-call","name":"present_chat_component","arguments":"{}"});
    let output = json!({"type":"function_call_output","call_id":"closer-call","output":"{}"});
    run["input"]
        .as_array_mut()
        .unwrap()
        .extend([call.clone(), output]);
    app.store
        .save_run_observed(&mut run, "model_output", &json!({"output":[call]}))
        .await
        .unwrap();
    let activity = app.store.private_run_activity(&sid, &id, 0).await.unwrap();
    let kinds: Vec<&str> = activity["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|event| event["kind"].as_str())
        .filter(|kind| kind.starts_with("tool_"))
        .collect();
    assert_eq!(kinds, ["tool_call", "tool_result"]);
}
