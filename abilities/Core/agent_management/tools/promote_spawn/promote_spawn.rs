//! promote_spawn: turn a sub agent session into a reusable agent.
use super::{agent_authority, in_scope, Args};
use crate::{Call, NativeHost};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

/// Fails unless `target` is an active sub agent of `parent`.
pub async fn require_child(host: &dyn NativeHost, parent: &str, target: &str) -> Result<()> {
    if in_scope(host, parent, target).await?["parent_session_id"] != parent {
        return Err(anyhow!("Sub agent session required"));
    }
    Ok(())
}

/// Create a reusable agent from sub agent session `q.session_id` with its instructions.
pub(super) async fn run(
    host: &dyn NativeHost,
    call: Call<'_>,
    args: &Value,
    cancel: &CancellationToken,
    run: &Value,
    request_id: &str,
    q: Args,
) -> Result<Value> {
    let sid = call.sid;
    let child = q
        .session_id
        .as_deref()
        .ok_or_else(|| anyhow!("Sub agent session required"))?;
    require_child(host, sid, child).await?;
    let name = q
        .name
        .filter(|v| !v.trim().is_empty() && v.len() <= 200)
        .ok_or_else(|| anyhow!("Agent name required"))?;
    let description = q.description.unwrap_or_default();
    if description.len() > 2000 {
        return Err(anyhow!("Description exceeds bounds"));
    }
    let content = q
        .instructions
        .filter(|v| !v.trim().is_empty() && v.len() <= 16000)
        .ok_or_else(|| anyhow!("Bounded reusable instructions required"))?;
    host.dependency(sid, run, "create_agent").await?;
    host.dependency(sid, run, "edit_agent_prompt").await?;
    let created = host
        .agent_request(
            "POST",
            "/models/agents".into(),
            json!({"client_request_id":request_id,"name":name,"description":description}),
        )
        .await?;
    let agent = created["id"]
        .as_str()
        .ok_or_else(|| anyhow!("Created agent ID unavailable"))?;
    if agent_authority(host, host.actor(), agent).await?["can_manage"] != true {
        return Err(anyhow!("Agent authority changed"));
    }
    if let Some(slot) = host.prompt_slot(host.actor(), agent, "core_helper").await? {
        if slot["content"] != content {
            return Err(anyhow!(
                "Reusable agent prompt changed; review it before editing"
            ));
        }
    } else {
        host.authorize(call, "promote_spawn", args, cancel).await?;
        host.agent_request(
            "PUT",
            format!("/agents/{agent}/slots/core_helper"),
            json!({"content":content,"expected_revision":0,"order_index":100,"lock":false}),
        )
        .await?;
    }
    Ok(
        json!({"agent":created,"source_session_id":child,"prompt_slot":"core_helper","permissions_copied":false}),
    )
}
