//! Runtime acceptance tests: Run lifecycle: queue, stops, recovery after restart, background work and late tool results.
use super::*;

// A new message replaces paused work and releases its held queue; uncertain work stays.
#[tokio::test]
async fn new_message_supersedes_paused_run_but_not_uncertain_work() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let (_, Json(first)) = send(
        State(app.clone()),
        Path(sid.clone()),
        Json(json!({"client_message_id":"one","text":"hello"})),
    )
    .await
    .unwrap();
    let paused_id = first["run_id"].as_str().unwrap().to_owned();
    let mut paused = app.run(&paused_id).await.unwrap();
    paused["status"] = json!("paused");
    paused["error"] = json!("Context preparation failed at fixture: invalid");
    app.save(&mut paused).await.unwrap();
    app.store
        .followup_hold(&sid, "Existing work must be resolved before queue delivery")
        .await
        .unwrap();
    let (_, Json(second)) = send(
        State(app.clone()),
        Path(sid.clone()),
        Json(json!({"client_message_id":"two","text":"continue"})),
    )
    .await
    .unwrap();
    let paused = app.run(&paused_id).await.unwrap();
    assert_eq!(paused["status"], "cancelled");
    assert_eq!(paused["superseded_by_message"], true);
    assert_eq!(app.store.followup_queue(&sid).await.unwrap()["held"], false);
    let next_id = second["run_id"]
        .as_str()
        .expect("message dispatched, not held");
    let mut uncertain = app.run(next_id).await.unwrap();
    uncertain["status"] = json!("paused");
    uncertain["uncertain"] = json!(true);
    app.save(&mut uncertain).await.unwrap();
    let _ = send(
        State(app.clone()),
        Path(sid.clone()),
        Json(json!({"client_message_id":"three","text":"again"})),
    )
    .await;
    assert_eq!(app.run(next_id).await.unwrap()["status"], "paused");
}

// With no work left, the user's next message is the resume for a held queue.
#[tokio::test]
async fn new_message_resumes_held_queue_after_run_ends() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let (_, Json(first)) = send(
        State(app.clone()),
        Path(sid.clone()),
        Json(json!({"client_message_id":"one","text":"hello"})),
    )
    .await
    .unwrap();
    let mut failed = app.run(first["run_id"].as_str().unwrap()).await.unwrap();
    failed["status"] = json!("failed");
    app.save(&mut failed).await.unwrap();
    app.store
        .followup_hold(
            &sid,
            "Previous run stopped or failed; resume the queue when ready",
        )
        .await
        .unwrap();
    let (_, Json(second)) = send(
        State(app.clone()),
        Path(sid.clone()),
        Json(json!({"client_message_id":"two","text":"carry on"})),
    )
    .await
    .unwrap();
    assert_eq!(app.store.followup_queue(&sid).await.unwrap()["held"], false);
    assert!(second["run_id"].is_string(), "message dispatched, not held");
}

// A bare stop phrase stops in-flight work without recording a message; idle sessions
// receive it as an ordinary message.
#[tokio::test]
async fn typed_stop_phrase_stops_running_work_without_a_message() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let (_, Json(first)) = send(
        State(app.clone()),
        Path(sid.clone()),
        Json(json!({"client_message_id":"one","text":"hello"})),
    )
    .await
    .unwrap();
    let run_id = first["run_id"].as_str().unwrap().to_owned();
    let body = json!({"client_message_id":"two","text":"Please stop."});
    for _ in 0..2 {
        let (_, Json(stopped)) = send(State(app.clone()), Path(sid.clone()), Json(body.clone()))
            .await
            .unwrap();
        assert_eq!(stopped["stopped_run"]["id"], run_id.as_str());
    }
    assert_eq!(app.run(&run_id).await.unwrap()["status"], "cancelled");
    assert_eq!(app.store.chat_snapshot(&sid).await.unwrap().0.len(), 1);
    let (_, Json(sent)) = send(
        State(app.clone()),
        Path(sid.clone()),
        Json(json!({"client_message_id":"three","text":"stop"})),
    )
    .await
    .unwrap();
    assert!(sent["stopped_run"].is_null());
    assert_eq!(app.store.chat_snapshot(&sid).await.unwrap().0.len(), 2);
}

