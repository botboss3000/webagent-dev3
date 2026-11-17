//! start_spawn: sub agent limits a new child inherits; the start runs through session messaging.
use super::{children, settings};
use crate::NativeHost;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};

/// The sub agent limits a new child of `sid` inherits through `start_spawn`, after
/// checking them; `null` for every other start, which creates a separate session.
pub async fn spawn_limits(host: &dyn NativeHost, sid: &str, name: &str) -> Result<Value> {
    if name != "start_spawn" {
        return Ok(Value::Null);
    }
    let (settings, _) = settings(host, sid).await?;
    let origin = host
        .store()
        .get(&format!("core_spawn_origin:{sid}"))
        .await?
        .unwrap_or(json!({}));
    let depth = origin["depth"].as_u64().unwrap_or(0);
    let max_depth = settings["max_spawn_depth"]
        .as_u64()
        .unwrap_or(2)
        .min(5)
        .min(origin["max_depth"].as_u64().unwrap_or(5));
    let max_children = settings["max_spawns_per_spawn"]
        .as_u64()
        .unwrap_or(1)
        .min(10);
    let max_active = settings["max_concurrent_spawns"]
        .as_u64()
        .unwrap_or(10)
        .clamp(1, 20);
    if depth > max_depth {
        return Err(anyhow!("Sub agent depth limit reached"));
    }
    let children = children(host, sid).await?;
    if depth > 0 && children.len() as u64 >= max_children {
        return Err(anyhow!("This sub agent's own sub agent budget is used up"));
    }
    let active = host.active_runs().await?;
    let mut count = 0u64;
    for child in children {
        let child = child["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Sub agent identity missing"))?;
        let queued = host.followup_queue(child).await?;
        if active
            .iter()
            .any(|r| r["session_id"] == child && !host.terminal(r["status"].as_str().unwrap_or("")))
            || queued["entries"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|v| matches!(v["status"].as_str(), Some("pending" | "reviewing" | "held")))
        {
            count += 1;
        }
    }
    if count >= max_active {
        return Err(anyhow!("Concurrent sub agent limit reached"));
    }
    Ok(
        json!({"depth":depth,"max_depth":max_depth,"max_children":max_children,"max_active":max_active}),
    )
}
