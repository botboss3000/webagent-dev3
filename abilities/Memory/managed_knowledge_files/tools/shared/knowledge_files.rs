//! Agent-owned text knowledge files (menus, price lists, policies, FAQs).
//! Staff save and delete; every session of the agent can list, search and read.
use crate::{Call, NativeHost};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_access::Relationship;

#[path = "../knowledge_file_delete/knowledge_file_delete.rs"]
mod knowledge_file_delete;
#[path = "../knowledge_file_get/knowledge_file_get.rs"]
mod knowledge_file_get;
#[path = "../knowledge_file_list/knowledge_file_list.rs"]
mod knowledge_file_list;
#[path = "../knowledge_file_save/knowledge_file_save.rs"]
mod knowledge_file_save;
#[path = "../knowledge_file_search/knowledge_file_search.rs"]
mod knowledge_file_search;

#[must_use]
pub fn owns(name: &str) -> bool {
    TOOLS.contains(&name)
}
/// Tools this module executes; their schemas live in the ability's `tools/<tool>/<tool>.tool.json`.
pub const TOOLS: &[&str] = &[
    "knowledge_file_list",
    "knowledge_file_search",
    "knowledge_file_get",
    "knowledge_file_save",
    "knowledge_file_delete",
];
/// Argument types of this family's tools, checked against their tool.json by the crate test.
pub const ARGUMENT_TYPES: &[webagent_abilities::tool_arguments::ArgumentType] = &[
    webagent_abilities::argument_type!("knowledge_file_delete", knowledge_file_delete::DeleteArgs),
    webagent_abilities::argument_type!("knowledge_file_get", knowledge_file_get::GetArgs),
    webagent_abilities::argument_type!("knowledge_file_save", knowledge_file_save::SaveArgs),
    webagent_abilities::argument_type!("knowledge_file_search", knowledge_file_search::SearchArgs),
];

fn file_id(value: &str) -> Result<String> {
    Some(value)
        .filter(|v| !v.is_empty() && v.len() <= 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("Invalid file ID"))
}
fn summary(file: &Value) -> Value {
    json!({"file_id":file["id"],"name":file["name"],"revision":file["revision"],"bytes":file["content"].as_str().map_or(0,str::len),"updated_at":file["updated_at"]})
}
/// Runs one knowledge file tool in the run's agent plane; writes need an owner or member.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let run = host.authorize(call, name, args, &cancel).await?;
    let agent = run["agent_id"]
        .as_str()
        .unwrap_or("shared_default")
        .to_owned();
    let actor = host.actor().to_owned();
    if !crate::is_read(name)
        && !matches!(
            host.relationship(call.sid, &agent).await?,
            Relationship::Owner | Relationship::Member
        )
    {
        return Ok(json!({"status":"denied","output":"Staff access required"}));
    }
    let store = host.agent_store(&agent).await?;
    let prefix = format!("agent:{agent}:knowledge:");
    match name {
        "knowledge_file_list" => knowledge_file_list::run(&store, &prefix).await,
        "knowledge_file_search" => knowledge_file_search::run(&store, args, &prefix).await,
        "knowledge_file_get" => knowledge_file_get::run(&store, args, &prefix).await,
        "knowledge_file_save" => {
            knowledge_file_save::run(&store, call, args, &agent, &actor, &prefix).await
        }
        "knowledge_file_delete" => knowledge_file_delete::run(&store, args, &prefix).await,
        _ => bail!("Unknown knowledge file tool"),
    }
}

#[cfg(test)]
#[path = "../../tests/knowledge_files.rs"]
mod tests;
