//! set_effort: validate a reasoning effort for the run's current model.
use crate::NativeHost;
use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};
use webagent_models::provider;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Effort {
    effort: String,
}
pub(super) fn run(
    host: &dyn NativeHost,
    run: &Value,
    args: &Value,
    state: &mut Value,
) -> Result<()> {
    let q: Effort = serde_json::from_value(args.clone())?;
    let selection =
        provider::ProviderSelection::parse(&run["provider_selection"], host.default_agent_model())?;
    webagent_models::transport::validate_reasoning_effort(
        selection.provider,
        &selection.model,
        Some(&q.effort),
    )?;
    state["state"]["effort"] = json!(q.effort);
    Ok(())
}
