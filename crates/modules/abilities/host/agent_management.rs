//! Agent Management runs as an ability plugin (`plugins/abilities/Core/agent_management`)
//! through `crate::extension_plugins`. This file keeps its tool list for schemas and the
//! startup check, and the route bridge its host requests (and Orchestration) go through.
use crate::{api, shared_chat, App, CancellationToken};
use anyhow::{anyhow, Result};
use axum::{body::Body, Extension};
use serde_json::{json, Value};
use tower::ServiceExt;

/// Tool folders of the ability; their schemas live in `tools/<tool>/tool.json`.
/// `kick_user_session` is served by Chat, the rest by the plugin.
pub const TOOLS: &[&str] = &[
    "list_agent_templates",
    "list_my_agents",
    "get_agent",
    "list_agent_tools",
    "create_agent",
    "update_agent",
    "set_agent_tool",
    "edit_agent_prompt",
    "set_agent_ability",
    "manage_agent_skills",
    "manage_agent_chat_config",
    "list_user_sessions",
    "manage_user_session",
    "kick_user_session",
    "watch_sessions",
    "create_user_session",
];

/// Whether the plugin serves `name`.
pub fn owns(name: &str) -> bool {
    name != "kick_user_session" && TOOLS.contains(&name)
}

pub fn schemas() -> Vec<Value> {
    crate::tools::folder_schemas(TOOLS)
}

/// Only fixed route callers use this bridge; no URL, method, identity or credential comes from model arguments.
pub async fn request(app: &App, method: &str, path: String, body: Value) -> Result<Value> {
    let actor = shared_chat::actor(app).to_owned();
    let mut caller = app.clone();
    caller.store = app.store.for_user(&actor).await?;
    caller.chat_access = None;
    caller.chat_actor = Some(actor.clone());
    let router = api::core_tools_router()
        .layer(Extension(json!({"_user_id":actor})))
        .layer(Extension(caller.clone()))
        .with_state(caller);
    let request = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body)?))?;
    let response = router.oneshot(request).await?;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024).await?;
    let result: Value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow!("Management API returned an invalid response"))?;
    if !status.is_success() {
        return Err(anyhow!(
            "{}",
            result["error"]
                .as_str()
                .unwrap_or("Management request rejected")
        ));
    }
    Ok(result)
}
#[allow(clippy::too_many_arguments)]
pub async fn execute(
    app: &App,
    sid: &str,
    rid: &str,
    generation: &str,
    pending: &str,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    crate::extension_plugins::execute(crate::extension_plugins::Call {
        app,
        sid,
        run_id: rid,
        generation,
        pending,
        name,
        args,
        cancel,
        ability: "Core/agent_management",
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key;
    #[tokio::test]
    async fn owning_api_bridge_preserves_session_lifecycle_and_tenant_checks() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let snapshot = request(&app, "GET", format!("/sessions/{sid}"), json!({}))
            .await
            .unwrap();
        assert_eq!(snapshot["session"]["id"], sid);
        let mut foreign = app.clone();
        foreign.chat_actor = Some("absent-user".into());
        assert!(
            request(&foreign, "GET", format!("/sessions/{sid}"), json!({}))
                .await
                .is_err()
        );
        let body = json!({"client_request_id":"core-management-archive","expected_revision":snapshot["session"]["metadata"]["revision"].as_u64().or_else(||snapshot["session"]["metadata_revision"].as_u64()).unwrap_or(0)});
        let archived = request(&app, "POST", format!("/sessions/{sid}/archive"), body)
            .await
            .unwrap();
        assert!(archived.is_object());
        assert_eq!(
            app.store.get(&key("session", &sid)).await.unwrap().unwrap()["status"],
            "archived"
        );
    }
}
