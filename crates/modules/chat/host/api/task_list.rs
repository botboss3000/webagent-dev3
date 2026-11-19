//! The chat's task panel API: the session task list with its newest revisions, and the
//! user's edits, which the agent receives with its next model call. The session router has
//! already scoped `app` to the session and checked that the caller owns it.
use super::*;
use webagent_chat::task_list::{self as rules, List};

fn invalid(error: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, error.to_string())
}

/// The session's task list (`null` before the first write), its revision and the newest
/// revisions with their changes.
pub(crate) async fn read(app: &App, sid: &str) -> Api {
    let document = app.store.task_list_current(sid).await?;
    let history = app.store.task_list_history(sid).await?;
    Ok(Json(json!({
        "revision":document.as_ref().and_then(|d| d["revision"].as_u64()).unwrap_or(0),
        "list":document.map(|d| d["list"].clone()),
        "history":history,
    })))
}

/// The user's edit of one section and its steps, in `task_list_write`'s form, fenced to
/// `expected_revision` and retried safely under `request_id`.
pub(crate) async fn write(app: &App, sid: &str, body: &Value) -> Api {
    let request = required(body, "request_id", 64)?;
    let expected = body["expected_revision"]
        .as_u64()
        .ok_or_else(|| invalid("expected_revision is required"))?;
    let write: rules::Write = serde_json::from_value(json!({
        "section":body["section"],
        "steps":body.get("steps").cloned().unwrap_or_else(|| json!([])),
    }))
    .map_err(invalid)?;
    let list: List = match app.store.task_list_current(sid).await? {
        Some(document) => serde_json::from_value(document["list"].clone()).map_err(invalid)?,
        None => List::default(),
    };
    if list.revision != expected {
        return Err(conflict(&format!(
            "The task list is at revision {}; reload it",
            list.revision
        )));
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64);
    let (next, changes) = rules::apply(&list, write, now).map_err(invalid)?;
    let saved = app
        .store
        .task_list_user_write(sid, request, expected, &json!(next), &json!(changes))
        .await
        .map_err(|error| conflict(&error.to_string()))?;
    Ok(Json(saved))
}
