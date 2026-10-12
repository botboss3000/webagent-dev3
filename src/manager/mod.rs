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
// The review remains in the conversation owner's store, but account/model grants
// belong to the trusted requester persisted by message admission.
async fn review_scope(app: &App, record: &Value) -> Result<App> {
    let sid = record["session_id"].as_str().ok_or_else(|| anyhow!("Review session missing"))?;
    if record["run"]["session_id"].as_str() != Some(sid)
        || record["run"]["agent_id"] != record["agent_id"]
    {
        return Err(anyhow!("Review identity mismatch"));
    }
    let requester = record["run"]["requester_user_id"].as_str()
        .unwrap_or(app.store.current_user_id()).to_owned();
    let mut actor = app.clone();
    actor.chat_actor = Some(requester);
    let scoped = crate::shared_chat::scope(actor, sid).await?;
    let access = scoped.chat_access.as_ref().ok_or_else(|| anyhow!("Review access missing"))?;
    if !access.can_write() || access.agent_id != record["agent_id"].as_str().unwrap_or("")
        || access.owner_id != app.store.current_user_id()
    {
        return Err(anyhow!("Review authority changed"));
    }
    Ok(scoped)
}
fn system_proxy(app: &App, record: &Value) -> Value {
    json!({"agent_id":record["agent_id"],"session_id":record["session_id"],
        "requester_user_id":record["run"]["requester_user_id"].as_str().unwrap_or(crate::shared_chat::actor(app)),
        "participation_generation":record["run"]["participation_generation"],
        "selection_plan":record["selection_plan"],"failover_state":record["failover_state"]})
}
async fn participation_authorized(app: &App, record: &Value) -> Result<()> {
    if !app.store.participation_allows_run(&record["run"]).await? {
        return Err(anyhow!("Agent participation changed"));
    }
    if !app.store.requester_claim_ready(&record["run"]).await? {
        return Err(anyhow!("Requester run quota changed"));
    }
    // Membership may have changed during credential refresh or a provider call.
    review_scope(app, record).await?;
    Ok(())
}
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

pub(crate) async fn judge_followup(app:&App,record:&mut Value)->Result<Verdict> {review::judge(app,record).await}
