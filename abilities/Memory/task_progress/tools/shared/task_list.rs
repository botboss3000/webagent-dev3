//! Session task list tools: instant reads and revision-fenced writes of the session's one
//! list. The model never selects a user, session or run: Rust supplies those identities.
use crate::{Call, NativeHost};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_chat::task_list::{self as rules, List};

#[path = "../task_list_read/task_list_read.rs"]
mod task_list_read;
#[path = "../task_list_write/task_list_write.rs"]
mod task_list_write;

/// Tools this module executes; their schemas live in the ability's `tools/<tool>/<tool>.tool.json`.
pub const TOOLS: &[&str] = &["task_list_read", "task_list_write"];
/// Whether this module executes tool `name`.
#[must_use]
pub fn owns(name: &str) -> bool {
    TOOLS.contains(&name)
}
/// Argument types of this family's tools, checked against their tool.json by the crate test.
pub const ARGUMENT_TYPES: &[webagent_abilities::tool_arguments::ArgumentType] = &[
    webagent_abilities::argument_type!("task_list_read", task_list_read::ReadArgs),
    webagent_abilities::argument_type!("task_list_write", task_list_write::WriteArgs),
];

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}
/// The session's list at `revision` (the current one when `None`); an empty list before the
/// first write, and an error for a revision that was never saved.
async fn list_at(host: &dyn NativeHost, call: Call<'_>, revision: Option<u64>) -> Result<List> {
    if revision == Some(0) {
        return Ok(List::default());
    }
    match host
        .task_list(host.store(), call.sid, call.run_id, revision)
        .await?
    {
        Some(document) => Ok(serde_json::from_value(document["list"].clone())?),
        None if revision.is_none() => Ok(List::default()),
        None => bail!("No task list revision {}", revision.unwrap_or(0)),
    }
}
/// Apply `write` to the current list and save it under the call's `receipt`, refusing it when
/// the list is no longer at `expected_revision` (`None` builds on whatever is current).
async fn save(
    host: &dyn NativeHost,
    call: Call<'_>,
    receipt: &str,
    expected_revision: Option<u64>,
    write: rules::Write,
) -> Result<Value> {
    let list = list_at(host, call, None).await?;
    if expected_revision.is_some_and(|expected| expected != list.revision) {
        bail!(
            "The task list is at revision {}; read it and write again",
            list.revision
        );
    }
    let (next, changes) = rules::apply(&list, write, now_ms())?;
    host.write_task_list(call, receipt, list.revision, &json!(next), &json!(changes))
        .await
}
/// Seed a user-requested section from an approved plan, built on the current list.
///
/// # Errors
/// When the plan's section is refused or the list cannot be saved.
pub async fn seed_plan(
    host: &dyn NativeHost,
    call: Call<'_>,
    write: rules::Write,
) -> Result<Value> {
    save(host, call, "plan_tasks", None, write).await
}
/// Run one task list tool under fresh authority; rejections are tool results.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    if cancel.is_cancelled() {
        bail!("tool execution cancelled");
    }
    let work = async {
        // Writes are fenced to the generation this fresh authority returns.
        let generation =
            crate::task::authorize(host, call.sid, call.run_id, call.pending_id, name).await?;
        let call = Call {
            generation: &generation,
            ..call
        };
        let output = match name {
            "task_list_read" => task_list_read::run(host, call, args).await?,
            "task_list_write" => task_list_write::run(host, call, args).await?,
            _ => return Err(anyhow!("Unknown task list tool")),
        };
        Ok(json!({"status":"success","output":output}))
    };
    let result = tokio::select! {
        _ = cancel.cancelled() => bail!("tool execution cancelled"),
        result = work => result,
    };
    Ok(result.unwrap_or_else(|error: anyhow::Error| {
        json!({"status":"error","error_kind":"task_list_rejected","output":error.to_string()})
    }))
}
