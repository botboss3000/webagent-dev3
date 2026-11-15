//! Navigation is a caller-bound request to the active browser, never arbitrary script or a URL.
use crate::{Call, NativeHost};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_host_core::{digest, now};

#[must_use]
pub fn owns(name: &str) -> bool {
    name == "set_app_view"
}
/// Tools this module executes; their schemas live in the ability's `tools/<tool>/tool.json`.
pub const TOOLS: &[&str] = &["set_app_view"];
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    view: String,
}
#[must_use]
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
/// Runs `set_app_view`; `name` is kept for a uniform family signature.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    _name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let q: Args = serde_json::from_value(args.clone())?;
    let target = destination(&q.view).ok_or_else(|| anyhow!("Unknown app destination"))?;
    let mut result = enqueue(host, call, "set_app_view", args, target, cancel).await?;
    result["output"]["view"] = json!(q.view);
    Ok(result)
}
/// Only fixed native handlers can request these caller-bound surfaces; no model URL is accepted.
pub async fn enqueue(
    host: &dyn NativeHost,
    call: Call<'_>,
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
    let sid = call.sid;
    let actor = host.actor();
    let id = format!("core_ui:{sid}");
    let _guard = host.gate().lock().await;
    let run = host.authorize(call, tool, args, &cancel).await?;
    let store = host.store();
    let mut saved = store.module_state(&id).await?;
    let request_id = digest(&json!([sid, call.run_id, call.pending_id]));
    if saved["state"]["request_id"] != request_id {
        saved["state"] = json!({"request_id":request_id,"destination":destination,"actor_id":actor,"agent_id":run["agent_id"],"session_id":sid,"created_at":now(),"expires_at":now()+120});
        store
            .module_cas(&id, saved["revision"].as_u64().unwrap_or(0), &saved)
            .await?;
    }
    Ok(
        json!({"status":"success","output":{"requested":true,"destination":destination,"delivery":"active_browser","expires_in_seconds":120}}),
    )
}
/// The caller's open app-view request in session `sid`, or null.
pub async fn projection(host: &dyn NativeHost, sid: &str) -> Result<Value> {
    let saved = host.store().module_state(&format!("core_ui:{sid}")).await?;
    // A saved or dismissed credential form leaves an empty state; its new revision
    // lets every open device clear the card.
    if saved["state"]
        .as_object()
        .is_some_and(|state| state.is_empty())
    {
        return Ok(json!({"revision":saved["revision"],"request":null}));
    }
    // Navigation requests expire; a credential form stays until saved, dismissed or replaced.
    let state = &saved["state"];
    if state["actor_id"] != host.actor()
        || (state["destination"] != "credential"
            && state["expires_at"].as_u64().unwrap_or(0) < now())
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
