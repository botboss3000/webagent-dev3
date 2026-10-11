//! Public route and cross-process cancellation regressions.
use crate::*;
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test]
async fn recycled_and_missing_routes_return_the_same_rejection() {
    let (_root,app)=runtime_tests::fixture().await;
    let sid=uuid();
    app.store.put(&key("session",&sid),&json!({"id":sid,"agent_id":"shared_default"})).await.unwrap();
    app.store.lifecycle_session(&sid,&store::session::SessionLifecycle {client_request_id:"recycle".into(),expected_revision:0},false).await.unwrap();
    let router=user_api::router().with_state(app.clone()).layer(axum::Extension(app.clone()))
        .layer(axum::Extension(json!({"expires":now()+60})));
    for suffix in ["","/events","/mode","/abilities","/controls"] {
        let response=router.clone().oneshot(Request::builder().uri(format!("/sessions/{sid}{suffix}")).body(axum::body::Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(),StatusCode::NOT_FOUND,"{suffix}");
        let bytes=response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap()["error"],"Session not found");
    }
    let response=router.oneshot(Request::builder().method("POST").uri(format!("/sessions/{sid}/messages"))
        .header("content-type","application/json").body(axum::body::Body::from(r#"{"text":"late","client_message_id":"late"}"#)).unwrap()).await.unwrap();
    assert_eq!(response.status(),StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn result_ready_before_poll_is_withheld_after_recycle_restore() {
    let (_root,app)=runtime_tests::fixture().await;
    let sid=uuid();
    app.store.put(&key("session",&sid),&json!({"id":sid})).await.unwrap();
    let result=app.session_operation(&sid,CancellationToken::new(),async {
        app.store.lifecycle_session(&sid,&store::session::SessionLifecycle {client_request_id:"recycle".into(),expected_revision:0},false).await?;
        app.store.lifecycle_session(&sid,&store::session::SessionLifecycle {client_request_id:"restore".into(),expected_revision:1},true).await?;
        Ok("late private output")
    }).await;
    assert_eq!(result.unwrap_err().to_string(),"Session not found");
}

#[tokio::test]
async fn session_epoch_cancels_work_even_after_immediate_restore() {
    let (_root,app)=runtime_tests::fixture().await;
    let sid=uuid();
    app.store.put(&key("session",&sid),&json!({"id":sid})).await.unwrap();
    let cancel=CancellationToken::new();
    let (started_tx,started_rx)=tokio::sync::oneshot::channel();
    let task_app=app.clone();let task_sid=sid.clone();let operation_cancel=cancel.clone();
    let work=tokio::spawn(async move {
        task_app.session_operation(&task_sid,operation_cancel.clone(),async move {
            let _=started_tx.send(());
            operation_cancel.cancelled().await;
            Ok(())
        }).await
    });
    started_rx.await.unwrap();
    app.store.lifecycle_session(&sid,&store::session::SessionLifecycle {client_request_id:"recycle".into(),expected_revision:0},false).await.unwrap();
    app.store.lifecycle_session(&sid,&store::session::SessionLifecycle {client_request_id:"restore".into(),expected_revision:1},true).await.unwrap();
    let error=tokio::time::timeout(Duration::from_secs(2),work).await.unwrap().unwrap().unwrap_err();
    assert!(cancel.is_cancelled());
    assert_eq!(error.to_string(),"Session not found");
}
