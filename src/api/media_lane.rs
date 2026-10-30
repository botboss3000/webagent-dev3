//! Bounded ephemeral specialized-model calls. No audio or quick answers enter durable chat.
use super::*;
use webagent_engine::runtime::lane::{LaneError, LaneStatus};
/// Public status and message for an Engine lane rejection.
fn api_error(error: LaneError) -> ApiError {
    match error {
        LaneError::Rejected(status, message) => ApiError(
            match status {
                LaneStatus::BadRequest => StatusCode::BAD_REQUEST,
                LaneStatus::NotFound => StatusCode::NOT_FOUND,
                LaneStatus::Conflict => StatusCode::CONFLICT,
                LaneStatus::GatewayTimeout => StatusCode::GATEWAY_TIMEOUT,
                LaneStatus::BadGateway => StatusCode::BAD_GATEWAY,
            },
            message.into(),
        ),
        LaneError::Dispatch(error)
            if error
                .downcast_ref::<crate::store::session::ArchivedSession>()
                .is_none() =>
        {
            ApiError(
                StatusCode::BAD_GATEWAY,
                "Voice or image model did not complete; check its assignment and connection"
                    .into(),
            )
        }
        LaneError::Dispatch(error) | LaneError::Internal(error) => error.into(),
    }
}
/// Pin the requested media role to an active session's execution epoch and agent.
pub(crate) async fn lane_run(
    app: &App,
    sid: &str,
    role: model_selection::ModelRole,
) -> Result<Value, ApiError> {
    app.engine().lane_run(sid, role).await.map_err(api_error)
}
/// Recheck the lane's session and resolve its current candidate and credentials.
pub(crate) async fn lane_selection(
    app: &App,
    run: &Value,
) -> Result<(provider::ProviderSelection, Value), ApiError> {
    app.engine().lane_selection(run).await.map_err(api_error)
}
/// Return bounded private media text; see `Runtime::lane_answer`.
pub(crate) async fn lane_answer(
    app: &App,
    run: &Value,
    input: Vec<Value>,
    instructions: &str,
) -> Result<String, ApiError> {
    app.engine()
        .lane_answer(run, input, instructions)
        .await
        .map_err(api_error)
}

/// Admit at most four concurrent media HTTP requests without queueing excess callers.
pub(crate) async fn limit(request: Request, next: Next) -> Response {
    static SLOTS: std::sync::OnceLock<Semaphore> = std::sync::OnceLock::new();
    let Ok(_permit) = SLOTS.get_or_init(|| Semaphore::new(4)).try_acquire() else {
        return ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Media is busy; try again shortly".into(),
        )
        .into_response();
    };
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::post, Router};

    /// Capture a real local media call to verify exact input/instructions and no durable message, context or agent-run publication.
    #[tokio::test]
    async fn typed_media_call_preserves_request_and_does_not_publish_chat() {
        let (_root, mut app) = crate::runtime_tests::fixture().await;
        let mut catalog = app.store.model_catalog().await.unwrap();
        let roster = catalog.agent_models.get_mut("shared_default").unwrap();
        let assignment = roster.roles[&model_selection::ModelRole::Standard].clone();
        roster
            .roles
            .insert(model_selection::ModelRole::Voice, assignment.clone());
        catalog
            .entries
            .get_mut(&assignment.entry_id)
            .unwrap()
            .capabilities
            .insert("voice".into());
        app.store
            .save_model_catalog(&catalog, catalog.revision)
            .await
            .unwrap();
        let sid = crate::create_session(
            State(app.clone()),
            Json(json!({"client_request_id":uuid(),"agent_id":"shared_default"})),
        )
        .await
        .unwrap()
        .0["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let captured = calls.clone();
        let mock = Router::new().route(
            "/responses",
            post(move |Json(body): Json<Value>| {
                let captured = captured.clone();
                async move {
                    captured.lock().await.push(body);
                    let event = json!({"type":"response.completed","response":{
                    "status":"completed","output":[{"type":"message","role":"assistant",
                    "content":[{"type":"output_text","text":"A bounded voice answer."}]}]}});
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
        let run = lane_run(&app, &sid, model_selection::ModelRole::Voice)
            .await
            .unwrap();
        let input = vec![json!({"role":"user","content":"What size is available?"})];
        let answer = lane_answer(&app, &run, input.clone(), "Answer briefly.")
            .await
            .unwrap();
        assert_eq!(answer, "A bounded voice answer.");
        let calls = calls.lock().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["input"], json!(input));
        assert_eq!(calls[0]["instructions"], "Answer briefly.");
        assert!(calls[0].get("tools").is_none());
        assert!(app.store.chat_snapshot(&sid).await.unwrap().0.is_empty());
        assert!(app.store.context(&sid).await.unwrap().is_empty());
        assert!(app.store.active_runs().await.unwrap().is_empty());
        server.abort();
    }
}
