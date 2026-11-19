//! Native User Files adapter. The actor's private database owns files; no host path is accepted.
use crate::{Call, NativeHost};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_abilities::user_files::Repository;

#[must_use]
pub fn owns(name: &str) -> bool {
    matches!(
        name,
        "save_file" | "list_user_files" | "read_user_file" | "attach_file"
    )
}
/// Tools this adapter executes; their schemas live in the ability's `tools/<tool>/<tool>.tool.json`.
pub const TOOLS: &[&str] = &[
    "save_file",
    "list_user_files",
    "read_user_file",
    "attach_file",
];
#[path = "../attach_file/attach_file.rs"]
mod attach_file;
#[path = "../list_user_files/list_user_files.rs"]
mod list_user_files;
#[path = "../read_user_file/read_user_file.rs"]
mod read_user_file;
#[path = "../save_file/save_file.rs"]
mod save_file;
/// Argument types of this family's tools, checked against their tool.json by the crate test.
pub const ARGUMENT_TYPES: &[webagent_abilities::tool_arguments::ArgumentType] = &[
    webagent_abilities::argument_type!("attach_file", attach_file::Attach),
    webagent_abilities::argument_type!("list_user_files", list_user_files::List),
    webagent_abilities::argument_type!("read_user_file", read_user_file::Read),
    webagent_abilities::argument_type!("save_file", save_file::Save),
];
/// Saves, lists, reads or attaches the owner's private files.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    if cancel.is_cancelled() {
        return Err(anyhow!("File operation cancelled"));
    }
    let run = host.authorize(call, name, args, &cancel).await?;
    let actor = host.actor();
    if actor != host.store().current_user_id() {
        return Err(anyhow!(
            "Use an owner-private conversation for private saved files"
        ));
    }
    let user = host.user_store(actor).await?;
    let repo = Repository::new(&user.pool, &user);
    let output = match name {
        "save_file" => save_file::run(host, call, &run, args, &repo).await?,
        "list_user_files" => list_user_files::run(args, &repo).await?,
        "read_user_file" => read_user_file::run(args, &repo).await?,
        "attach_file" => attach_file::run(host, call, args, &repo).await?,
        _ => return Err(anyhow!("Unknown User Files tool")),
    };
    Ok(json!({"status":"success","output":output}))
}