#[tokio::test]
async fn recovery_settles_cut_off_writes_and_requeues_safe_reads() {
    let (root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    running(&app, &id, "first").await;
    let mut run = app.run(&id).await.unwrap();
    run["phase"] = json!("tools");
    run["pending"] = json!({"name":"file_write","state":"executing","call_id":"c1"});
    workflow::record(
        &mut run,
        &workflow::Policy::resolve(None).unwrap(),
        &checkpoint_pre_tool("file_write"),
        workflow::Decision::Pass,
    )
    .unwrap();
    let receipt = run["workflow"]["receipts"]["pre_tool"].clone();
    app.save(&mut run).await.unwrap();
    recover(&app, true).await.unwrap();
    // A cut-off write resumes with a result asking the agent to check it; no review card.
    let run = app.run(&id).await.unwrap();
    assert_eq!(run["status"], "queued");
    assert_eq!(run["uncertain"], false);
    assert!(run.to_string().contains("unexpected app restart"));
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
    assert_eq!(app.run(&id).await.unwrap()["status"], "queued");
    let reopened = Store::open(root.path()).await.unwrap();
    let durable = reopened.get(&key("run", &id)).await.unwrap().unwrap();
    assert_eq!(durable["workflow"]["receipts"]["pre_tool"], receipt);
}

// A context pause caused only by restart cancellation resumes at startup; a
// genuine context fault, a stale pause and uncertain work stay paused.
#[tokio::test]
async fn startup_requeues_restart_cancelled_context_pause_only() {
    let (_root, app) = fixture().await;
    let pause = |app: App, error: &'static str, uncertain: bool, age: u64| async move {
        let sid = conversation(&app).await;
        let id = queued(&app, &sid).await;
        let mut run = app.run(&id).await.unwrap();
        run["status"] = json!("paused");
        run["error"] = json!(error);
        run["uncertain"] = json!(uncertain);
        app.save(&mut run).await.unwrap();
        if age > 0 {
            let mut run = app.run(&id).await.unwrap();
            run["updated_at"] = json!(now() - age);
            app.store
                .save_run_override(&mut run, "run_status", &json!({"run_id":id}))
                .await
                .unwrap();
        }
        id
    };
    let cancelled = "Context preparation failed at provider_profile: Engine call cancelled";
    let resumable = pause(app.clone(), cancelled, false, 0).await;
    let fault = pause(
        app.clone(),
        "Context preparation failed at history: invalid",
        false,
        0,
    )
    .await;
    let uncertain = pause(app.clone(), cancelled, true, 0).await;
    let stale = pause(app.clone(), cancelled, false, 2 * 24 * 60 * 60).await;
    recover(&app, true).await.unwrap();
    let run = app.run(&resumable).await.unwrap();
    assert_eq!(run["status"], "queued");
    assert_eq!(run["error"], "Recovered after interruption");
    for id in [fault, uncertain, stale] {
        assert_eq!(app.run(&id).await.unwrap()["status"], "paused");
    }
}

