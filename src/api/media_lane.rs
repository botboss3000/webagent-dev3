//! Bounded ephemeral specialized-model calls. No audio or quick answers enter durable chat.
use super::*;
use tokio::sync::mpsc;
/// Pin the requested media role to an active session's execution epoch and agent after checking shutdown/model availability.
pub(crate) async fn lane_run(
    app: &App,
    sid: &str,
    role: model_selection::ModelRole,
) -> Result<Value, ApiError> {
    if app.shutoff().await? {
        return Err(conflict("Global shutoff is active"));
    }
    let session = app
        .store
        .get(&key("session", sid))
        .await?
        .ok_or_else(|| bad("Session not found"))?;
    if session["status"] != "active" {
        return Err(ApiError(StatusCode::NOT_FOUND,"Session not found".into()));
    }
    let agent = session_agent_id(app, sid).await?;
    let mut plan = app
        .store
        .model_catalog()
        .await?
        .resolve_role(&agent, role)
        .map_err(|_| conflict("Configure and connect the required model role for this agent"))?;
    app.pin_model_plan(&mut plan)
        .await
        .map_err(|_| conflict("Required model is unavailable"))?;
    Ok(
        json!({"agent_id":agent,"session_id":sid,"session_execution_epoch":session["execution_epoch"].as_u64().unwrap_or(0),
        "selection_plan":plan,"failover_state":model_selection::FailoverState::default()}),
    )
}
/// Recheck session/agent identity and pinned epoch, then resolve the current candidate and its authorized credentials.
pub(crate) async fn lane_selection(
    app: &App,
    run: &Value,
) -> Result<(provider::ProviderSelection, Value), ApiError> {
    let sid=run["session_id"].as_str().unwrap_or("");
    session_agent_id(app,sid).await?;
    if let Some(epoch)=run["session_execution_epoch"].as_u64() {
        if !app.store.get(&key("session",sid)).await?.is_some_and(|s|s["execution_epoch"].as_u64().unwrap_or(0)==epoch) {
            return Err(ApiError(StatusCode::NOT_FOUND,"Session not found".into()));
        }
    }
    let candidate = app
        .run_candidate(run)
        .await
        .map_err(|_| conflict("Model access changed"))?
        .ok_or_else(|| conflict("Model unavailable"))?;
    let selection = provider::ProviderSelection { limits:candidate.limits.clone(),
        provider: candidate.transport.provider_id(),
        model: candidate.upstream_model,
    };
    let tokens = app
        .tokens_for(&selection, run)
        .await
        .map_err(|_| conflict("Connect the required model provider"))?;
    Ok((selection, tokens))
}
// @code-index {"version":1,"feature":"provider-projection","aliases":["provider formatting","pure provider component"],"purpose":"Ephemeral media caller prepares pure projection before session/model admission; Config: engine controls and provider descriptor fence dispatch. No media answer enters durable history.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"config/engine-components.json","pointers":["/disabled"]},{"path":"plugins/engine/provider-projection/component.json","pointers":["/interface_version","/implementation_version","/limits"]}],"opportunities":[]}}
/// Return bounded private media text under session-epoch supervision and a 90-second deadline.
/// Calls CallDraft::prepare before admission; fences current model/component authority and the first network poll, then discards deltas.
pub(crate) fn lane_answer<'a>(
    app: &'a App,
    run: &'a Value,
    input: Vec<Value>,
    instructions: &'a str,
) -> impl std::future::Future<Output = Result<String, ApiError>> + Send + 'a {
    // Keep the complete media state machine on the heap: voice/upload callers
    // otherwise embed both credential-selection futures and their dispatch fence.
    // Borrowing preserves session_operation cancellation without a detached task.
    Box::pin(async move {
        let (selection, tokens) = lane_selection(app, run).await?;
        let (tx, mut rx) = mpsc::channel(16);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let cancel=app.stop.child_token();
        let result = tokio::time::timeout(
            Duration::from_secs(90),
            app.session_operation(run["session_id"].as_str().unwrap_or(""),cancel.clone(),async {
                let selection = &selection;
                let tokens = &tokens;
                let cancel = cancel.clone();
                // Component IPC completes before the final session/model authority check.
                let engine_revision = crate::engine_components::catalog()?.revision.clone();
                        let engine_controls = crate::engine_components::controls(&app.store.root)?.revision;
                let prepared = crate::provider::CallDraft::new(
                    selection,
                    input,
                    vec![],
                    run["session_id"].as_str().unwrap(),
                    instructions,
                ).prepare(&app.store.root, &cancel).await?;
                let dispatch_guard = app.gate.lock().await;
                let (current_selection, _) = lane_selection(app, run).await
                    .map_err(|_| anyhow!("Model access changed"))?;
                if current_selection != *selection || app.shutoff().await? {
                    return Err(anyhow!("Model access changed"));
                }
                if cancel.is_cancelled()
                            || crate::engine_components::catalog()?.revision != engine_revision
                            || crate::engine_components::controls(&app.store.root)?.revision != engine_controls
                            || !crate::engine_components::component_available(
                                &app.store.root, crate::engine_components::PROJECTION)
                        {
                            return Err(anyhow!("Provider component or authority changed"));
                        }
                // Poll the actual transport once while authority writers remain fenced.
                        let mut completion = Box::pin(async {
                            let ready = prepared.bind_credentials(tokens, &cancel)?;
                            app.provider.dispatch(ready, tx, cancel).await
                        });
                        let mut immediate = None;
                        futures_util::future::poll_fn(|cx| {
                            if let std::task::Poll::Ready(result) =
                                std::future::Future::poll(completion.as_mut(), cx)
                            {
                                immediate = Some(result);
                            }
                            std::task::Poll::Ready(())
                        }).await;
                        drop(dispatch_guard);
                        match immediate {
                            Some(result) => result,
                            None => completion.await,
                        }
            }),
        )
        .await;
        cancel.cancel();
        drain.abort();
        let result = result
            .map_err(|_| {
                ApiError(
                    StatusCode::GATEWAY_TIMEOUT,
                    "Voice or image model timed out".into(),
                )
            })?
            .map_err(|error| {
                if error.downcast_ref::<crate::store::session::RecycledSession>().is_some() {return ApiError::from(error);}
                ApiError(
                    StatusCode::BAD_GATEWAY,
                    "Voice or image model did not complete; check its assignment and connection".into(),
                )
            })?;
        lane_selection(app, run).await?;
        let answer = result["output"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| item["type"] == "message")
            .flat_map(|item| item["content"].as_array().into_iter().flatten())
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if answer.trim().is_empty() || answer.len() > 32768 {
            return Err(ApiError(
                StatusCode::BAD_GATEWAY,
                "Model returned no bounded answer".into(),
            ));
        }
        Ok(answer)
    })
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
