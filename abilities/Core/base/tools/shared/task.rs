//! Plan validation, action ledger reads and safe declarative presentation.
//! The model never selects a user, session or run: Rust supplies those identities.
use crate::{Call, NativeHost};
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_access::Permission;
use webagent_host_core::key;

#[path = "../action_ledger_read/action_ledger_read.rs"]
mod action_ledger_read;
#[path = "../present_chat_component/present_chat_component.rs"]
mod present_chat_component;
/// Argument types of this family's tools, checked against their tool.json by the crate test.
pub const ARGUMENT_TYPES: &[webagent_abilities::tool_arguments::ArgumentType] = &[
    webagent_abilities::argument_type!("action_ledger_read", action_ledger_read::LedgerArgs),
    webagent_abilities::argument_type!(
        "present_chat_component",
        present_chat_component::ComponentArgs
    ),
];

pub use present_chat_component::answer_text;

/// Tools this module executes; their schemas are built in below.
pub const TOOLS: &[&str] = &["action_ledger_read", "present_chat_component"];
/// Whether this module executes tool `name`.
#[must_use]
pub fn owns(name: &str) -> bool {
    matches!(name, "action_ledger_read" | "present_chat_component")
}
#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Step {
    label: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    verification: Option<String>,
}
/// The agreed plan an agent hands over when it leaves Plan mode (`set_execution_mode`).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    #[serde(default)]
    goal: String,
    #[serde(default)]
    criteria: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    title: String,
    checklist: Vec<Step>,
}
impl Plan {
    /// The task list write that seeds a user-requested section from this plan's checklist.
    ///
    /// # Errors
    /// When the title holds control characters or exceeds its limit.
    pub fn task_section(&self) -> Result<webagent_chat::task_list::Write> {
        let labels: Vec<String> = self
            .checklist
            .iter()
            .map(|step| step.label.clone())
            .collect();
        Ok(webagent_chat::task_list::plan_section(
            &short_title(&self.title, "Plan")?,
            &self.notes,
            &labels,
        ))
    }
}
/// Session artifact id (and kind) of a session's agreed plan.
pub const PLAN_ARTIFACT: &str = "plan";
pub(super) fn text(value: &str, limit: usize) -> Result<String> {
    let value = value.trim();
    if value.len() > limit
        || value
            .chars()
            .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
    {
        bail!("Text exceeds {limit} bytes or contains control characters");
    }
    Ok(value.to_owned())
}
pub(super) fn short_title(value: &str, fallback: &str) -> Result<String> {
    let value = text(value, 80)?;
    if value.is_empty() || value.split_whitespace().count() > 3 {
        Ok(fallback.to_owned())
    } else {
        Ok(value)
    }
}
pub(super) fn validate_steps(steps: &[Step]) -> Result<Value> {
    if steps.is_empty() || steps.len() > 32 {
        bail!("Expected 1..32 task steps");
    }
    let mut output = Vec::with_capacity(steps.len());
    for step in steps {
        let label = text(&step.label, 240)?;
        if label.len() < 3 {
            bail!("Task step label is too short");
        }
        let status = step.status.as_deref().unwrap_or("pending");
        if !matches!(status, "pending" | "in_progress" | "done" | "blocked") {
            bail!("Invalid task step status");
        }
        output.push(json!({"label":label,"status":status,
            "verification":text(step.verification.as_deref().unwrap_or(""),240)?}));
    }
    Ok(json!(output))
}

