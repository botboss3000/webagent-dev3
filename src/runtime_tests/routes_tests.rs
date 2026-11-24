//! Runtime acceptance tests: Authenticated HTTP routes: auth, CSRF, sessions, messaging, diagnostics and state views.
use super::*;

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
        // Per-caller JSON with an ETag: the browser may keep it but must revalidate each use.
        assert_eq!(response.headers()["cache-control"], "private, no-cache");
        assert!(response.headers().contains_key("etag"));
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

#[tokio::test]
#[cfg(feature = "administrator")]
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
    set_tool_permission(&app, "source_review", access::Permission::Deny).await;
    let response = router(app.clone())
        .oneshot(request(&uri, Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    set_tool_permission(&app, "source_review", access::Permission::Ask).await;
    let response = router(app.clone())
        .oneshot(request(&uri, Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    set_tool_permission(&app, "source_review", access::Permission::Auto).await;
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
    let chunk = tokio::time::timeout(TEST_TIMEOUT, stream.next())
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
async fn delta_snapshot_sends_new_and_recent_messages_only_for_the_held_revision() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    for index in 0..8_i64 {
        sqlx::query(
            "INSERT INTO chat(id,session_id,run_id,role,content,created_at) VALUES(?,?,?,?,?,?)",
        )
        .bind(uuid())
        .bind(&sid)
        .bind("run-test")
        .bind("user")
        .bind(app.store.seal(&json!(format!("message {index}"))).unwrap())
        .bind(index)
        .execute(&app.store.pool)
        .await
        .unwrap();
    }
    let (all, last) = app.store.chat_snapshot(&sid).await.unwrap();
    let seqs = |messages: &[Value]| {
        messages
            .iter()
            .map(|m| m["seq"].as_i64().unwrap())
            .collect::<Vec<_>>()
    };
    let all_seqs = seqs(&all);
    // After the third message, with the newest 2 always resent: messages 4..8.
    let (delta, delta_last) = app
        .store
        .chat_snapshot_since(&sid, all_seqs[2], 2)
        .await
        .unwrap();
    assert_eq!(seqs(&delta), all_seqs[3..].to_vec());
    assert_eq!(delta_last, last);
    // Nothing new: only the newest 2.
    let (recent, _) = app.store.chat_snapshot_since(&sid, last, 2).await.unwrap();
    assert_eq!(seqs(&recent), all_seqs[6..].to_vec());
    // The handler uses the delta only for the history revision the client holds.
    let revision = app.store.get(&key("session", &sid)).await.unwrap().unwrap()["history_revision"]
        .as_u64()
        .unwrap_or(0);
    let held = SnapshotArgs {
        since: Some(last),
        history_revision: Some(revision),
    };
    let Json(body) = snapshot_query(State(app.clone()), Path(sid.clone()), Query(held))
        .await
        .unwrap();
    assert_eq!(body["delta"], true);
    let stale = SnapshotArgs {
        since: Some(last),
        history_revision: Some(revision + 1),
    };
    let Json(body) = snapshot_query(State(app.clone()), Path(sid.clone()), Query(stale))
        .await
        .unwrap();
    assert_eq!(body["delta"], false);
    assert_eq!(body["messages"].as_array().unwrap().len(), 8);
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
    set_tool_permission(&app, "state_inspect", access::Permission::Deny).await;
    let response = router(app.clone())
        .oneshot(request(Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
#[tokio::test]
async fn session_metadata_routes_enforce_auth_csrf_ownership_and_retries() {
    use axum::http::Method;
    use webagent_engine::ports::ChatEffects;
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let mut tx = app.store.pool.begin().await.unwrap();
    app.store
        .insert_chat(&mut tx, &sid, "fixture", "user", &json!("hello"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
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
    // List rows add the newest run's live status to the session summary.
    let mut listed = list["sessions"][0].clone();
    assert!(listed["run_status"].is_null());
    listed.as_object_mut().unwrap().remove("run_status");
    assert_eq!(listed, result["session"]);
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
            .body(Body::from(json!({"mode":mode,"expected_revision":if mode=="auto"{Some(2)}else{None},"confirm_write":mode=="auto"}).to_string()))
            .unwrap()
    };
    let response = router(app.clone()).oneshot(get(None, &uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = router(app.clone())
        .oneshot(put(None, None, "plan"))
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
        .oneshot(put(Some(&cookie), None, "plan"))
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
    assert_eq!(body["execution_mode"], "ask");
    assert_eq!(body["revision"], 0);

    for (mode, revision) in [("plan", 1), ("ask", 2), ("auto", 3)] {
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
        assert_eq!(body, json!({"execution_mode":mode,"revision":revision}));
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
        workflow_policy_set(State(app.clone()), Json(json!({"max_attempts":4})))
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
async fn issues_route_aggregates_the_conversation_tree() {
    let (_root, app) = fixture().await;
    let root = conversation(&app).await;
    let child = conversation(&app).await;
    let unrelated = conversation(&app).await;
    app.store
        .put(
            &format!("core_spawn_origin:{child}"),
            &json!({"parent_session_id":root,"root_session_id":root,"depth":1}),
        )
        .await
        .unwrap();
    let sentinel = "secret argument 4471";
    for sid in [&root, &child, &unrelated] {
        let id = queued(&app, sid).await;
        let mut run = app.run(&id).await.unwrap();
        run["pending"] = json!({"name":"browser_read","arguments":{"selector":sentinel}});
        webagent_engine::runtime::health::record_result(
            &mut run,
            &json!({"status":"error","error_kind":"timeout","message":format!("Timed out: {sentinel}")}),
        );
        app.save(&mut run).await.unwrap();
    }
    let cookie = ability_auth(&app).await;
    let read = |sid: String| {
        let app = app.clone();
        let cookie = cookie.clone();
        async move {
            let request = Request::builder()
                .uri(format!("/api/v3/sessions/{sid}/issues"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap();
            let response = router(app).oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap();
            (status, String::from_utf8_lossy(&bytes).into_owned())
        }
    };
    for sid in [root.clone(), child.clone()] {
        let (status, body) = read(sid).await;
        assert_eq!(status, StatusCode::OK);
        let view: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(view["totals"]["runs"], 2, "{body}");
        assert_eq!(view["totals"]["sessions"], 2);
        assert_eq!(view["groups"][0]["tool"], "browser_read");
        assert_eq!(view["groups"][0]["count"], 2);
        assert!(!body.contains(sentinel));
    }
    let (_, body) = read(unrelated).await;
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["totals"]["runs"],
        1
    );
    let (status, _) = read(crate::uuid()).await;
    assert_ne!(status, StatusCode::OK);
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
    assert_eq!(body["repeat_guard"], json!([]));
    assert_eq!(body.as_object().unwrap().len(), 12);
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
            app.store.full_user_tier().await,
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
            app.store.full_user_tier().await,
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
