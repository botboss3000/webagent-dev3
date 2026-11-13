//! The host owns authority and encrypted connections; the live package only transports MCP.
pub(crate) use webagent_extensions::mcp_bridge as bridge;
mod connections;
use crate::{json, App, CancellationToken, Result, Value};
pub(crate) use connections::{configured, inspect, put, read, remove, search};

/// Handled here; schemas and guidance live in `abilities/Core/mcp`.
pub(crate) const TOOLS: &[&str] = &[
    "mcp_list_connections",
    "mcp_search_servers",
    "mcp_inspect_server",
    "mcp_list_tools",
    "mcp_call_tool",
    "mcp_add_connection",
];
pub(crate) fn owns(name: &str) -> bool {
    TOOLS.contains(&name)
}
/// Schemas from the MCP tool folders; a missing or invalid folder drops only its tool.
pub(crate) fn all_schemas() -> Vec<Value> {
    TOOLS
        .iter()
        .filter_map(|name| webagent_abilities::tool_definitions::get(name))
        .map(webagent_abilities::tool_definitions::ToolDefinition::schema)
        .collect()
}
pub(crate) fn schemas() -> Vec<Value> {
    if bridge::available() {
        all_schemas()
    } else {
        Vec::new()
    }
}
pub(crate) async fn execute(
    app: &App,
    sid: &str,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    if !bridge::available() {
        return Ok(json!({"status":"denied","output":"MCP plugin is unavailable"}));
    }
    let agent = crate::session_agent_id(app, sid).await?;
    // Shared runs keep the owner's storage while the sealed requester supplies authority.
    let access =
        crate::shared_chat::live_access(&app.store, crate::shared_chat::actor(app), sid, &agent)
            .await?;
    anyhow::ensure!(access.admitted(), "MCP access revoked");
    match name {
        "mcp_list_connections" => connections::model_connections(app, &agent).await,
        "mcp_list_tools" => connections::model_tools(app, &agent, args).await,
        "mcp_inspect_server" => {
            bridge::call(
                json!({"version":1,"operation":"inspect","endpoint":args["endpoint"]}),
                cancel,
            )
            .await
        }
        "mcp_search_servers" => {
            bridge::call(
                json!({"version":1,"operation":"search","query":args["query"]}),
                cancel,
            )
            .await
        }
        "mcp_add_connection" => connections::model_add(app, &agent, args, cancel).await,
        "mcp_call_tool" => connections::model_call(app, &agent, args, cancel).await,
        _ => anyhow::bail!("Unknown MCP operation"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn remote_arguments_remain_open_for_provider_function_calling() {
        let schemas = super::all_schemas();
        let call = schemas
            .iter()
            .find(|v| v["name"] == "mcp_call_tool")
            .unwrap();
        assert_eq!(call["strict"], false);
        assert!(call["parameters"]["properties"]["arguments"]
            .get("additionalProperties")
            .is_none());
    }
    #[test]
    fn connection_management_and_remote_calls_are_independent_mutations() {
        let policy = crate::policy_default();
        assert!(!crate::tools::is_read("mcp_add_connection"));
        assert!(!crate::tools::is_read("mcp_call_tool"));
        assert_eq!(policy["rules"]["mcp_add_connection"], "require_approval");
        assert_eq!(policy["rules"]["mcp_call_tool"], "require_approval");
        assert!(!crate::tool_execution::background_capable("mcp_call_tool"));
        let mut restricted = policy;
        restricted["rules"]["mcp_add_connection"] = serde_json::json!("deny");
        assert_eq!(restricted["rules"]["mcp_call_tool"], "require_approval");
    }
}
