//! get_task_checkpoint: reads the run's task checkpoint and action statuses.
use super::authorize;
use crate::NativeHost;
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use webagent_host_core::key;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointArgs {
    #[serde(default)]
    action_id: Option<String>,
}
pub(super) async fn run(
    host: &dyn NativeHost,
    sid: &str,
    rid: &str,
    pending_id: &str,
    name: &str,
    generation: &str,
    args: &Value,
) -> Result<Value> {
    let query: CheckpointArgs = serde_json::from_value(args.clone())?;
    let mut state = host.task_checkpoint(host.store(), sid, rid).await?;
    let run = host
        .store()
        .get(&key("run", rid))
        .await?
        .ok_or_else(|| anyhow!("Run not found"))?;
    state["actions"] = json!(host.action_statuses(&run).await?);
    if let Some(id) = query.action_id {
        state["action"] = host.action_lookup(&run, &id).await?;
    }
    if authorize(host, sid, rid, pending_id, name).await? != generation {
        bail!("Task run generation changed");
    }
    Ok(json!({"status":"success","output":state}))
}
