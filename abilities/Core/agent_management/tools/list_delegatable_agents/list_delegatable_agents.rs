//! list_delegatable_agents: agents a session may delegate to, with model readiness.
use super::{allow_delegation, private_admitted};
use crate::NativeHost;
use anyhow::Result;
use serde_json::{json, Value};
use webagent_models::model_selection::ModelRole;

/// The owner-private agents the actor may start, and whether session `sid` may delegate.
pub(super) async fn run(host: &dyn NativeHost, sid: &str) -> Result<Value> {
    let all = host
        .agent_request("GET", "/models/agents".into(), json!({}))
        .await?;
    let catalog = host.model_catalog().await?;
    let mut eligible = Vec::new();
    for row in all["agents"].as_array().into_iter().flatten() {
        if let Some(id) = row["id"].as_str() {
            if private_admitted(host, id, true).await? {
                eligible.push(json!({"id":id,"name":row["name"],"description":row["description"],"standard_model_assigned":catalog.resolve_role(id,ModelRole::Standard).is_ok()}));
            }
        }
    }
    Ok(json!({ "agents": eligible, "delegation_allowed": allow_delegation(host, sid).await? }))
}
