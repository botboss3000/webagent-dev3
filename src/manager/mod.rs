//! Independent V3 manager. Reviews are not agent runs and never enter agent history.
use crate::*;
pub mod config;
mod gates;
mod review;
mod scheduler;
#[cfg(test)]
mod tests;
mod verdict;
pub(crate) use scheduler::scheduler;
pub(crate) use verdict::Verdict;
pub(crate) async fn prepare(app: &App, agent: &str) -> Result<Option<Value>> {
    let (settings, settings_revision) = config::snapshot(&app.store, agent).await?;
    if !settings.enabled {
        return Ok(None);
    }
    let mut plan = app
        .store
        .model_catalog()
        .await?
        .resolve_role(agent, model_selection::ModelRole::System)?;
    app.pin_model_plan(&mut plan).await?;
    Ok(Some(
        json!({"settings":settings,"settings_revision":settings_revision,"selection_plan":plan,
        "failover_state":model_selection::FailoverState::default()}),
    ))
}
pub(crate) fn public(record: &Value) -> Value {
    json!({"review_id":record["id"],"gate":"incoming_message","status":record["status"],
        "verdict":record["verdict"],"error":record["error"],"usage":usage::public(&record["usage"]),
        "run_id":if record["status"]=="acceptable" {record["id"].clone()} else {Value::Null}})
}
pub(crate) async fn decorate(app: &App, messages: &mut [Value]) -> Result<()> {
    for message in messages.iter_mut().filter(|m| m["role"] == "user") {
        if let Some(rid) = message["run_id"].as_str() {
            if let Some(record) = app.store.get(&format!("manager_review:{rid}")).await? {
                message["admission"] = public(&record);
            }
        }
    }
    Ok(())
}
