//! action_ledger_read: the run requester's recorded actions, or one action's saved result.
use super::authorize;
use crate::NativeHost;
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use webagent_host_core::key;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LedgerArgs {
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
    let query: LedgerArgs = serde_json::from_value(args.clone())?;
    let run = host
        .store()
        .get(&key("run", rid))
        .await?
        .ok_or_else(|| anyhow!("Run not found"))?;
    let mut output = json!({"actions":host.action_statuses(&run).await?});
    if let Some(id) = query.action_id {
        output["action"] = host.action_lookup(&run, &id).await?;
    }
    if authorize(host, sid, rid, pending_id, name).await? != generation {
        bail!("Task run generation changed");
    }
    Ok(json!({"status":"success","output":output}))
}
