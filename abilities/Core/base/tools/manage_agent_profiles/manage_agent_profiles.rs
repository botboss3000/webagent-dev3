//! manage_agent_profiles: list, read, save, remove and apply identity presets.
use super::{identifier, Args, Call, NativeHost};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};

pub(super) async fn run(host: &dyn NativeHost, call: Call<'_>, q: Args) -> Result<Value> {
    if q.action.as_deref() == Some("list") {
        let items=host.store().list("core_profile:").await?.into_iter().filter(|v|v["artifact"]["deleted"]!=true).map(|v|json!({"profile_id":v["id"],"revision":v["revision"],"name":v["artifact"]["name"],"description":v["artifact"]["description"]})).take(100).collect::<Vec<_>>();
        return Ok(json!({"status":"success","output":{"profiles":items}}));
    }
    let id = identifier(q.profile_id.as_deref(), 64)?;
    let prior = host.store().get(&format!("core_profile:{id}")).await?;
    let output = match q.action.as_deref() {
        Some("read") => prior
            .filter(|v| v["artifact"]["deleted"] != true)
            .ok_or_else(|| anyhow!("Profile not found"))?,
        Some("save" | "remove") => {
            let remove = q.action.as_deref() == Some("remove");
            let artifact = if remove {
                if prior
                    .as_ref()
                    .is_none_or(|v| v["artifact"]["deleted"] == true)
                {
                    return Err(anyhow!("Profile not found"));
                }
                json!({"deleted":true})
            } else {
                let name = q
                    .name
                    .filter(|v| {
                        !v.trim().is_empty() && v.len() <= 200 && !v.chars().any(char::is_control)
                    })
                    .ok_or_else(|| anyhow!("Bounded profile name required"))?;
                let description = q.description.unwrap_or_default();
                if description.len() > 2000 {
                    return Err(anyhow!("Profile description exceeds bounds"));
                }
                if prior.is_none() && host.store().list("core_profile:").await?.len() >= 100 {
                    return Err(anyhow!("Profile quota reached"));
                }
                json!({"name":name,"description":description,"deleted":false})
            };
            host.write_artifact(
                call,
                "core_profile",
                id,
                q.expected_revision
                    .ok_or_else(|| anyhow!("Profile revision required"))?,
                &artifact,
            )
            .await?
        }
        Some("apply") => {
            let preset = prior
                .filter(|v| v["artifact"]["deleted"] != true)
                .ok_or_else(|| anyhow!("Profile not found"))?;
            if preset["revision"]
                != q.expected_revision
                    .ok_or_else(|| anyhow!("Profile revision required"))?
            {
                return Err(anyhow!("Profile revision changed"));
            }
            let agent = identifier(q.agent_id.as_deref(), 128)?;
            host.agent_request("PUT",format!("/models/agents/{agent}/identity"),json!({"revision":q.agent_revision.ok_or_else(||anyhow!("Agent identity revision required"))?,"name":preset["artifact"]["name"],"description":preset["artifact"]["description"]})).await?
        }
        _ => return Err(anyhow!("Unknown profile action")),
    };
    Ok(json!({"status":"success","output":output}))
}
