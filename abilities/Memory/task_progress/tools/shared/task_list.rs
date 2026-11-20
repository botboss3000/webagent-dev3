//! Session task list tools: instant reads and revision-fenced writes of the session's one
//! list. The model never selects a user, session or run: Rust supplies those identities.
use crate::{Call, NativeHost};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_chat::task_list::{self as rules, List};
use webagent_context_contracts::context_compaction;
use webagent_host_core::key;

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
/// `fold` carries the agent's fold of earlier work and its summary focus.
async fn save(
    host: &dyn NativeHost,
    call: Call<'_>,
    receipt: &str,
    expected_revision: Option<u64>,
    write: rules::Write,
    fold: Option<&str>,
) -> Result<Value> {
    let list = list_at(host, call, None).await?;
    if expected_revision.is_some_and(|expected| expected != list.revision) {
        bail!(
            "The task list is at revision {}; read it and write again",
            list.revision
        );
    }
    let target = write.section.id.clone();
    let (next, changes) = rules::apply(&list, write, now_ms())?;
    let marks = rules::marks(&list, &next);
    let mut output = host
        .write_task_list(
            call,
            receipt,
            list.revision,
            &json!(next),
            &json!(changes),
            &marks,
        )
        .await?;
    // A plan's section keeps the plan's checklist current; the plan seeding call has
    // already saved the plan itself.
    let section = match target {
        Some(id) => rules::section(&next, &id),
        None => next.sections.last(),
    };
    if let Some(focus) = fold {
        let started = section.is_some_and(|section| {
            marks["started"]
                .as_array()
                .is_some_and(|ids| ids.iter().any(|id| *id == section.id.as_str()))
        });
        output["fold"] = json!(match section.filter(|_| started) {
            Some(section) => match request_fold(host, call, &section.id, focus).await {
                Ok(()) => "Earlier work folds into a summary in the background; this task's \
                    messages stay word for word."
                    .to_owned(),
                Err(error) => format!("Nothing was folded: {error}."),
            },
            None => "Nothing was folded: fold_earlier applies only to the write that starts a \
                section's first step."
                .to_owned(),
        });
    }
    if let Some(section) = section.filter(|section| section.plan && receipt == "tasks") {
        output["plan"] = json!(match update_plan(host, call, section).await {
            Ok(()) => "The plan's checklist was updated.".to_owned(),
            Err(error) => format!("The plan's checklist was not updated: {error}"),
        });
    }
    Ok(output)
}
/// Store the agent's fold of everything before task `section`, which the run loop summarizes
/// in the background at its next model call; refused while another summary is requested or
/// running, or when there is too little earlier context.
async fn request_fold(
    host: &dyn NativeHost,
    call: Call<'_>,
    section: &str,
    focus: &str,
) -> Result<()> {
    let agent = host.session_agent_id(call.sid).await?;
    let scoped = host.agent_store(&agent).await?;
    let run = scoped
        .get(&key("run", call.run_id))
        .await?
        .context("run not found")?;
    let input = run["input"].as_array().context("run context unavailable")?;
    let request_key = format!("compaction_request:{}", call.sid);
    let request = scoped.get(&request_key).await?.unwrap_or(Value::Null);
    let job = scoped
        .get(&format!("context_compaction_job:{}", call.sid))
        .await?
        .unwrap_or(Value::Null);
    if request["state"] == "pending" || job["state"] == "running" {
        bail!("a summary is already requested or running");
    }
    let config = host.compaction_config()?;
    let cut = context_compaction::task_fold_cut(input, section, &config)?
        .context("there is too little earlier context to fold")?;
    let turn = context_compaction::turn_key(&input[cut]).context("no user message to cut at")?;
    scoped
        .put(
            &request_key,
            &json!({"state":"pending","source":"task","focus":focus,
                "until":{"turn_key":turn},"requested_at":now_ms()}),
        )
        .await
}
/// Rewrite the session plan's checklist from its section's steps.
async fn update_plan(
    host: &dyn NativeHost,
    call: Call<'_>,
    section: &rules::Section,
) -> Result<()> {
    let Some(plan) = host
        .session_artifact(call.sid, crate::task::PLAN_ARTIFACT)
        .await?
    else {
        bail!("The session has no plan");
    };
    let body = rules::plan_with_checklist(&plan, section)?;
    host.write_session_artifact(
        call,
        crate::task::PLAN_ARTIFACT,
        crate::task::PLAN_ARTIFACT,
        &body,
    )
    .await?;
    Ok(())
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
    save(host, call, "plan_tasks", None, write, None).await
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
