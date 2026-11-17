//! update_task_checkpoint: validates and saves the agent's task checkpoint.
use super::{authorize, text, validate_steps, Step};
use crate::{Call, NativeHost};
use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateArgs {
    expected_revision: u64,
    update: Update,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Update {
    summary: String,
    #[serde(default)]
    steps: Option<Vec<Step>>,
    #[serde(default)]
    active_step: Option<String>,
    #[serde(default)]
    next_action: Option<String>,
    #[serde(default)]
    blockers: Option<Vec<String>>,
}
pub(super) fn checkpoint(update: Update) -> Result<Value> {
    let summary = text(&update.summary, 1200)?;
    if summary.len() < 20 {
        bail!("Task summary needs 20..1200 bytes");
    }
    let steps = update
        .steps
        .as_ref()
        .map(|steps| validate_steps(steps))
        .transpose()?;
    let active_step = update
        .active_step
        .as_deref()
        .map(|s| text(s, 120))
        .transpose()?;
    let next_action = update
        .next_action
        .as_deref()
        .map(|s| text(s, 1200))
        .transpose()?;
    let blockers = update
        .blockers
        .map(|items| {
            if items.len() > 8 {
                bail!("Too many task blockers");
            }
            items
                .into_iter()
                .map(|item| text(&item, 240))
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?;
    Ok(
        json!({"summary":summary,"steps":steps,"active_step":active_step,
        "next_action":next_action,"blockers":blockers,"provenance":"agent_claim"}),
    )
}
pub(super) async fn run(
    host: &dyn NativeHost,
    call: Call<'_>,
    sid: &str,
    rid: &str,
    pending_id: &str,
    name: &str,
    generation: &str,
    args: &Value,
) -> Result<Value> {
    let request: UpdateArgs = serde_json::from_value(args.clone())?;
    let artifact = checkpoint(request.update)?;
    if authorize(host, sid, rid, pending_id, name).await? != generation {
        bail!("Task run generation changed");
    }
    let saved = host
        .write_artifact(
            call,
            "checkpoint",
            "task",
            request.expected_revision,
            &artifact,
        )
        .await?;
    Ok(json!({"status":"success","output":saved}))
}
