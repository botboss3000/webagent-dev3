//! A model can change only the authenticated run's own session mode.
//! Entering Auto always needs a fresh human approval. Leaving Plan carries the agreed plan,
//! which is saved as the session's plan artifact once the user lets the work start.
use crate::task::{plan_artifact, Plan, PLAN_ARTIFACT};
use crate::{Call, NativeHost};
use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_host_core::key;

/// Whether this family executes tool `name`.
#[must_use]
pub fn owns(name: &str) -> bool {
    name == "set_execution_mode"
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    mode: String,
    /// Filled by the server from the mode last shown to this run; accepted for compatibility.
    #[serde(default)]
    expected_revision: Option<u64>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    plan: Option<Plan>,
}
/// Argument types of this family's tools, checked against their tool.json by the crate test.
pub const ARGUMENT_TYPES: &[webagent_abilities::tool_arguments::ArgumentType] =
    &[webagent_abilities::argument_type!(
        "set_execution_mode",
        Args
    )];
/// Change the session mode at `call`'s position; a rejected change is an error result.
///
/// # Errors
/// When the tool is not owned or the call is already cancelled.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    if !owns(name) || cancel.is_cancelled() {
        bail!("Mode change unavailable");
    }
    let result: Result<Value> = async {
        let query: Args = serde_json::from_value(args.clone())?;
        if query.mode.is_empty()
            || query.mode.len() > 32
            || query.reason.as_ref().is_some_and(|r| r.len() > 500)
        {
            bail!("Invalid mode change");
        }
        // A bad plan is refused before the mode changes.
        let artifact = query.plan.as_ref().map(plan_artifact).transpose()?;
        let section = query.plan.as_ref().map(Plan::task_section).transpose()?;
        let agent = host.session_agent_id(call.sid).await?;
        let store = host.agent_store(&agent).await?;
        // "Continue in Ask mode" approves this Auto request with Ask as the target.
        let run = store
            .get(&key("run", call.run_id))
            .await?
            .unwrap_or(Value::Null);
        let to_ask = query.mode == "auto"
            && run["pending"]["id"] == call.pending_id
            && run["pending"]["alternate_mode"] == "ask";
        let mode = if to_ask { "ask" } else { query.mode.as_str() };
        // The revision is the one in the mode guidance this run last saw, so a mode the user
        // changed since then is never overwritten by a change the agent decided on earlier.
        let current = host.session_mode(call.sid).await?;
        let seen = query
            .expected_revision
            .or_else(|| run["mode_revision_shown"].as_u64())
            .or_else(|| current["revision"].as_u64())
            .unwrap_or(0);
        let response = host
            .change_session_mode(&agent, call, mode, seen)
            .await?
            .map_err(|_| {
                if current["revision"].as_u64().is_some_and(|now| now != seen) {
                    anyhow::anyhow!(
                        "The user changed the execution mode to {} since your last step, so \
                         this change was not made. The current mode guidance is in this step; \
                         ask the user before changing the mode again.",
                        current["execution_mode"].as_str().unwrap_or("another mode")
                    )
                } else {
                    anyhow::anyhow!("Session mode access changed")
                }
            })?;
        if let Some(artifact) = artifact {
            host.write_session_artifact(call, PLAN_ARTIFACT, PLAN_ARTIFACT, &artifact)
                .await?;
        }
        // The plan's checklist becomes a section of the session task list. The mode has
        // already changed, so a list that cannot take it is reported, not a failed call.
        let tasks = match section {
            Some(section) => Some(
                match crate::task_list::seed_plan(host, call, section).await {
                    Ok(saved) => saved,
                    Err(error) => {
                        json!({"error":format!("The plan was not added to the task list: {error}")})
                    }
                },
            ),
            None => None,
        };
        // The result carries the new mode's guidance, so the conversation history holds it from
        // the step it changed (unless mode_guidance.json puts the guidance in each step instead).
        let now = host.session_mode(call.sid).await?;
        let mut result = json!({"status":"success","output":response,
            "mode":now["definition"]["id"].as_str().unwrap_or(mode),"revision":now["revision"]});
        if webagent_context::defaults::mode_guidance() != "per_step" {
            result["guidance"] = json!(guidance_text(&now));
        }
        if let Some(tasks) = tasks {
            result["task_list"] = tasks;
        }
        if to_ask {
            result["note"] = json!(
                "The user chose Ask mode instead of Auto: execute the agreed plan now; \
                 tools that need approval will ask the user."
            );
        }
        Ok(result)
    }
    .await;
    Ok(match result {
        Ok(value) => value,
        Err(error) => json!({"status":"error","error_kind":"mode_change_rejected",
            "output":error.to_string()}),
    })
}
/// The guidance text for `mode`: its label and prompt.
#[must_use]
pub fn guidance_text(mode: &Value) -> String {
    format!(
        "Execution mode: {}\n{}",
        mode["definition"]["label"].as_str().unwrap_or("Unknown"),
        mode["definition"]["prompt"].as_str().unwrap_or("").trim()
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_cannot_select_another_session() {
        assert!(serde_json::from_value::<Args>(json!({"mode":"auto",
            "expected_revision":0,"session_id":"other"}))
        .is_err());
        assert!(serde_json::from_value::<Args>(json!({"mode":"plan"})).is_ok());
    }
}
