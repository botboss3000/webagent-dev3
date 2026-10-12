//! Caller-scoped undo/fork; source history never grants runtime authority.
use super::*;
use crate::store::{conversation_history::HistoryRequest, session::SessionPatchOutcome};
async fn register_fork(app: &App, result: &Value) -> Result<(), ApiError> {
    let child = result["session_id"].as_str().ok_or_else(|| bad("Fork result unavailable"))?;
    let agent = result["agent_id"].as_str().ok_or_else(|| bad("Fork agent unavailable"))?;
    app.store.register_shared_session(app.store.current_user_id(),child,agent).await?;
    Ok(())
}
async fn change(app: App, id: String, body: HistoryRequest, fork: bool) -> Api {
    let _guard = app.gate.lock().await;
    let receipt_key = format!("receipt:session:history:{id}:{}", body.client_request_id);
    if let Some(receipt) = app.store.get(&receipt_key).await? {
        if receipt["digest"] != digest(&json!({"body":body,"fork":fork})) {
            return Err(conflict("Request identifier reused with different input"));
        }
        if fork { register_fork(&app,&receipt["result"]).await?; }
        return Ok(Json(receipt["result"].clone()));
    }
    let child = if fork {
        let session = app
            .store
            .get(&key("session", &id))
            .await?
            .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Session not found".into()))?;
        let prepared = super::sessions::prepare_session(
            &app,
            &json!({"agent_id":session["agent_id"],"title":"Fork"}),
        )
        .await?;
        let mode = prepared
            .docs
            .iter()
            .find(|(key, _)| key.starts_with("session_mode:"))
            .map(|(_, value)| value.clone())
            .ok_or_else(|| bad("Session mode unavailable"))?;
        let access = app
            .store
            .live_access(
                app.store.current_user_id(),
                session["agent_id"].as_str().unwrap_or("shared_default"),
            )
            .await?;
        Some((prepared.value, mode, access.max_sessions() as usize))
    } else {
        None
    };
    match app.store.conversation_history(&id, &body, child).await? {
        SessionPatchOutcome::Saved(result) => {
            if fork { register_fork(&app,&result).await?; }
            Ok(Json(result))
        },
        SessionPatchOutcome::NotFound => Err(ApiError(
            StatusCode::NOT_FOUND,
            "Message or session not found".into(),
        )),
        SessionPatchOutcome::Conflict(message) => Err(conflict(message)),
        SessionPatchOutcome::Invalid(message) => Err(bad(message)),
    }
}
pub(crate) async fn undo_session_api(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(body): Json<HistoryRequest>,
) -> Api {
    change(app, id, body, false).await
}
pub(crate) async fn fork_session_api(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(body): Json<HistoryRequest>,
) -> Api {
    change(app, id, body, true).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    #[tokio::test]
    async fn history_routes_require_auth_csrf_and_owned_session() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let token = crate::uuid();
        app.store
            .put(
                &format!("auth:{}", crate::digest(&json!(token))),
                &json!({"csrf":"fixture","expires":now()+3600,"generation":"fixture-generation"}),
            )
            .await
            .unwrap();
        let router = crate::router(app);
        for action in ["undo", "fork"] {
            let uri = format!("/api/v3/sessions/missing/{action}");
            let body=json!({"client_request_id":"history-test","message_id":"message","expected_last_seq":1}).to_string();
            let request = |cookie: bool, csrf: bool| {
                let mut builder = Request::builder()
                    .method("POST")
                    .uri(&uri)
                    .header("content-type", "application/json");
                if cookie {
                    builder = builder.header("cookie", format!("v3_session={token}"));
                }
                if csrf {
                    builder = builder.header("x-csrf-token", "fixture");
                }
                builder.body(Body::from(body.clone())).unwrap()
            };
            assert_eq!(
                router
                    .clone()
                    .oneshot(request(false, false))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(
                router
                    .clone()
                    .oneshot(request(true, false))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::FORBIDDEN
            );
            assert_eq!(
                router
                    .clone()
                    .oneshot(request(true, true))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::NOT_FOUND
            );
        }
    }
}