#[tokio::test]
async fn build_waiting_run_survives_restart_while_its_model_is_unauthorized() {
    // A deploy restarts the loop under a run that is only following its build job;
    // reading the receipt needs no model, so recovery keeps following it.
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    running(&app, &id, "build-generation").await;
    let mut run = app.run(&id).await.unwrap();
    run["phase"] = json!("tools");
    run["selection_plan"] = Value::Null;
    run["pending"] = json!({"name":"v3_build_work","state":"executing",
        "call_id":"unauthorized-build-call","arguments":{"action":"finish",
            "task_id":"build-unauthorized-test","expected_inputs_sha256":"0".repeat(64)}});
    app.save(&mut run).await.unwrap();
    recover(&app, true).await.unwrap();
    let waiting = app.run(&id).await.unwrap();
    assert_eq!(waiting["status"], "waiting_external");
    assert!(waiting["error"].is_null());
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
async fn startup_resume_setting_stops_safe_runs_and_settled_writes() {
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
    write["pending"] = json!({"name":"file_write","state":"executing","call_id":"c1"});
    app.save(&mut write).await.unwrap();
    recover(&app, true).await.unwrap();
    let stopped = app.run(&read_id).await.unwrap();
    assert_eq!(stopped["status"], "stopped");
    assert_eq!(stopped["pending"]["state"], "requested");
    assert!(stopped["error"].as_str().unwrap().contains("disabled"));
    let settled = app.run(&write_id).await.unwrap();
    assert_eq!(settled["status"], "stopped");
    assert_eq!(settled["uncertain"], false);
    // Non-startup recovery leaves a running run alone while its lease is held by a live loop
    // (another loop process is executing it) and repairs it once the lease expired.
    sqlx::query("INSERT INTO loop_members(idx,n,owner,started_at,updated_at,serving) VALUES(1,2,'fixture-loop',unixepoch(),unixepoch(),1)")
        .execute(&app.store.app_pool)
        .await
        .unwrap();
    let sid3 = conversation(&app).await;
    let lease_id = queued(&app, &sid3).await;
    running(&app, &lease_id, "lease-generation").await;
    recover(&app, false).await.unwrap();
    assert_eq!(app.run(&lease_id).await.unwrap()["status"], "running");
    // Start-up recovery of loop 0 also leaves another live loop's run alone.
    recover(&app, true).await.unwrap();
    assert_eq!(app.run(&lease_id).await.unwrap()["status"], "running");
    sqlx::query("UPDATE run_index SET lease_expires_at=lease_expires_at-60 WHERE id=?")
        .bind(&lease_id)
        .execute(&app.store.pool)
        .await
        .unwrap();
    recover(&app, false).await.unwrap();
    assert_eq!(app.run(&lease_id).await.unwrap()["status"], "queued");
    // A run whose loop is no longer a live member is taken back before its lease lapses.
    let sid4 = conversation(&app).await;
    let orphan_id = queued(&app, &sid4).await;
    running(&app, &orphan_id, "orphan-generation").await;
    sqlx::query("UPDATE loop_members SET updated_at=updated_at-60")
        .execute(&app.store.app_pool)
        .await
        .unwrap();
    recover(&app, false).await.unwrap();
    assert_eq!(app.run(&orphan_id).await.unwrap()["status"], "queued");
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
    for (name, expected) in [("file_read", "stopped"), ("file_write", "stopped")] {
        let sid = conversation(&app).await;
        let id = queued(&app, &sid).await;
        running(&app, &id, "corrupt-config-generation").await;
        let mut run = app.run(&id).await.unwrap();
        run["pending"] = json!({"name":name,"state":"executing","call_id":"c1"});
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

/// "Send now" cancels only the model call: the run continues with the message.
#[tokio::test]
async fn send_now_replaces_the_model_call_and_continues_the_run() {
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
                    let first = {
                        let mut requests = requests.lock().await;
                        requests.push(body);
                        requests.len() == 1
                    };
                    if first {
                        // Hold the first call open until the forced message replaces it.
                        tokio::time::sleep(Duration::from_secs(600)).await;
                    }
                    let output = json!([{"type":"message","role":"assistant",
                        "content":[{"type":"output_text","text":"Answered the new message."}]}]);
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
    running(&app, &id, "send-now").await;
    let cancel = CancellationToken::new();
    let task = tokio::spawn(execute_run(
        app.clone(),
        id.clone(),
        "send-now".into(),
        cancel.clone(),
    ));
    tokio::time::timeout(TEST_TIMEOUT, async {
        while requests.lock().await.is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("first model call must start");
    let entry = json!({"id":"forced","message_id":"user:forced","session_id":sid,
        "agent_id":"shared_default","revision":0,"choice":"steer","status":"pending",
        "review_status":"acceptable","content":"switch topic","context_content":"switch topic",
        "attachments":[]});
    app.store
        .followup_submit(&sid, entry, vec![], "forced", "digest", &json!({}))
        .await
        .unwrap();
    app.store
        .followup_choice(&sid, Some("forced"), 0, "now", &json!({"choice":"now"}))
        .await
        .unwrap();
    tokio::time::timeout(TEST_TIMEOUT, task)
        .await
        .expect("run must continue after send now")
        .unwrap()
        .unwrap();
    assert!(!cancel.is_cancelled());
    let run = app.run(&id).await.unwrap();
    assert_eq!(run["status"], "completed");
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert!(requests[1]["input"].to_string().contains("switch topic"));
    let q = app.store.followup_queue(&sid).await.unwrap();
    assert_eq!(q["entries"][0]["status"], "delivered");
    assert_eq!(q["entries"][0]["consuming_run_id"], json!(id));
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
    tokio::time::timeout(TEST_TIMEOUT, started.notified())
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
    tokio::time::timeout(TEST_TIMEOUT, executing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(app.run(&id).await.unwrap()["status"], "stopped");
    let (chat, _) = app.store.chat_snapshot(&sid).await.unwrap();
    // The stop leaves a run-status notice, but no published reply.
    let chat: Vec<_> = chat
        .into_iter()
        .filter(|row| row["role"] != "notice")
        .collect();
    assert_eq!(chat.len(), 1);
    assert_eq!(chat[0]["role"], "user");
    server.abort();
}

// Exercise shutdown during a real streaming dispatch, then reopen durable state.
#[tokio::test]
async fn runtime_restart_resumes_model_once_but_preserves_intentional_stops() {
    use axum::response::{sse::Event, Sse};
    for action in ["restart", "disabled", "cancel", "shutoff", "lease_cancel"] {
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
        tokio::time::timeout(TEST_TIMEOUT, started.notified())
            .await
            .unwrap();
        match action {
            "restart" | "disabled" => {
                app.stop.cancel();
            }
            "cancel" => {
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
        tokio::time::timeout(TEST_TIMEOUT, executing)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        app.active.lock().await.remove(&id);
        let interrupted = app.run(&id).await.unwrap();
        // The first step adds the session's starting set_execution_mode entry; everything
        // else in the input is unchanged by the interruption.
        assert_eq!(
            without_mode_entries(&interrupted["input"]),
            without_mode_entries(&original),
            "{action}"
        );
        let published = app.store.chat_snapshot(&sid).await.unwrap().0;
        assert_eq!(
            published
                .iter()
                .filter(|row| row["role"] != "notice")
                .count(),
            1,
            "{action}"
        );
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
    tokio::time::timeout(TEST_TIMEOUT, started.notified())
        .await
        .unwrap();
    app.stop.cancel();
    tokio::time::timeout(TEST_TIMEOUT, executing)
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
        .any(|item| item["type"] == "function_call_output" && !mode_entry(item)));
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
async fn a_waiting_run_starts_as_soon_as_a_slot_frees() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let slot = app.slots.clone().acquire_owned().await.unwrap();
    let task = tokio::spawn(scheduler(app.clone()));
    // Passes while the only run slot is taken: the run waits and its account stays unchanged,
    // so later passes offer it from what the first pass found.
    for _ in 0..3 {
        crate::wake::notify(&app).await;
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
    assert_eq!(status(&app.run(&id).await.unwrap()), "queued");
    // A run ends elsewhere: its slot is given back and its end sends a hint. The waiting run
    // starts on that hint, well before the 5 s fallback pass.
    drop(slot);
    crate::wake::notify(&app).await;
    let freed = std::time::Instant::now();
    while status(&app.run(&id).await.unwrap()) == "queued" {
        assert!(
            freed.elapsed() < Duration::from_millis(2500),
            "the waiting run did not start when the slot freed"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    app.stop.cancel();
    task.await.unwrap();
}
#[tokio::test]
async fn background_read_overlaps_model_and_delivers_at_next_boundary() {
    background_execution_journey("overlap").await;
}

#[tokio::test]
async fn background_read_outliving_its_run_reports_as_a_waking_notice() {
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
        let (mode_revision, visibility, state) = ability_controls(&app, &sid).await.unwrap();
        let access = app
            .store
            .live_access(app.store.current_user_id(), "shared_default")
            .await
            .unwrap();
        let pending = json!({
            "id":"first-pending","name":"file_read","call_id":"stable-background-call",
            "arguments":args,"digest":digest(&args),
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
            0,
            &settings,
        )
        .await
        .unwrap();
        let task_id = handle["call_id"].as_str().unwrap();
        let original = app.store.background_get(task_id).await.unwrap().unwrap();
        set_tool_permission(&app, "file_read", access::Permission::Ask).await;
        let mut visibility = app.store.agent_abilities().await.unwrap();
        let revision = visibility["revision"].as_u64().unwrap();
        visibility["revision"] = json!(revision + 1);
        visibility["tool_execution"]["file_read"] = json!({
            "mode":"model","max_concurrency":1
        });
        app.store
            .save_agent_abilities(&visibility, revision)
            .await
            .unwrap();
        let (mode_revision, visibility, state) = ability_controls(&app, &sid).await.unwrap();
        let access = app
            .store
            .live_access(app.store.current_user_id(), "shared_default")
            .await
            .unwrap();
        run["generation"] = json!("reclaimed-background-generation");
        run["loaded_abilities"] = json!(["codebase_admin", "later-activation"]);
        let mut pending = json!({
            "id":"reclaimed-pending","name":"file_read","call_id":"stable-background-call",
            "arguments":args,"digest":digest(&args),
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
        let version = 0;
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
        set_tool_permission(&app, "file_read", access::Permission::Deny).await;
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

#[tokio::test]
#[cfg(feature = "administrator")]
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
        app.save(&mut run).await.unwrap();
        let views = app.store.recent_runs(Some(&sid)).await.unwrap();
        assert_eq!(views.len(), 1);
        assert!(views[0]["model"].is_object());
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

#[tokio::test]
async fn late_single_tool_failure_survives_stop_and_reopen() {
    for name in ["file_list", "file_write"] {
        late_single_tool_case(name, "cancel", "none", false).await;
    }
}

#[tokio::test]
async fn late_single_tool_rejects_stale_attempts_and_keeps_known_write_result() {
    for mutation in ["generation", "pending", "requested"] {
        late_single_tool_case("file_write", "cancel", mutation, false).await;
        late_single_tool_case("file_write", "cancel", mutation, true).await;
    }
    late_single_tool_case("file_write", "cancel", "none", true).await;
}

#[tokio::test]
#[cfg_attr(not(administrator_pack), ignore = "needs the Administrator pack")]
async fn late_single_tool_broker_finish_keeps_receipt_reconciliation() {
    late_single_tool_case("build_work", "cancel", "none", false).await;
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
        // "pause" is no longer a run action; a finished run rejects "cancel" as a conflict.
        let expected = if action == "pause" {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::CONFLICT
        };
        let error = command(
            State(app.clone()),
            Path((id.clone(), action.into())),
            Json(json!({"client_request_id":uuid()})),
        )
        .await
        .unwrap_err();
        assert_eq!(error.0, expected, "{action}");
    }
    assert_eq!(app.run(&id).await.unwrap()["status"], "completed");
}

#[tokio::test]
async fn recovery_sorts_calls_a_restart_left_executing() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    run["status"] = json!("recovery");
    run["uncertain"] = json!(true);
    run["batch"] = json!([
        {"state":"executing","pending":{"id":"p1","call_id":"c1","name":"shell","arguments":{"command":"x"}}},
        {"state":"executing","pending":{"id":"p2","call_id":"c2","name":"file_read","arguments":{}}}
    ]);
    app.save(&mut run).await.unwrap();
    // Low-risk writes and reads need no review: any decision resumes the run.
    command(
        State(app.clone()),
        Path((id.clone(), "recovery".into())),
        Json(json!({"client_request_id":"sort-executing","decision":"retry"})),
    )
    .await
    .unwrap();
    let run = app.run(&id).await.unwrap();
    assert_eq!(run["status"], "queued");
    assert_eq!(run["uncertain"], false);
    assert_eq!(run["batch"][0]["state"], "done");
    assert_eq!(run["batch"][0]["result"]["error_kind"], "outcome_unknown");
    assert_eq!(run["batch"][1]["state"], "requested");
}

#[tokio::test]
async fn a_loop_started_with_another_count_waits_until_the_other_loops_are_gone() {
    use webagent_engine::partition::{Member, MemberState};
    const SERVING: MemberState = MemberState::Serving;
    let (_root, app) = fixture().await;
    let single = Member::new(0, 1).unwrap();
    // A loop past the fence but still in its start-up recovery does not make readiness yet.
    assert!(app
        .store
        .beat_loop_member(single, "single", MemberState::Starting)
        .await
        .unwrap());
    assert_eq!(app.store.loops_ready().await.unwrap(), None);
    let (first, second) = (Member::new(0, 2).unwrap(), Member::new(1, 2).unwrap());
    assert!(app
        .store
        .beat_loop_member(single, "single", SERVING)
        .await
        .unwrap());
    assert_eq!(app.store.loops_ready().await.unwrap(), Some(1));
    // Loop 1 of 2 is refused while the single loop lives.
    assert!(!app
        .store
        .beat_loop_member(second, "pair-1", SERVING)
        .await
        .unwrap());
    assert_eq!(app.store.loops_ready().await.unwrap(), Some(1));
    // Loop 0 of 2 holds index 0's lock, so the single loop's row is a dead process's: replaced.
    assert!(app
        .store
        .beat_loop_member(first, "pair-0", SERVING)
        .await
        .unwrap());
    // Not ready until loop 1 of 2 is live too.
    assert_eq!(app.store.loops_ready().await.unwrap(), None);
    assert!(app
        .store
        .beat_loop_member(second, "pair-1", SERVING)
        .await
        .unwrap());
    assert_eq!(app.store.loops_ready().await.unwrap(), Some(2));
    let repository = app.store.engine_repository();
    assert_eq!(
        repository.live_loop_owners(Some(0)).await.unwrap(),
        vec!["pair-1".to_owned()]
    );
    // A one-loop process starting now is refused while loop 1 of 2 lives.
    assert!(!app
        .store
        .beat_loop_member(single, "single-again", SERVING)
        .await
        .unwrap());
    // Stale rows do not count: the loops stopped renewing more than 10 s ago.
    sqlx::query("UPDATE loop_members SET updated_at=updated_at-60")
        .execute(&app.store.app_pool)
        .await
        .unwrap();
    assert_eq!(app.store.loops_ready().await.unwrap(), None);
    assert!(repository.live_loop_owners(None).await.unwrap().is_empty());
    assert!(app
        .store
        .beat_loop_member(single, "single-again", SERVING)
        .await
        .unwrap());
    assert_eq!(app.store.loops_ready().await.unwrap(), Some(1));
    // Leaving at the end of a drain removes only the leaver's own row.
    app.store
        .leave_loop_members(single, "someone-else")
        .await
        .unwrap();
    assert_eq!(app.store.loops_ready().await.unwrap(), Some(1));
    app.store
        .leave_loop_members(single, "single-again")
        .await
        .unwrap();
    assert_eq!(app.store.loops_ready().await.unwrap(), None);
}

#[tokio::test]
async fn loop_liveness_does_not_wait_for_the_shared_application_pool() {
    let (_root, app) = fixture().await;
    // Every slot of the shared application pool is busy, as under heavy load.
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(app.store.app_pool.acquire().await.unwrap());
    }
    let limit = std::time::Duration::from_secs(10);
    let member = webagent_engine::partition::Member::default();
    let serving = webagent_engine::partition::MemberState::Serving;
    assert!(tokio::time::timeout(
        limit,
        app.store
            .beat_loop_member(member, "liveness-owner", serving)
    )
    .await
    .expect("heartbeat waited for the shared pool")
    .unwrap());
    assert_eq!(
        tokio::time::timeout(limit, app.store.loops_ready())
            .await
            .expect("readiness waited for the shared pool")
            .unwrap(),
        Some(1)
    );
    drop(held);
}
