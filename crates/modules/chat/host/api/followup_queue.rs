use super::*;
fn queue_error(e: anyhow::Error) -> ApiError {
    if e.to_string() == "Queued message not found" {
        ApiError(StatusCode::NOT_FOUND, e.to_string())
    } else {
        conflict(&e.to_string())
    }
}
pub(crate) async fn delivery(
    State(app): State<App>,
    Path((sid, id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Api {
    let request = required(&body, "client_request_id", 128)?;
    let revision = body["expected_revision"]
        .as_u64()
        .ok_or_else(|| bad("expected_revision required"))?;
    if !matches!(
        body["choice"].as_str(),
        Some("queue" | "steer" | "interrupt" | "cancel")
    ) {
        return Err(bad("Invalid delivery choice"));
    }
    if body.as_object().is_none_or(|m| {
        m.keys().any(|k| {
            !matches!(
                k.as_str(),
                "client_request_id" | "expected_revision" | "choice"
            )
        })
    }) {
        return Err(bad("Unknown delivery field"));
    }
    let _g = app.gate.lock().await;
    Ok(Json(
        app.store
            .followup_choice(&sid, Some(&id), revision, request, &body)
            .await
            .map_err(queue_error)?,
    ))
}
pub(crate) async fn resume(
    State(app): State<App>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    let request = required(&body, "client_request_id", 128)?;
    let revision = body["expected_revision"]
        .as_u64()
        .ok_or_else(|| bad("expected_revision required"))?;
    if body.as_object().is_none_or(|m| {
        m.keys()
            .any(|k| !matches!(k.as_str(), "client_request_id" | "expected_revision"))
    }) {
        return Err(bad("Unknown queue field"));
    }
    if app.shutoff().await? {
        return Err(conflict("Global shutoff is active"));
    }
    let _g = app.gate.lock().await;
    Ok(Json(
        app.store
            .followup_choice(&sid, None, revision, request, &body)
            .await
            .map_err(queue_error)?,
    ))
}
