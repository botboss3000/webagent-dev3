//! Deterministic model fixtures exercise admission independently of LLM judgment quality.
use super::*;
use axum::{routing::post, Json, Router};
async fn settings(app: &App, enabled: bool) {
    let store = app.store.for_agent("shared_default").await.unwrap();
    let raw: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id='shared_default'")
        .fetch_one(&store.agent_pool)
        .await
        .unwrap();
    let mut metadata = store.unseal(&raw).unwrap();
    metadata["manager_incoming_message_revision"] = json!(
        metadata["manager_incoming_message_revision"]
            .as_u64()
            .unwrap_or(0)
            + 1
    );
    metadata["manager_incoming_message"] =
        json!({"enabled":enabled,"criteria":"Only accept questions about clothing and sizes."});
    sqlx::query("UPDATE agents SET metadata=? WHERE id='shared_default'")
        .bind(store.seal(&metadata).unwrap())
        .execute(&store.agent_pool)
        .await
        .unwrap();
}
async fn session(app: &App) -> String {
    crate::create_session(
        State(app.clone()),
        Json(json!({"client_request_id":uuid(),"agent_id":"shared_default"})),
    )
    .await
    .unwrap()
    .0["id"]
        .as_str()
        .unwrap()
        .to_owned()
}
async fn submit(app: &App, sid: &str, request: &str, text: &str) -> Value {
    crate::send(
        State(app.clone()),
        Path(sid.to_owned()),
        Json(json!({"client_message_id":request,"text":text})),
    )
    .await
    .unwrap()
    .1
     .0
}
async fn provider(
    app: &mut App,
    answer: &str,
    delay: bool,
) -> (tokio::task::JoinHandle<()>, Arc<Mutex<Vec<Value>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let captured = calls.clone();
    let answer = answer.to_owned();
    let mock=Router::new().route("/responses",post(move |Json(body):Json<Value>| {
        let calls=captured.clone();let answer=answer.clone();
        async move {
            calls.lock().await.push(body.clone());
            if delay {tokio::time::sleep(Duration::from_millis(100)).await;}
            let text=if body["instructions"].as_str().unwrap_or("").contains("admission judge") {answer.as_str()} else {"We have small, medium and large."};
            let event=json!({"type":"response.completed","response":{"status":"completed",
                "usage":{"input_tokens":12,"output_tokens":4},
                "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}]}});
            ([("content-type","text/event-stream")],format!("data: {event}\n\n"))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    (server, calls)
}
#[tokio::test]
async fn recycling_cancels_pending_review_without_provider_dispatch() {
    let (_root,mut app)=crate::runtime_tests::fixture().await;
    settings(&app,true).await;
    let (server,calls)=provider(&mut app,r#"{"verdict":"acceptable"}"#,false).await;
    let sid=session(&app).await;
    let result=submit(&app,&sid,"request","Write a Python program").await;
    let id=result["review_id"].as_str().unwrap();
    let metadata=app.store.session_read_summary(&sid).await.unwrap();
    app.store.lifecycle_session(&sid,&crate::store::session::SessionLifecycle {
        client_request_id:"recycle".into(),expected_revision:metadata["metadata_revision"].as_u64().unwrap()
    },false).await.unwrap();
    super::scheduler::process(app.clone(),id.to_owned()).await.unwrap();
    assert!(calls.lock().await.is_empty());
    assert!(app.store.get(&format!("run:{id}")).await.unwrap().is_none());
    assert_eq!(app.store.get(&format!("manager_review:{id}")).await.unwrap().unwrap()["status"],"cancelled");
    assert_eq!(crate::send(State(app),Path(sid),Json(json!({"text":"late","client_message_id":"request"}))).await.unwrap_err().0,StatusCode::NOT_FOUND);
    server.abort();
}

#[tokio::test]
async fn unacceptable_keeps_chat_excludes_context_and_retry_does_not_rejudge() {
    let (_root, mut app) = crate::runtime_tests::fixture().await;
    settings(&app, true).await;
    let (server, calls) = provider(&mut app, r#"{"verdict":"unacceptable"}"#, false).await;
    let sid = session(&app).await;
    let result = submit(&app, &sid, "request", "Write a Python program").await;
    assert!(result["run_id"].is_null());
    let id = result["review_id"].as_str().unwrap();
    assert_eq!(app.store.chat_snapshot(&sid).await.unwrap().0.len(), 1);
    assert_eq!(
        app.store
            .get(&format!("session:{sid}"))
            .await
            .unwrap()
            .unwrap()["auto_naming"]["stage"],
        "awaiting_message"
    );
    assert!(app.store.context(&sid).await.unwrap().is_empty());
    assert!(app.store.active_runs().await.unwrap().is_empty());
    // A pending gate fences additional messages, even through the ordinary API.
    assert!(crate::send(
        State(app.clone()),
        Path(sid.clone()),
        Json(json!({"client_message_id":"other","text":"Sizes?"}))
    )
    .await
    .is_err());
    super::scheduler::process(app.clone(), id.to_owned())
        .await
        .unwrap();
    let retry = submit(&app, &sid, "request", "Write a Python program").await;
    assert_eq!(retry["admission"]["status"], "unacceptable");
    assert!(retry["run_id"].is_null());
    assert!(app.store.context(&sid).await.unwrap().is_empty());
    assert!(app.store.get(&format!("run:{id}")).await.unwrap().is_none());
    assert_eq!(calls.lock().await.len(), 1);
    let captured = calls.lock().await;
    assert!(captured[0]["tools"].as_array().is_none_or(|v| v.is_empty()));
    assert!(captured[0]["instructions"]
        .as_str()
        .unwrap()
        .contains("Only accept"));
    assert!(captured[0]["input"].to_string().contains("Write a Python"));
    drop(captured);
    let snapshot = crate::snapshot(State(app.clone()), Path(sid.clone()))
        .await
        .unwrap()
        .0;
    assert_eq!(
        snapshot["messages"][0]["admission"]["status"],
        "unacceptable"
    );
    assert!(!snapshot.to_string().contains("Only accept questions"));
    // Turning the gate off never admits a previously rejected message.
    settings(&app, false).await;
    submit(&app, &sid, "later", "What sizes do you have?").await;
    let context = app.store.context(&sid).await.unwrap();
    assert_eq!(context.len(), 1);
    assert_eq!(context[0]["content"], "What sizes do you have?");
    server.abort();
}
#[tokio::test]
async fn acceptable_queues_once_and_publishes_one_terminal_reply() {
    let (_root, mut app) = crate::runtime_tests::fixture().await;
    settings(&app, true).await;
    let (server, calls) = provider(&mut app, r#"{"verdict":"acceptable"}"#, false).await;
    let sid = session(&app).await;
    let result = submit(&app, &sid, "request", "What sizes do you have?").await;
    let id = result["review_id"].as_str().unwrap().to_owned();
    assert_eq!(
        app.store
            .get(&format!("session:{sid}"))
            .await
            .unwrap()
            .unwrap()["auto_naming"]["stage"],
        "awaiting_message"
    );
    super::scheduler::process(app.clone(), id.clone())
        .await
        .unwrap();
    super::scheduler::process(app.clone(), id.clone())
        .await
        .unwrap();
    let named = app
        .store
        .get(&format!("session:{sid}"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(named["title"], "New: What sizes do you have?");
    assert_eq!(named["auto_naming"]["stage"], "waiting_turn");
    assert_eq!(app.store.context(&sid).await.unwrap().len(), 1);
    assert_eq!(app.store.active_runs().await.unwrap().len(), 1);
    let retry = submit(&app, &sid, "request", "What sizes do you have?").await;
    assert_eq!(retry["run_id"], id);
    assert_eq!(calls.lock().await.len(), 1);
    let mut run = app.run(&id).await.unwrap();
    assert!(app
        .store
        .claim_run(&mut run, "fixture-loop", "manager-accepted")
        .await
        .unwrap());
    crate::execute_run(
        app.clone(),
        id.clone(),
        "manager-accepted".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(app.run(&id).await.unwrap()["status"], "completed");
    let messages = app.store.chat_snapshot(&sid).await.unwrap().0;
    assert_eq!(
        messages.iter().filter(|m| m["role"] == "assistant").count(),
        1
    );
    assert_eq!(messages.len(), 2);
    assert_eq!(calls.lock().await.len(), 2);
    server.abort();
}
#[tokio::test]
async fn malformed_verdict_and_changed_criteria_never_start_agent() {
    for (answer, change) in [("acceptable", false), (r#"{"verdict":"acceptable"}"#, true)] {
        let (_root, mut app) = crate::runtime_tests::fixture().await;
        settings(&app, true).await;
        let (server, calls) = provider(&mut app, answer, change).await;
        let sid = session(&app).await;
        let result = submit(&app, &sid, "request", "What sizes?").await;
        let id = result["review_id"].as_str().unwrap().to_owned();
        let copy = app.clone();
        let review_id = id.clone();
        let task = tokio::spawn(async move {
            super::scheduler::process(copy, review_id).await.unwrap();
        });
        if change {
            for _ in 0..100 {
                if !calls.lock().await.is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            settings(&app, false).await;
            // Restoring identical criteria still withdraws the older review revision.
            settings(&app, true).await;
        }
        task.await.unwrap();
        assert_eq!(
            app.store
                .get(&format!("manager_review:{id}"))
                .await
                .unwrap()
                .unwrap()["status"],
            "review_error"
        );
        assert!(app.store.context(&sid).await.unwrap().is_empty());
        assert!(app.store.get(&format!("run:{id}")).await.unwrap().is_none());
        server.abort();
    }
}
#[tokio::test]
async fn interrupted_claim_expires_without_another_provider_call() {
    let (_root, mut app) = crate::runtime_tests::fixture().await;
    settings(&app, true).await;
    let (server, calls) = provider(&mut app, r#"{"verdict":"acceptable"}"#, false).await;
    let sid = session(&app).await;
    let result = submit(&app, &sid, "request", "Sizes?").await;
    let id = result["review_id"].as_str().unwrap();
    let mut record = app.store.claim_manager_review(id).await.unwrap().unwrap();
    record["deadline"] = json!(0);
    app.store
        .put(&format!("manager_review:{id}"), &record)
        .await
        .unwrap();
    let task = tokio::spawn(super::scheduler(app.clone()));
    for _ in 0..100 {
        if app
            .store
            .get(&format!("manager_pending:{sid}"))
            .await
            .unwrap()
            .is_none()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    app.stop.cancel();
    task.await.unwrap();
    assert_eq!(
        app.store
            .get(&format!("manager_review:{id}"))
            .await
            .unwrap()
            .unwrap()["status"],
        "review_error"
    );
    assert!(calls.lock().await.is_empty());
    assert!(app.store.active_runs().await.unwrap().is_empty());
    server.abort();
}
