//! list_models: the agent's model roster with connection state, efforts and roles.
use crate::NativeHost;
use anyhow::Result;
use serde_json::{json, Value};
use webagent_models::model_selection::Catalog;

pub(super) async fn run(host: &dyn NativeHost, catalog: &Catalog, agent: &str) -> Result<Value> {
    let mut models = vec![];
    if let Some(roster) = catalog.agent_models.get(agent) {
        for id in &roster.entry_ids {
            let Ok(mut plan) = catalog.resolve_custom(agent, id) else {
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
        .get(agent)
        .map(|r| {
            r.roles
                .iter()
                .map(|(role, a)| json!({"role":role.label(),"entry_id":a.entry_id}))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(json!({"status":"success","output":{"models":models,"roles":roles,"scope":"current_run"}}))
}
