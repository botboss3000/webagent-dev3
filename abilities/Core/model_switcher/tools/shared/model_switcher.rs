//! Run-scoped model changes use only the agent's existing roster and live Models authority.
use crate::{Call, NativeHost};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_host_core::digest;
use webagent_models::{model_selection, provider};

#[path = "../list_models/list_models.rs"]
mod list_models;
#[path = "../reset_to_default/reset_to_default.rs"]
mod reset_to_default;
#[path = "../set_effort/set_effort.rs"]
mod set_effort;
#[path = "../set_model/set_model.rs"]
mod set_model;
#[path = "../use_premium_model/use_premium_model.rs"]
mod use_premium_model;

/// Whether this module executes tool `name`.
#[must_use]
pub fn owns(name: &str) -> bool {
    matches!(
        name,
        "list_models" | "set_model" | "use_premium_model" | "set_effort" | "reset_to_default"
    )
}
/// Tools this module executes; their schemas live in the ability's `tools/<tool>/<tool>.tool.json`.
pub const TOOLS: &[&str] = &[
    "list_models",
    "set_model",
    "use_premium_model",
    "set_effort",
    "reset_to_default",
];
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
/// Argument types of this family's tools, checked against their tool.json by the crate test.
pub const ARGUMENT_TYPES: &[webagent_abilities::tool_arguments::ArgumentType] = &[
    webagent_abilities::argument_type!("reset_to_default", Empty),
    webagent_abilities::argument_type!("set_effort", set_effort::Effort),
    webagent_abilities::argument_type!("set_model", set_model::Model),
    webagent_abilities::argument_type!("use_premium_model", Empty),
];
fn state_id(rid: &str) -> String {
    format!("core_model:{rid}")
}
fn permission_to_switch(run: &Value) -> Result<()> {
    if run["optimization"].is_object() {
        return Err(anyhow!("This restricted run cannot change its model"));
    }
    Ok(())
}
/// Execute model control `name` at `call`'s position.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let (sid, rid, pending) = (call.sid, call.run_id, call.pending_id);
    let run = host.authorize(call, name, args, &cancel).await?;
    permission_to_switch(&run)?;
    let agent = host.session_agent_id(sid).await?;
    let catalog = host.model_catalog().await?;
    if name == "list_models" {
        return list_models::run(host, &catalog, &agent).await;
    }
    let _guard = host.gate().lock().await;
    host.authorize(call, name, args, &cancel).await?;
    let mut state = host.store().module_state(&state_id(rid)).await?;
    let invocation = digest(&json!([sid, rid, pending]));
    let fingerprint = digest(&json!([name, args]));
    if let Some(prior) = state["state"]["receipts"].get(&invocation) {
        if prior["fingerprint"] != fingerprint {
            return Err(anyhow!("Model invocation reused with different input"));
        }
        return Ok(json!({"status":"success","output":prior["result"]}));
    }
    if !state["state"].is_object() {
        state["state"] = json!({});
    }
    if !state["state"]["receipts"].is_object() {
        state["state"]["receipts"] = json!({});
    }
    match name {
        "set_model" => set_model::run(host, &catalog, &agent, args, &mut state).await?,
        "use_premium_model" => {
            use_premium_model::run(host, &catalog, &agent, args, &mut state).await?;
        }
        "reset_to_default" => {
            reset_to_default::run(host, &catalog, &agent, args, &mut state).await?;
        }
        "set_effort" => set_effort::run(host, &run, args, &mut state)?,
        _ => return Err(anyhow!("Unknown model control")),
    }
    let result = json!({"accepted":true,"applies":"next_model_call","scope":"current_run","selection":state["state"]["selection"],"effort":state["state"]["effort"]});
    state["state"]["receipts"][invocation] = json!({"fingerprint":fingerprint,"result":result});
    host.store()
        .module_cas(
            &state_id(rid),
            state["revision"].as_u64().unwrap_or(0),
            &state,
        )
        .await?;
    Ok(json!({"status":"success","output":result}))
}
/// Apply accepted overrides once at a model boundary; System and later runs keep their own plans.
pub async fn apply(host: &dyn NativeHost, run: &mut Value) -> Result<bool> {
    let Some(rid) = run["id"].as_str() else {
        return Ok(false);
    };
    let state = host.store().module_state(&state_id(rid)).await?;
    let revision = state["revision"].as_u64().unwrap_or(0);
    if revision == run["core_model_revision"].as_u64().unwrap_or(0) {
        return Ok(false);
    }
    permission_to_switch(run)?;
    let agent = run["agent_id"]
        .as_str()
        .ok_or_else(|| anyhow!("Run agent missing"))?;
    let catalog = host.model_catalog().await?;
    let selection = &state["state"]["selection"];
    if selection.is_object() {
        let mut plan = if let Some(entry) = selection["entry_id"].as_str() {
            catalog.resolve_custom(agent, entry)?
        } else {
            catalog.resolve_role(
                agent,
                if selection["role"] == "premium" {
                    model_selection::ModelRole::Premium
                } else {
                    model_selection::ModelRole::Standard
                },
            )?
        };
        host.pin_model_plan(&mut plan).await?;
        let first = &plan.candidates[0];
        run["provider_selection"] = provider::ProviderSelection {
            provider: first.transport.provider_id(),
            model: first.upstream_model.clone(),
        }
        .value();
        run["selection_plan"] = json!(plan);
        run["failover_state"] = json!(model_selection::FailoverState::default());
        // The conversation's effort belonged to the replaced model.
        run["chat_effort"] = Value::Null;
        run["attempt"] = json!(0);
    }
    let current =
        provider::ProviderSelection::parse(&run["provider_selection"], host.default_agent_model())?;
    // An effort chosen for the previous model is dropped when the new one lacks it.
    run["core_model_effort"] = json!(webagent_models::providers::supported(
        current.provider,
        &current.model,
        state["state"]["effort"].as_str(),
    ));
    run["core_model_revision"] = json!(revision);
    Ok(true)
}
