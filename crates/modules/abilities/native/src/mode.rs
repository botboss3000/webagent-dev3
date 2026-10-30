//! A model can change only the authenticated run's own session mode.
//! Elevating read_only to write always needs a fresh human approval.
use crate::{Call, NativeHost};
use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

/// Whether this family executes tool `name`.
#[must_use]
pub fn owns(name: &str) -> bool {
    name == "set_execution_mode"
}
/// The mode tool's schema; the session is always the authenticated run's own.
#[must_use]
pub fn schemas() -> Vec<Value> {
    vec![json!({"type":"function","name":"set_execution_mode",
        "description":"Change this session to an agent execution mode ID (ask, plan, auto, or custom); legacy read_only/write are supported at a revision checkpoint. Moving to write requires human approval.",
        "parameters":{"type":"object","properties":{
            "mode":{"type":"string","maxLength":32},
            "expected_revision":{"type":"integer","minimum":0},
            "reason":{"type":"string","maxLength":500}},
            "required":["mode","expected_revision"],"additionalProperties":false}})]
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    mode: String,
    expected_revision: u64,
    #[serde(default)]
    reason: Option<String>,
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
        let agent = host.session_agent_id(call.sid).await?;
        let response = host
            .change_session_mode(&agent, call, &query.mode, query.expected_revision)
            .await?
            .map_err(|_| anyhow::anyhow!("Session mode revision or access changed"))?;
        Ok(json!({"status":"success","output":response}))
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
        assert_eq!(schemas()[0]["name"], "set_execution_mode");
        assert!(serde_json::from_value::<Args>(json!({"mode":"write",
            "expected_revision":0,"session_id":"other"}))
        .is_err());
    }
}
