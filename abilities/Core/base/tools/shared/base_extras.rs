//! Caller-owned identity presets, skill feedback and protected connection setup.
use crate::{Call, NativeHost};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
/// Whether this family executes tool `name`.
#[must_use]
pub fn owns(name: &str) -> bool {
    matches!(
        name,
        "register_user" | "manage_agent_profiles" | "rate_skill" | "request_agent_login"
    )
}
#[path = "../manage_agent_profiles/manage_agent_profiles.rs"]
mod manage_agent_profiles;
#[path = "../rate_skill/rate_skill.rs"]
mod rate_skill;
#[path = "../register_user/register_user.rs"]
mod register_user;
#[path = "../request_agent_login/request_agent_login.rs"]
mod request_agent_login;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    action: Option<String>,
    profile_id: Option<String>,
    name: Option<String>,
    description: Option<String>,
    expected_revision: Option<u64>,
    agent_id: Option<String>,
    agent_revision: Option<u64>,
    ability_id: Option<String>,
    kind: Option<String>,
    skill_id: Option<String>,
    score: Option<u8>,
    comment: Option<String>,
    connection_id: Option<String>,
}
/// Argument types of this family's tools, checked against their tool.json by the crate test.
pub const ARGUMENT_TYPES: &[webagent_abilities::tool_arguments::ArgumentType] = &[
    webagent_abilities::argument_type!("manage_agent_profiles", Args),
    webagent_abilities::argument_type!("rate_skill", Args),
    webagent_abilities::argument_type!("register_user", Args),
    webagent_abilities::argument_type!("request_agent_login", Args),
];
fn identifier(raw: Option<&str>, max: usize) -> Result<&str> {
    let raw = raw.ok_or_else(|| anyhow!("Identifier required"))?;
    if raw.is_empty()
        || raw.len() > max
        || !raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(anyhow!("Invalid identifier"));
    }
    Ok(raw)
}
/// Run one owner-private account tool at `call`'s position under fresh run authority.
///
/// # Errors
/// When authority, the caller, the arguments or the private record checks fail.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let (sid, rid) = (call.sid, call.run_id);
    let run = host.authorize(call, name, args, &cancel).await?;
    if host.actor() != host.store().current_user_id() {
        return Err(anyhow!(
            "Use an owner-private conversation for private account records"
        ));
    }
    let q: Args = serde_json::from_value(args.clone())?;
    let generation = run["generation"]
        .as_str()
        .ok_or_else(|| anyhow!("Run generation missing"))?;
    let call = Call { generation, ..call };
    match name {
        "register_user" => register_user::run(host, call, name, args, cancel).await,
        "request_agent_login" => request_agent_login::run(host, sid, &q).await,
        "rate_skill" => rate_skill::run(host, call, sid, rid, &run, q).await,
        "manage_agent_profiles" => manage_agent_profiles::run(host, call, q).await,
        _ => Err(anyhow!("Unknown Base account tool")),
    }
}
#[cfg(test)]
#[path = "../../tests/base_extras.rs"]
mod tests;
