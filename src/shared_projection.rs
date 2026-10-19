//! Caller-specific public views for one canonical shared conversation.
use crate::{json, shared_chat, App, Result, Value};
pub(crate) fn observer(app: &App) -> bool {
    app.chat_access
        .as_ref()
        .is_some_and(|access| matches!(access.kind, shared_chat::AccessKind::Observer))
}
pub(crate) async fn messages(app: &App, rows: &mut [Value]) -> Result<()> {
    for row in rows {
        let rid = row["run_id"].as_str().unwrap_or("");
        let message_id = row["id"].as_str().unwrap_or("");
        let mut metadata = app.store.get(&format!("message_meta:{message_id}")).await?;
        if metadata.is_none() && row["role"] == "user" {
            metadata = app.store.get(&format!("message_meta:{rid}")).await?;
        }
        if let Some(meta) = metadata {
            row["agent_message"] = meta["agent_message"].clone();
            row["author_id"] = meta["author_id"].clone();
            row["author_name"] = meta["author_name"].clone();
            row["message_kind"] = meta
                .get("message_kind")
                .or_else(|| meta.get("kind"))
                .cloned()
                .unwrap_or(Value::Null);
            if row["message_kind"] == "participant_joined" {
                row["role"] = json!("system");
            }
            if row["author_name"].is_null() {
                if let Some(author) = row["author_id"].as_str() {
                    let profile = app.store.user_profile(author).await?.unwrap_or(json!({}));
                    row["author_name"] = profile["display_name"].clone();
                }
            }
        } else if row["role"] == "user" {
            row["author_id"] = json!(app.store.current_user_id());
        }
    }
    Ok(())
}
pub(crate) async fn runs(app: &App, rows: &mut [Value]) -> Result<()> {
    for row in rows {
        let Some(id) = row["id"].as_str() else {
            continue;
        };
        let Some(run) = app.store.get(&format!("run:{id}")).await? else {
            continue;
        };
        let owner = run
            .pointer("/pending/authorization/owner_user_id")
            .and_then(Value::as_str)
            .or_else(|| run["requester_user_id"].as_str())
            .unwrap_or(app.store.current_user_id());
        let can_decide = !observer(app) && owner == shared_chat::actor(app);
        row["can_decide"] = json!(can_decide);
        if !can_decide {
            row["pending"] = if row["status"] == "waiting_approval" {
                json!({"state":"waiting","authorization_required":true,"can_decide":false})
            } else {
                Value::Null
            };
            // Raw errors may describe private tool inputs.
            row["error"] = Value::Null;
        }
        if observer(app) {
            row["attachments"] = json!([]);
        }
    }
    Ok(())
}
