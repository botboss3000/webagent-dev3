//! Human escalation: hand a conversation to the agent's staff or leave them a
//! message. Messages live in the agent plane so staff see every visitor's message.
use crate::{Call, NativeHost};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_access::Relationship;

#[path = "../request_human/request_human.rs"]
mod request_human;
#[path = "../staff_message_list/staff_message_list.rs"]
mod staff_message_list;
#[path = "../staff_message_send/staff_message_send.rs"]
mod staff_message_send;
#[path = "../staff_message_update/staff_message_update.rs"]
mod staff_message_update;

/// Whether this family executes tool `name`.
#[must_use]
pub fn owns(name: &str) -> bool {
    TOOLS.contains(&name)
}
/// Tools this adapter executes; their schemas live in the ability's `tools/<tool>/<tool>.tool.json`.
pub const TOOLS: &[&str] = &[
    "request_human",
    "staff_message_send",
    "staff_message_list",
    "staff_message_update",
];
/// Argument types of this family's tools, checked against their tool.json by the crate test.
pub const ARGUMENT_TYPES: &[webagent_abilities::tool_arguments::ArgumentType] = &[
    webagent_abilities::argument_type!("request_human", request_human::HelpArgs),
    webagent_abilities::argument_type!("staff_message_list", staff_message_list::ListArgs),
    webagent_abilities::argument_type!("staff_message_send", staff_message_send::SendArgs),
    webagent_abilities::argument_type!("staff_message_update", staff_message_update::UpdateArgs),
];

pub(crate) fn text(value: &str, limit: usize) -> Result<String> {
    let value = value.trim();
    if value.len() > limit
        || value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        bail!("Text exceeds {limit} bytes or contains control characters");
    }
    Ok(value.to_owned())
}
/// Run human escalation tool `name` under fresh run authority at `call`'s position.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let (sid, rid, pending) = (call.sid, call.run_id, call.pending_id);
    let run = host.authorize(call, name, args, &cancel).await?;
    let agent = run["agent_id"]
        .as_str()
        .unwrap_or("shared_default")
        .to_owned();
    let actor = host.actor().to_owned();
    let relationship = host.relationship(sid, &agent).await?;
    // Staff tools are checked here as well as by the capability ladder, so a
    // visitor-level grant of this ability never exposes other visitors' messages.
    if matches!(name, "staff_message_list" | "staff_message_update")
        && !matches!(relationship, Relationship::Owner | Relationship::Member)
    {
        return Ok(json!({"status":"denied","output":"Staff access required"}));
    }
    let store = host.agent_store(&agent).await?;
    match name {
        "staff_message_send" => {
            staff_message_send::run(&store, args, &agent, &actor, sid, rid, pending).await
        }
        "staff_message_list" => staff_message_list::run(&store, args, &agent).await,
        "staff_message_update" => {
            staff_message_update::run(&store, args, &agent, &actor, pending).await
        }
        "request_human" => request_human::run(host, args, &agent, &actor, sid, pending).await,
        _ => bail!("Unknown human escalation tool"),
    }
}

#[cfg(test)]
#[path = "../../tests/human_escalation.rs"]
mod tests;
