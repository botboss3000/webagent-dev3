//! Bounded ephemeral specialized-model calls. No audio or quick answers enter durable chat.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
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
                "Voice or image model did not complete; check its assignment and connection".into(),
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

/// Requests in one lane, released when the guard drops (also on a cancelled request).
struct InFlight(&'static AtomicUsize);
impl InFlight {
    /// Takes a slot when fewer than `limit` are in use.
    fn take(count: &'static AtomicUsize, limit: usize) -> Option<Self> {
        count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < limit).then_some(n + 1)
            })
            .ok()
            .map(|_| Self(count))
    }
}
impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The live scheduler limits (`config/engine/loop-runtime.json`); the defaults if the app is
/// not attached to the request. Takes the cloned handle: a borrowed request held across the
/// await would make the middleware future not Send.
async fn limits(app: Option<App>) -> (usize, usize) {
    let Some(app) = app else {
        return (4, 16);
    };
    let config = app.runtime.read().await;
    (
        config.scheduler.max_media_requests,
        config.scheduler.max_attachment_reads,
    )
}

fn busy() -> Response {
    ApiError(
        StatusCode::TOO_MANY_REQUESTS,
        "Media is busy; try again shortly".into(),
    )
    .into_response()
}

/// Admit `scheduler.max_media_requests` concurrent media HTTP requests without queueing excess
/// callers.
pub(crate) async fn limit(request: Request, next: Next) -> Response {
    static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
    let (max, _) = limits(request.extensions().get::<App>().cloned()).await;
    let user = request
        .extensions()
        .get::<Value>()
        .and_then(|identity| identity["_user_id"].as_str())
        .unwrap_or("")
        .to_owned();
    // One user may hold at most half the slots, so media stays available to others.
    let Some(_own) = UserSlot::take(&user, max.div_ceil(2).max(1)) else {
        return busy();
    };
    let Some(_slot) = InFlight::take(&IN_FLIGHT, max) else {
        return busy();
    };
    next.run(request).await
}

/// One user's in-flight media requests, released when the guard drops.
struct UserSlot(String);
static USER_SLOTS: std::sync::Mutex<Option<std::collections::HashMap<String, usize>>> =
    std::sync::Mutex::new(None);
impl UserSlot {
    fn take(user: &str, limit: usize) -> Option<Self> {
        let mut slots = USER_SLOTS.lock().unwrap_or_else(|p| p.into_inner());
        let count = slots
            .get_or_insert_with(Default::default)
            .entry(user.to_owned())
            .or_insert(0);
        if *count >= limit {
            return None;
        }
        *count += 1;
        Some(Self(user.to_owned()))
    }
}
impl Drop for UserSlot {
    fn drop(&mut self) {
        let mut slots = USER_SLOTS.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(map) = slots.as_mut() {
            if let Some(count) = map.get_mut(&self.0) {
                *count -= 1;
                if *count == 0 {
                    map.remove(&self.0);
                }
            }
        }
    }
}

/// Attachment reads (a strip of screenshot thumbnails loads together) wait up to ten seconds
/// for one of `scheduler.max_attachment_reads` slots instead of sharing the media-call slots
/// and failing at once.
pub(crate) async fn limit_reads(request: Request, next: Next) -> Response {
    static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
    let (_, max) = limits(request.extensions().get::<App>().cloned()).await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let _slot = loop {
        if let Some(slot) = InFlight::take(&IN_FLIGHT, max) {
            break slot;
        }
        if tokio::time::Instant::now() >= deadline {
            return busy();
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    next.run(request).await
}

#[cfg(test)]
mod tests {
    #[test]
    fn one_user_holds_at_most_its_share_of_media_slots() {
        let a = super::UserSlot::take("media-share-user", 2).unwrap();
        let _b = super::UserSlot::take("media-share-user", 2).unwrap();
        assert!(super::UserSlot::take("media-share-user", 2).is_none());
        assert!(super::UserSlot::take("media-share-other", 2).is_some());
        drop(a);
        assert!(super::UserSlot::take("media-share-user", 2).is_some());
    }
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
