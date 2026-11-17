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
    expected_revision: u64,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    plan: Option<Plan>,
}
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
        let response = host
            .change_session_mode(&agent, call, mode, query.expected_revision)
            .await?
            .map_err(|_| anyhow::anyhow!("Session mode revision or access changed"))?;
        if let Some(artifact) = artifact {
            host.write_session_artifact(call, PLAN_ARTIFACT, PLAN_ARTIFACT, &artifact)
                .await?;
        }
        let mut result = json!({"status":"success","output":response});
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
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_cannot_select_another_session() {
        assert!(serde_json::from_value::<Args>(json!({"mode":"auto",
            "expected_revision":0,"session_id":"other"}))
        .is_err());
    }
}
