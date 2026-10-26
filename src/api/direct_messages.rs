//! Atomic session creation and first-message admission for the authenticated caller.
use super::*;

pub(crate) async fn send(
    State(mut app): State<App>,
    Json(body): Json<Value>,
) -> std::result::Result<(StatusCode, Json<Value>), ApiError> {
    let object = body
        .as_object()
        .ok_or_else(|| bad("Message request must be an object"))?;
    if object.keys().any(|name| {
        !matches!(
            name.as_str(),
            "text"
                | "client_message_id"
                | "agent_id"
                | "title"
                | "attachment_ids"
                | "attachments"
                | "model_entry_id"
                | "model_role"
        )
    }) {
        return Err(bad("Unknown message request field"));
    }
    let request_id = required(&body, "client_message_id", 128)?;
    let _guard = app.gate.lock().await;
    let receipt_key = format!("receipt:direct_message:{request_id}");
    // Replay the immutable response before fresh capacity checks; authentication
    // already scoped this Store to the current caller.
    if let Some(receipt) = app.store.get(&receipt_key).await? {
        if receipt["digest"] != digest(&body) {
            return Err(conflict("Request identifier reused with different input"));
        }
        let sid=receipt["result"]["session"]["id"].as_str().ok_or_else(||bad("Invalid message receipt"))?;
        session_agent_id(&app,sid).await?;
        return Ok((StatusCode::ACCEPTED, Json(receipt["result"].clone())));
    }
    let mut message_body = body.clone();
    if !object.contains_key("text") {
        message_body["text"] = json!("");
    }
    let text = message_body["text"]
        .as_str()
        .ok_or_else(|| bad("text is required"))?;
    if text.len() > 65536 {
        return Err(bad("text has invalid length"));
    }
    // Session preparation performs admission and limits but writes nothing.
    // A partial admission keeps its requester reservation. Retrying the same
    // immutable request must address that same session and run reservation.
    use sha2::Digest;
    let hash = sha2::Sha256::digest(serde_json::to_vec(&json!({
        "domain":"direct-message-session-v1", "actor":app.store.current_user_id(),
        "request":request_id, "body":digest(&body)
    })).unwrap());
    let mut bytes=[0u8;16]; bytes.copy_from_slice(&hash[..16]);
    bytes[6]=(bytes[6]&0x0f)|0x50; bytes[8]=(bytes[8]&0x3f)|0x80;
    let sid=uuid::Uuid::from_bytes(bytes).to_string();
    let session = super::chat_sessions::prepare_session_id(&app, &body, Some(sid)).await?;
    let sid = session.result["id"].as_str().unwrap();
    let agent = session.result["agent_id"].as_str().unwrap();
    app.store = app.store.for_agent(agent).await?;
    let prepared =
        super::chat::prepare_message(&app, sid, &message_body, Some(&session.value)).await?;
    let mut result = prepared.result;
    result["session"] = session.result.clone();
    let mut docs = session.docs;
    docs.extend(prepared.docs);
    docs.push((receipt_key, json!({"digest":digest(&body),"result":result})));
    app.store
        .atomic_first_message(docs, sid, prepared.kind, &prepared.message, &session.result)
        .await
        .map_err(super::chat::admission_error)?;
    app.store.register_shared_session(app.store.current_user_id(),sid,agent).await?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn post(app: &App, body: Value) -> Value {
        let (code, Json(result)) = send(State(app.clone()), Json(body)).await.unwrap();
        assert_eq!(code, StatusCode::ACCEPTED);
        result
    }
    async fn stages(app: &App, sid: &str) -> Vec<String> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT type FROM notification_stage WHERE session_id=? ORDER BY source_seq",
        )
        .bind(sid)
        .fetch_all(&app.store.pool)
        .await
        .unwrap();
        rows
    }
    #[tokio::test]
    async fn atomic_retry_concurrency_and_notifications() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let body = json!({"client_message_id":"whole-operation","text":"Hello"});
        let (a, b) = tokio::join!(post(&app, body.clone()), post(&app, body.clone()));
        assert_eq!(a, b);
        let sid = a["session"]["id"].as_str().unwrap();
        assert_eq!(app.store.list("session:").await.unwrap().len(), 1);
        assert_eq!(app.store.recent_runs(Some(sid)).await.unwrap().len(), 1);
        assert_eq!(app.store.chat_snapshot(sid).await.unwrap().0.len(), 1);
        assert_eq!(
            stages(&app, sid).await,
            vec!["session.created", "message.received"]
        );
        assert_eq!(
            app.store
                .get(&format!("session:{sid}"))
                .await
                .unwrap()
                .unwrap()["auto_naming"]["stage"],
            "waiting_turn"
        );
        // Capacity changes after admission cannot break a successful retry.
        app.store
            .put("control", &json!({"shutoff":true}))
            .await
            .unwrap();
        let mut session = app
            .store
            .get(&format!("session:{sid}"))
            .await
            .unwrap()
            .unwrap();
        session["title"] = json!("Later automatic title");
        app.store
            .put(&format!("session:{sid}"), &session)
            .await
            .unwrap();
        assert_eq!(post(&app, body).await, a);
        let error = send(
            State(app.clone()),
            Json(json!({"client_message_id":"whole-operation","text":"Different"})),
        )
        .await
        .unwrap_err();
        assert_eq!(error.0, StatusCode::CONFLICT);
        assert_eq!(app.store.list("session:").await.unwrap().len(), 1);
        let mut account=app.store.account_by_id("admin").await.unwrap().unwrap();
        account["user_id"]=json!("other");account["username"]=json!("other");
        sqlx::query("INSERT INTO user_accounts(user_id,username,value) VALUES('other','other',?)")
            .bind(app.store.seal(&account).unwrap()).execute(&app.store.app_pool).await.unwrap();
        let other = app.store.for_user("other").await.unwrap();
        assert!(other
            .get("receipt:direct_message:whole-operation")
            .await
            .unwrap()
            .is_none());
    }
    #[tokio::test]
    async fn rejected_requests_leave_no_session_or_outbox() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        for (body, want) in [
            (
                json!({"client_message_id":"empty","text":""}),
                StatusCode::BAD_REQUEST,
            ),
            (
                json!({"client_message_id":"missing"}),
                StatusCode::BAD_REQUEST,
            ),
            (
                json!({"client_message_id":"bad-agent","text":"Hi","agent_id":"missing"}),
                StatusCode::FORBIDDEN,
            ),
            (
                json!({"client_message_id":"identity","text":"Hi","user_id":"other"}),
                StatusCode::BAD_REQUEST,
            ),
            (
                json!({"client_message_id":"bad-model","text":"Hi","model_entry_id":"missing"}),
                StatusCode::CONFLICT,
            ),
            (
                json!({"client_message_id":"bad-files","attachments":{}}),
                StatusCode::BAD_REQUEST,
            ),
            (
                json!({"client_message_id":"long","text":"a".repeat(65537)}),
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let error = send(State(app.clone()), Json(body)).await.unwrap_err();
            assert_eq!(error.0, want);
        }
        assert!(app.store.list("session:").await.unwrap().is_empty());
        assert!(app
            .store
            .list("receipt:direct_message:")
            .await
            .unwrap()
            .is_empty());
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM notification_stage")
            .fetch_one(&app.store.pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
    #[tokio::test]
    async fn inline_attachments_and_session_bound_uploads() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let a=post(&app,json!({"client_message_id":"inline","attachments":[{"name":"evidence.txt","media_type":"text/plain","text":"Source material"}],"title":"Chosen title"})).await;
        let sid = a["session"]["id"].as_str().unwrap();
        assert_eq!(a["attachments"].as_array().unwrap().len(), 1);
        assert_eq!(
            app.store
                .get(&format!("session:{sid}"))
                .await
                .unwrap()
                .unwrap()["auto_naming"]["stage"],
            "locked"
        );
        let aid = a["attachments"][0]["id"].as_str().unwrap();
        let error = send(
            State(app.clone()),
            Json(json!({"client_message_id":"foreign","text":"Hi","attachment_ids":[aid]})),
        )
        .await
        .unwrap_err();
        assert_eq!(error.0, StatusCode::NOT_FOUND);
        assert_eq!(app.store.list("session:").await.unwrap().len(), 1);
    }
    #[tokio::test]
    async fn session_queue_and_shutoff_limits() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let access = app
            .store
            .live_access("admin", "shared_default")
            .await
            .unwrap();
        let mut policy = app.store.read_access_policy().unwrap();
        policy["tiers"][&access.tier.id]["max_sessions"] = json!(0);
        std::fs::write(
            app.store.access_policy_path(),
            serde_json::to_vec(&policy).unwrap(),
        )
        .unwrap();
        let error = send(
            State(app.clone()),
            Json(json!({"client_message_id":"limit","text":"Hi"})),
        )
        .await
        .unwrap_err();
        assert_eq!(error.0, StatusCode::CONFLICT);
        assert_eq!(error.1, "Session limit reached");
        policy["tiers"][&access.tier.id]["max_sessions"] = json!(10);
        std::fs::write(
            app.store.access_policy_path(),
            serde_json::to_vec(&policy).unwrap(),
        )
        .unwrap();
        app.runtime.write().await.scheduler.max_unresolved_runs = 0;
        let error = send(
            State(app.clone()),
            Json(json!({"client_message_id":"queue","text":"Hi"})),
        )
        .await
        .unwrap_err();
        assert_eq!(error.0, StatusCode::CONFLICT);
        assert_eq!(error.1, "Run queue is full");
        app.store
            .put("control", &json!({"shutoff":true}))
            .await
            .unwrap();
        let error = send(
            State(app.clone()),
            Json(json!({"client_message_id":"stop","text":"Hi"})),
        )
        .await
        .unwrap_err();
        assert_eq!(error.0, StatusCode::CONFLICT);
        assert_eq!(error.1, "Global shutoff is active");
        assert!(app.store.list("session:").await.unwrap().is_empty());
    }
    #[tokio::test]
    async fn manager_review_is_preserved_and_retry_response_is_immutable() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let store = app.store.for_agent("shared_default").await.unwrap();
        let raw: Vec<u8> =
            sqlx::query_scalar("SELECT metadata FROM agents WHERE id='shared_default'")
                .fetch_one(&store.agent_pool)
                .await
                .unwrap();
        let mut metadata = store.unseal(&raw).unwrap();
        metadata["manager_incoming_message_revision"] = json!(1);
        metadata["manager_incoming_message"] =
            json!({"enabled":true,"criteria":"Review incoming questions."});
        sqlx::query("UPDATE agents SET metadata=? WHERE id='shared_default'")
            .bind(store.seal_agent(&metadata).unwrap())
            .execute(&store.agent_pool)
            .await
            .unwrap();
        let body = json!({"client_message_id":"review","text":"Hello"});
        let result = post(&app, body.clone()).await;
        assert!(result["run_id"].is_null());
        assert!(result["review_id"].is_string());
        let sid = result["session"]["id"].as_str().unwrap();
        assert!(app.store.recent_runs(Some(sid)).await.unwrap().is_empty());
        assert_eq!(
            stages(&app, sid).await,
            vec!["session.created", "message.received"]
        );
        let key = format!("manager_review:{}", result["review_id"].as_str().unwrap());
        let mut review = app.store.get(&key).await.unwrap().unwrap();
        review["status"] = json!("rejected");
        app.store.put(&key, &review).await.unwrap();
        assert_eq!(post(&app, body).await, result);
    }
    #[tokio::test]
    async fn storage_failure_rolls_back_session_receipt_and_notification() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let prepared = super::super::chat_sessions::prepare_session(&app, &json!({}))
            .await
            .unwrap();
        let sid = prepared.result["id"].as_str().unwrap();
        let mut docs = prepared.docs;
        docs.push((
            "receipt:direct_message:rollback".into(),
            json!({"digest":"fixture","result":{}}),
        ));
        // Invalid message fails after staging session creation.
        assert!(app
            .store
            .atomic_first_message(
                docs,
                sid,
                "message",
                &json!({"role":"user","content":"Hello"}),
                &prepared.result
            )
            .await
            .is_err());
        assert!(app
            .store
            .get(&format!("session:{sid}"))
            .await
            .unwrap()
            .is_none());
        assert!(app
            .store
            .get("receipt:direct_message:rollback")
            .await
            .unwrap()
            .is_none());
        assert!(stages(&app, sid).await.is_empty());
    }
    #[tokio::test]
    async fn route_enforces_cookie_and_csrf_and_preserves_two_step() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let token = uuid();
        let account = app.store.account_by_id("admin").await.unwrap().unwrap();
        app.store.put(&format!("auth:{}",digest(&json!(token))),&json!({"user_id":"admin","generation":account["generation"],"expires":now()+3600,"csrf":"fixture"})).await.unwrap();
        let router = crate::api::router(app.clone());
        for (cookie, csrf, want) in [
            (false, false, StatusCode::UNAUTHORIZED),
            (true, false, StatusCode::FORBIDDEN),
            (true, true, StatusCode::ACCEPTED),
        ] {
            let mut req = Request::builder()
                .method("POST")
                .uri("/api/v3/messages")
                .header("content-type", "application/json");
            if cookie {
                req = req.header("cookie", format!("v3_session={token}"));
            }
            if csrf {
                req = req.header("x-csrf-token", "fixture");
            }
            let response = router
                .clone()
                .oneshot(
                    req.body(Body::from(
                        json!({"client_message_id":"route","text":"Hi"}).to_string(),
                    ))
                    .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), want);
            if want == StatusCode::ACCEPTED {
                let value: Value = serde_json::from_slice(
                    &response.into_body().collect().await.unwrap().to_bytes(),
                )
                .unwrap();
                assert!(value["session"]["id"].is_string());
                assert!(value["run_id"].is_string());
            }
        }
        let session = crate::create_session(
            State(app.clone()),
            Json(json!({"client_request_id":"legacy","title":"Legacy"})),
        )
        .await
        .unwrap()
        .0;
        let body = json!({"client_message_id":"legacy","text":"Hello"});
        let first = crate::send(
            State(app.clone()),
            Path(session["id"].as_str().unwrap().into()),
            Json(body.clone()),
        )
        .await
        .unwrap()
        .1
         .0;
        let retry = crate::send(
            State(app.clone()),
            Path(session["id"].as_str().unwrap().into()),
            Json(body),
        )
        .await
        .unwrap()
        .1
         .0;
        assert_eq!(first, retry);
    }
}