/// The session artifact for an agreed `plan`: goal, criteria and notes, then the numbered
/// checklist. Validates every field, so a bad plan is refused before anything changes.
pub fn plan_artifact(args: &Plan) -> Result<Value> {
    let steps = validate_steps(&args.checklist)?;
    let mut sections = Vec::new();
    for (label, value) in [
        ("Goal", &args.goal),
        ("Done when", &args.criteria),
        ("Notes", &args.notes),
    ] {
        let value = text(value, 1200)?;
        if !value.is_empty() {
            sections.push(json!({"label":label,"value":value,"status":null}));
        }
    }
    let list = steps
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, s)| {
            format!(
                "{}. [{}] {}",
                i + 1,
                if s["status"] == "done" { "x" } else { " " },
                s["label"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    sections.push(json!({"label":"Checklist","value":list,"status":null}));
    let title = short_title(&args.title, "Plan")?;
    let artifact = json!({"title":title,"sections":sections});
    if serde_json::to_vec(&artifact)?.len() > 24 * 1024 {
        bail!("Plan exceeds 24 KiB");
    }
    Ok(artifact)
}
/// Fresh live authority for the task call `pending_id`: the run generation, or an error
/// when the call, its recorded revisions or the caller's live access no longer admit it.
pub(crate) async fn authorize(
    host: &dyn NativeHost,
    sid: &str,
    rid: &str,
    pending_id: &str,
    name: &str,
) -> Result<String> {
    let agent = host.session_agent_id(sid).await?;
    let scoped = host.agent_store(&agent).await?;
    let run = scoped
        .get(&key("run", rid))
        .await?
        .ok_or_else(|| anyhow!("Run not found"))?;
    if run["session_id"] != sid
        || run["agent_id"].as_str().unwrap_or("shared_default") != agent
        || run["status"] != "running"
    {
        bail!("Task run unavailable for current session");
    }
    let (mode_revision, visibility, state) = host.scoped_ability_controls(&scoped, sid).await?;
    let access = host
        .scoped_live_access(&scoped, host.actor(), sid, &agent)
        .await?;
    let pending = if run["pending"]["id"] == pending_id && run["pending"]["state"] == "executing" {
        Some(&run["pending"])
    } else {
        run["batch"]
            .as_array()
            .and_then(|batch| {
                batch.iter().find(|item| {
                    item["pending"]["id"] == pending_id && item["state"] == "executing"
                })
            })
            .map(|item| &item["pending"])
    }
    .ok_or_else(|| anyhow!("Task call is no longer executing"))?;
    if pending["mode_revision"] != json!(mode_revision)
        || pending["visibility_revision"] != visibility["revision"]
        || pending["ability_state_revision"] != state["revision"]
        || pending["access_revision"] != json!(access.revision.clone())
    {
        bail!("Task authorization changed during dispatch");
    }
    if !access.admitted()
        || host
            .live_tool_decision(&access, name, &visibility, &state, &run["loaded_abilities"])
            .permission
            == Permission::Deny
    {
        bail!("Base task tool unavailable under current access");
    }
    Ok(run["generation"]
        .as_str()
        .ok_or_else(|| anyhow!("Task run generation unavailable"))?
        .to_owned())
}
async fn work(
    host: &dyn NativeHost,
    sid: &str,
    rid: &str,
    pending_id: &str,
    name: &str,
    args: &Value,
) -> Result<Value> {
    let generation = authorize(host, sid, rid, pending_id, name).await?;
    let call = Call {
        sid,
        run_id: rid,
        generation: &generation,
        pending_id,
    };
    match name {
        "action_ledger_read" => {
            action_ledger_read::run(host, sid, rid, pending_id, name, &generation, args).await
        }
        "present_chat_component" => {
            present_chat_component::run(host, call, sid, rid, pending_id, name, &generation, args)
                .await
        }
        _ => bail!("Unknown Base task tool"),
    }
}
/// Run one task tool under fresh authority; rejections are tool results.
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
    let result = tokio::select! {
        _=cancel.cancelled()=>bail!("tool execution cancelled"),
        result=work(host,call.sid,call.run_id,call.pending_id,name,args)=>result,
    };
    if cancel.is_cancelled() {
        bail!("tool execution cancelled");
    }
    Ok(match result {
        Ok(output) => output,
        Err(error) => {
            json!({"status":"error","error_kind":"base_task_rejected","output":error.to_string()})
        }
    })
}
#[cfg(test)]
#[path = "../../tests/task.rs"]
mod tests;
