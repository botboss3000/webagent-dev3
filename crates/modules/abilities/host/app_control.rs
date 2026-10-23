//! Navigation is a caller-bound request to the active browser, never arbitrary script or a URL.
use crate::*;
use serde::Deserialize;
pub fn owns(name: &str) -> bool {
    name == "set_app_view"
}
pub fn schemas() -> Vec<Value> {
    vec![
        json!({"type":"function","name":"set_app_view","description":"Request a registered app destination in this caller's active conversation browser. Existing page permissions still apply. No browser means the request remains unapplied.",
 "parameters":{"type":"object","properties":{"view":{"type":"string","enum":["chat","files","agents","models","abilities","settings","automations","builds"]}},"required":["view"],"additionalProperties":false}}),
    ]
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    view: String,
}
pub fn destination(view: &str) -> Option<&'static str> {
    Some(match view {
        "chat" => "chat",
        "files" => "app:files",
        "agents" => "agents:config",
        "models" => "agents:model",
        "abilities" => "agents:abilities",
        "settings" => "app:settings",
        "automations" => "automation",
        "builds" => "builds",
        _ => return None,
    })
}
pub async fn execute(
    app: &App,
    sid: &str,
    rid: &str,
    generation: &str,
    pending: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let q: Args = serde_json::from_value(args.clone())?;
    let target = destination(&q.view).ok_or_else(|| anyhow!("Unknown app destination"))?;
    let mut result = enqueue(
        app,
        sid,
        rid,
        generation,
        pending,
        "set_app_view",
        args,
        target,
        cancel,
    )
    .await?;
    result["output"]["view"] = json!(q.view);
    Ok(result)
}
/// Only fixed native handlers can request these caller-bound surfaces; no model URL is accepted.
pub async fn enqueue(
    app: &App,
    sid: &str,
    rid: &str,
    generation: &str,
    pending: &str,
    tool: &str,
    args: &Value,
    destination: &str,
    cancel: CancellationToken,
) -> Result<Value> {
    if !matches!(
        destination,
        "chat"
            | "app:files"
            | "agents:config"
            | "agents:model"
            | "agents:abilities"
            | "app:settings"
            | "automation"
            | "builds"
            | "registration"
    ) {
        return Err(anyhow!("Unknown app destination"));
    }
    let actor = shared_chat::actor(app);
    let id = format!("core_ui:{sid}");
    let _guard = app.gate.lock().await;
    let run = super::core_authority::check(app, sid, rid, generation, pending, tool, args, &cancel)
        .await?;
    let mut saved = app.store.module_state(&id).await?;
    let request_id = digest(&json!([sid, rid, pending]));
    if saved["state"]["request_id"] != request_id {
        saved["state"] = json!({"request_id":request_id,"destination":destination,"actor_id":actor,"agent_id":run["agent_id"],"session_id":sid,"created_at":now(),"expires_at":now()+120});
        app.store
            .module_cas(&id, saved["revision"].as_u64().unwrap_or(0), &saved)
            .await?;
    }
    Ok(
        json!({"status":"success","output":{"requested":true,"destination":destination,"delivery":"active_browser","expires_in_seconds":120}}),
    )
}
pub async fn projection(app: &App, sid: &str) -> Result<Value> {
    let saved = app.store.module_state(&format!("core_ui:{sid}")).await?;
    if saved["state"]["actor_id"] != shared_chat::actor(app)
        || saved["state"]["expires_at"].as_u64().unwrap_or(0) < now()
    {
        return Ok(Value::Null);
    }
    Ok(json!({"revision":saved["revision"],"request":saved["state"]}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_registered_destinations_are_accepted() {
        for view in [
            "javascript:alert(1)",
            "https://example.invalid",
            "../settings",
            "",
        ] {
            assert!(destination(view).is_none());
        }
        assert_eq!(destination("files"), Some("app:files"));
    }
}
