//! request_human: ask the agent's staff to join this shared chat.
use super::text;
use crate::NativeHost;
use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HelpArgs {
    reason: String,
}

pub(super) async fn run(
    host: &dyn NativeHost,
    args: &Value,
    agent: &str,
    actor: &str,
    sid: &str,
    pending: &str,
) -> Result<Value> {
    if !host.shared_chat_policy(agent).await?.help {
        return Ok(
            json!({"status":"denied","output":"Human handoff is not enabled for this agent; offer to leave staff a message instead"}),
        );
    }
    let help: HelpArgs = webagent_abilities::tool_arguments::read("request_human", args)?;
    text(&help.reason, 300)?;
    let owner = host.shared_chat_owner(sid, actor).await?;
    let owner = host.user_store(&owner).await?;
    let request = host
        .shared_help_request(&owner, sid, actor, pending)
        .await?;
    let output = json!({"help_request":request["status"],"note":"Staff were notified in the app. Tell the visitor a team member will join this chat; keep helping meanwhile."});
    Ok(json!({"status":"success","output":output}))
}
