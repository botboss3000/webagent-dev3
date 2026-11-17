//! request_agent_login: starts a protected model connection authorization.
use super::{identifier, Args, NativeHost};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};

pub(super) async fn run(host: &dyn NativeHost, sid: &str, q: &Args) -> Result<Value> {
    let id = identifier(q.connection_id.as_deref(), 128)?;
    let started = host
        .agent_request(
            "POST",
            format!("/models/connections/{id}/authorization"),
            json!({}),
        )
        .await?;
    if !started.is_object() {
        return Err(anyhow!("Authorization setup unavailable"));
    }
    let output = json!({"authorization_started":true,"user_action_required":true,"setup_url":format!("/app?tab=agents&agent={}&agentView=model",host.session_agent_id(sid).await?)});
    Ok(json!({"status":"success","output":output}))
}
