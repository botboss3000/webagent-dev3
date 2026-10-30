//! Run-scoped model changes use only the agent's existing roster and live Models authority.
use crate::{Call, NativeHost};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_host_core::digest;
use webagent_models::{model_selection, provider};

/// Whether this module executes tool `name`.
#[must_use]
pub fn owns(name: &str) -> bool {
    matches!(
        name,
        "list_models" | "set_model" | "use_premium_model" | "set_effort" | "reset_to_default"
    )
}
/// Tools this module executes; their schemas live in the ability's `tools/<tool>/tool.json`.
pub const TOOLS: &[&str] = &[
    "list_models",
    "set_model",
    "use_premium_model",
    "set_effort",
    "reset_to_default",
];
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Model {
    entry_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Effort {
    effort: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
fn state_id(rid: &str) -> String {
    format!("core_model:{rid}")
}
fn permission_to_switch(run: &Value) -> Result<()> {
    if run["post_run_followup"] == true || run["optimization"].is_object() {
        return Err(anyhow!("This restricted run cannot change its model"));
    }
    Ok(())
}
fn premium_entry(catalog: &model_selection::Catalog, agent: &str, entry: &str) -> bool {
    let Some(roster) = catalog.agent_models.get(agent) else {
        return false;
    };
    if roster
        .roles
        .get(&model_selection::ModelRole::Standard)
        .is_some_and(|a| a.entry_id == entry)
    {
        return false;
    }
    roster
        .roles
        .get(&model_selection::ModelRole::Premium)
        .is_some_and(|a| a.entry_id == entry || a.backup_entry_ids.iter().any(|id| id == entry))
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
        let mut models = vec![];
        if let Some(roster) = catalog.agent_models.get(&agent) {
            for id in &roster.entry_ids {
                let Ok(mut plan) = catalog.resolve_custom(&agent, id) else {
                    continue;
                };
                if host.pin_model_plan(&mut plan).await.is_err() {
                    continue;
                }
                let entry = &catalog.entries[id];
                let first = &plan.candidates[0];
                let efforts = webagent_models::providers::efforts(
                    first.transport.provider_id(),
                    &entry.upstream_model,
                )
                .unwrap_or_default();
                models.push(json!({"entry_id":id,"name":entry.display_name,"model":entry.upstream_model,
     "capabilities":entry.capabilities,"connected":first.platform.is_some()||host.store().get(&first.credential_ref).await?.is_some(),
     "efforts":efforts.levels,"default_effort":first.default_effort.clone().or(efforts.default)}));
            }
        }
        let roles = catalog
            .agent_models
            .get(&agent)
            .map(|r| {
                r.roles
                    .iter()
                    .map(|(role, a)| json!({"role":role.label(),"entry_id":a.entry_id}))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        return Ok(
            json!({"status":"success","output":{"models":models,"roles":roles,"scope":"current_run"}}),
        );
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
        "set_model" => {
            let q: Model = serde_json::from_value(args.clone())?;
            let mut plan = catalog.resolve_custom(&agent, &q.entry_id)?;
            host.pin_model_plan(&mut plan).await?;
            let premium = premium_entry(&catalog, &agent, &q.entry_id);
            if premium && run["pending"]["human_approved"] != true {
                return Err(anyhow!(
                    "Premium model selection requires exact human approval"
                ));
            }
            state["state"]["selection"] = json!({"entry_id":q.entry_id,"premium_approved":premium});
        }
        "use_premium_model" => {
            let _: Empty = serde_json::from_value(args.clone())?;
            if run["pending"]["human_approved"] != true {
                return Err(anyhow!("Premium role requires exact human approval"));
            }
            let mut plan = catalog.resolve_role(&agent, model_selection::ModelRole::Premium)?;
            host.pin_model_plan(&mut plan).await?;
            state["state"]["selection"] = json!({"role":"premium"});
        }
        "reset_to_default" => {
            let _: Empty = serde_json::from_value(args.clone())?;
            let mut plan = catalog.resolve_role(&agent, model_selection::ModelRole::Standard)?;
            host.pin_model_plan(&mut plan).await?;
            state["state"]["selection"] = json!({"role":"standard"});
            state["state"]["effort"] = Value::Null;
        }
        "set_effort" => {
            let q: Effort = serde_json::from_value(args.clone())?;
            let selection = provider::ProviderSelection::parse(
                &run["provider_selection"],
                host.default_agent_model(),
            )?;
            webagent_models::transport::validate_reasoning_effort(
                selection.provider,
                &selection.model,
                Some(&q.effort),
            )?;
            state["state"]["effort"] = json!(q.effort);
        }
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
/// Mark a premium model change on `run`'s pending call as needing new-intent confirmation.
pub async fn require_premium_confirmation(
    host: &dyn NativeHost,
    run: &mut Value,
    name: &str,
    args: &Value,
) -> Result<()> {
    if name == "use_premium_model"
        || name == "set_model"
            && premium_entry(
                &host.model_catalog().await?,
                run["agent_id"].as_str().unwrap_or("shared_default"),
                args["entry_id"].as_str().unwrap_or(""),
            )
    {
        run["pending"]["new_intent_confirmation"] = json!(true);
    }
    Ok(())
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
            if premium_entry(&catalog, agent, entry) && selection["premium_approved"] != true {
                return Err(anyhow!("Selected entry now requires Premium approval"));
            }
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
            limits: first.limits.clone(),
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
