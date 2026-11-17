//! An agent operator's decision on the fix proposed after a context error: raise that agent's
//! compaction trigger to the measured minimum. The failed run stays immutable; the decision
//! is its own record, read back onto the run's notice when the session loads.
use super::{bad, conflict, json, now, ApiError, App, Json, StatusCode, Value};

type Answer = std::result::Result<Json<Value>, ApiError>;

fn decision_key(rid: &str) -> String {
    format!("context_remedy:{rid}")
}

/// `POST /sessions/{id}/runs/{run}/context-remedy` with `{"decision":"approve"|"deny"}`.
/// Only an operator of the run's agent may decide; approval changes that agent only.
pub(crate) async fn decide(app: App, sid: String, rid: String, body: Value) -> Answer {
    let decision = body["decision"]
        .as_str()
        .filter(|d| matches!(*d, "approve" | "deny"))
        .ok_or_else(|| bad("Decision must be approve or deny"))?
        .to_owned();
    if body.as_object().is_none_or(|fields| fields.len() != 1) {
        return Err(bad("Unknown request field"));
    }
    let not_found = || ApiError(StatusCode::NOT_FOUND, "Run not found".into());
    let run = app.run(&rid).await.map_err(|_| not_found())?;
    if run["session_id"] != sid.as_str() {
        return Err(not_found());
    }
    let remedy = &run["context_remedy"];
    let (Some(agent), Some(to)) = (remedy["agent_id"].as_str(), remedy["to_tokens"].as_u64())
    else {
        return Err(conflict("This run has no proposed fix"));
    };
    if remedy["kind"] != "raise_compaction_trigger"
        || run["agent_id"].as_str().unwrap_or("shared_default") != agent
    {
        return Err(conflict("This run has no proposed fix"));
    }
    let actor = crate::shared_chat::actor(&app).to_owned();
    if !app
        .store
        .agent_authority(&actor, agent)
        .await
        .is_ok_and(|rights| rights["can_manage"] == true)
    {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Only an agent operator can decide this fix".into(),
        ));
    }
    if app.store.get(&decision_key(&rid)).await?.is_some() {
        return Err(conflict("This fix was already decided"));
    }
    let state = if decision == "approve" {
        crate::agents_api::raise_compaction_trigger(&app, agent, to as usize).await?;
        "approved"
    } else {
        "denied"
    };
    let record = json!({"state":state,"to_tokens":to,"decided_by":actor,"decided_at":now()});
    app.store.put(&decision_key(&rid), &record).await?;
    Ok(Json(record))
}

/// Show each notice's recorded decision instead of the proposal it was published with.
pub(crate) async fn decorate(app: &App, messages: &mut [Value]) -> anyhow::Result<()> {
    for item in messages.iter_mut() {
        if item["role"] != "notice" || !item["content"]["remedy"].is_object() {
            continue;
        }
        let Some(rid) = item["run_id"].as_str() else {
            continue;
        };
        if let Some(decision) = app.store.get(&decision_key(rid)).await? {
            item["content"]["remedy"]["state"] = decision["state"].clone();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A failed run of the conversation's agent carrying a proposed trigger raise.
    async fn failed_run(app: &App, sid: &str) -> (String, String) {
        let agent = super::super::session_agent_id(app, sid).await.unwrap();
        let rid = "remedy-run".to_owned();
        app.store
            .put(
                &format!("run:{rid}"),
                &json!({"id":rid,"session_id":sid,"agent_id":agent,"status":"failed",
                    "context_remedy":{"kind":"raise_compaction_trigger","agent_id":agent,
                        "from_tokens":60000,"to_tokens":70000,"state":"proposed"}}),
            )
            .await
            .unwrap();
        (agent, rid)
    }

    /// The agent override another session's test set: trigger 60,000, tail 2,000.
    async fn low_trigger(app: &App, agent: &str) {
        let store = app.store.for_agent(agent).await.unwrap();
        let raw: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
            .bind(agent)
            .fetch_one(&store.agent_pool)
            .await
            .unwrap();
        let mut metadata = store.unseal_agent(&raw).unwrap();
        metadata["compaction_thresholds"] = json!({"trigger_tokens":60000,"tail_tokens":2000});
        sqlx::query("UPDATE agents SET metadata=? WHERE id=?")
            .bind(store.seal_agent(&metadata).unwrap())
            .bind(agent)
            .execute(&store.agent_pool)
            .await
            .unwrap();
    }

    async fn agent_trigger(app: &App, agent: &str) -> Value {
        let store = app.store.for_agent(agent).await.unwrap();
        let raw: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
            .bind(agent)
            .fetch_one(&store.agent_pool)
            .await
            .unwrap();
        store.unseal_agent(&raw).unwrap()["compaction_thresholds"].clone()
    }

    /// Approval raises only that agent's trigger, once; a second decision is refused and the
    /// notice shows the recorded state.
    #[tokio::test]
    async fn an_operator_approves_once_and_the_notice_shows_it() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let (agent, rid) = failed_run(&app, &sid).await;
        low_trigger(&app, &agent).await;
        let bad_body = decide(
            app.clone(),
            sid.clone(),
            rid.clone(),
            json!({"decision":"maybe"}),
        );
        assert!(bad_body.await.is_err());
        let answer = decide(
            app.clone(),
            sid.clone(),
            rid.clone(),
            json!({"decision":"approve"}),
        )
        .await
        .unwrap();
        assert_eq!(answer.0["state"], "approved");
        let thresholds = agent_trigger(&app, &agent).await;
        assert_eq!(
            (
                thresholds["trigger_tokens"].as_u64(),
                thresholds["tail_tokens"].as_u64()
            ),
            (Some(70000), Some(2000))
        );
        let again = decide(
            app.clone(),
            sid.clone(),
            rid.clone(),
            json!({"decision":"deny"}),
        );
        assert!(again.await.is_err());
        let mut messages = vec![json!({"role":"notice","run_id":rid,
            "content":{"status":"failed","remedy":{"state":"proposed"}}})];
        super::decorate(&app, &mut messages).await.unwrap();
        assert_eq!(messages[0]["content"]["remedy"]["state"], "approved");
    }

    /// A denial changes no setting, and a run from another conversation is not found.
    #[tokio::test]
    async fn a_denial_leaves_the_agent_unchanged() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let (agent, rid) = failed_run(&app, &sid).await;
        low_trigger(&app, &agent).await;
        let elsewhere = decide(
            app.clone(),
            "other".into(),
            rid.clone(),
            json!({"decision":"deny"}),
        );
        assert!(elsewhere.await.is_err());
        let answer = decide(app.clone(), sid, rid, json!({"decision":"deny"}))
            .await
            .unwrap();
        assert_eq!(answer.0["state"], "denied");
        assert_eq!(agent_trigger(&app, &agent).await["trigger_tokens"], 60000);
    }
}
