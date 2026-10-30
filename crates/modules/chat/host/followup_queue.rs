//! Durable followup admission and safe delivery at Rust execution boundaries.
use crate::*;
fn successor_id(app: &App, sid: &str, entry: &Value) -> Result<String> {
    webagent_engine::runtime::followup::successor_id(app.store.current_user_id(),sid,entry)
}
pub(crate) use webagent_chat::followup::{public, public_entry};
pub(crate) async fn should_queue(app: &App, s: &str) -> Result<bool> {
    let q = app.store.followup_queue(s).await?;
    Ok(q["held"] == true
        || q["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| matches!(e["status"].as_str(), Some("pending" | "reviewing" | "held")))
        || app
            .store
            .get(&format!("manager_pending:{s}"))
            .await?
            .is_some()
        || app
            .store
            .active_runs()
            .await?
            .iter()
            .any(|r| r["session_id"] == s && !terminal(status(r))))
}
pub(crate) fn prepared_entry(prepared:api::chat::PreparedMessage,choice:&str)->(Value,Vec<(String,Value)>) {
    let id = prepared.message["run_id"].as_str().unwrap().to_owned();
    let mut template = Value::Null;
    let mut review = Value::Null;
    let mut docs = vec![];
    for (key, value) in prepared.docs {
        if key.starts_with("run:") {
            template = value;
        } else if key.starts_with("manager_review:") {
            template = value["run"].clone();
            review = value;
        } else if !key.starts_with("manager_pending:") {
            docs.push((key, value));
        }
    }
    let e = json!({"id":id,"message_id":format!("user:{id}"),"session_id":template["session_id"],"agent_id":template["agent_id"],"revision":0,"choice":choice,"status":if review.is_null(){"pending"}else{"reviewing"},"content":prepared.message["content"],"context_content":prepared.message.get("context_content").unwrap_or(&prepared.message["content"]),"attachments":prepared.result["attachments"],"agent_message":prepared.message["agent_message"],"template":template,"review":review,"review_status":if review.is_null(){"acceptable"}else{"pending_review"},"created_at":now(),"consuming_run_id":null});
    (e,docs)
}
pub(crate) async fn submit(
    app: &App,
    s: &str,
    body: &Value,
) -> std::result::Result<Value, ApiError> {
    if crate::shared_chat::actor(app)!=app.store.current_user_id(){return Err(ApiError(StatusCode::FORBIDDEN,"Private delivery controls belong to the conversation owner".into()));}
    let prepared = api::chat::prepare_followup(app, s, body).await?;
    let (e,docs)=prepared_entry(prepared,"queue");
    let id=e["id"].as_str().unwrap().to_owned();
    let result = json!({"run_id":null,"submission_id":id,"message_id":e["message_id"],"delivery_state":"queued","queue_entry":public_entry(&e),"attachments":e["attachments"]});
    app.store
        .followup_submit(
            s,
            e,
            docs,
            body["client_message_id"].as_str().unwrap(),
            &digest(body),
            &result,
        )
        .await
        .map_err(|e| conflict(&e.to_string()))
}
pub(crate) async fn interrupt_requested(app: &App, rid: &str) -> Result<bool> {
    app.engine().interrupt_requested(rid).await
}
pub(crate) async fn apply_steering(app: &App, run: &mut Value) -> Result<bool> {
    app.engine().apply_steering(run).await
}
pub(crate) async fn finish_interrupt(app: &App, run: &mut Value) -> Result<bool> {
    app.engine().finish_interrupt(run).await
}
async fn review_entry(app: App, s: String, e: Value) -> Result<()> {
    static REVIEW_LIMIT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
    let Ok(_permit) = REVIEW_LIMIT.try_acquire() else {
        return Ok(());
    };
    let id = e["id"].as_str().unwrap();
    let revision = e["revision"].as_u64().unwrap_or(0);
    if e["template"]["requester_user_id"].as_str().is_some_and(|actor|actor!=app.store.current_user_id()){return Ok(());}
    if !app.store.participation_allows_run(&e["template"]).await? {return Ok(());}
    let mut quota=e["template"].clone();quota["id"]=json!(format!("followup-review:{}:{}:{id}",app.store.current_user_id(),s));
    match app.store.requester_claim_ready(&quota).await {
        Ok(true)=>{},
        Ok(false)=>return Ok(()),
        Err(error) if error.to_string()=="Account run limit reached"=>return Ok(()),
        Err(error)=>return Err(error),
    }
    let mut r = e["review"].clone();
    r["run"]["id"]=quota["id"].clone();
    r["run"]["requester_user_id"]=json!(app.store.current_user_id());
    r["status"] = json!("reviewing");
    r["deadline"] = json!(now() + 50);
    if !app
        .store
        .followup_review_cas(&s, id, revision, "reviewing", Some(&r))
        .await?
    {
        return Ok(());
    }
    let verdict = tokio::time::timeout(
        Duration::from_secs(45),
        crate::manager::judge_followup(&app, &mut r),
    )
    .await;
    let (state, review_status) = match verdict {
        Ok(Ok(crate::manager::Verdict::Acceptable)) => ("pending", "acceptable"),
        Ok(Ok(crate::manager::Verdict::Unacceptable)) => ("rejected", "unacceptable"),
        _ => ("held", "review_error"),
    };
    r["status"] = json!(review_status);
    // Choice changes during review must survive; a fresh revision only changes the
    // gate result, and cancellation always wins over a late provider verdict.
    let current = app.store.followup_queue(&s).await?;
    if let Some(current) = current["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id)
    {
        app.store
            .followup_review_cas(
                &s,
                id,
                current["revision"].as_u64().unwrap_or(0),
                state,
                Some(&r),
            )
            .await?;
    }
    app.store.reconcile_requester_slots().await?;
    Ok(())
}
pub(crate) async fn scheduler_tick(app: &App) -> Result<()> {
    let queues = app.store.list("followup_queue:").await?;
    for q in queues {
        let s = q["session_id"].as_str().unwrap_or("").to_owned();
        if s.is_empty() {
            continue;
        }
        // Archived or deleted chats retain history, but cannot dispatch work.
        // Skip them here so one inactive ledger cannot stall other conversations.
        let Some(session) = app.store.get(&format!("session:{s}")).await? else {
            continue;
        };
        if session["status"].as_str().unwrap_or("active") != "active" || app.store.participation_state(&s).await?["active"]!=true {
            continue;
        }
        crate::session_messaging::revalidate(app,&s).await?;
        let q = app.store.followup_queue(&s).await?;
        for e in q["entries"].as_array().unwrap() {
            if e["review_status"] == "pending_review" {
                tokio::spawn(review_entry(app.clone(), s.clone(), e.clone()));
            } else if e["review_status"] == "reviewing"
                && e["review"]["deadline"].as_u64().unwrap_or(0) <= now()
            {
                let mut r = e["review"].clone();
                r["status"] = json!("review_error");
                app.store
                    .followup_review_cas(
                        &s,
                        e["id"].as_str().unwrap(),
                        e["revision"].as_u64().unwrap_or(0),
                        "held",
                        Some(&r),
                    )
                    .await?;
            }
        }
        if app.shutoff().await? {
            app.store
                .followup_hold(&s, "Global shutoff is active")
                .await?;
            continue;
        }
        let active = app.store.active_runs().await?;
        if let Some(r) = active.iter().find(|r| r["session_id"] == s) {
            if matches!(
                status(r),
                "stopped" | "paused" | "recovery" | "waiting_external"
            ) {
                app.store
                    .followup_hold(
                        &s,
                        if r["uncertain"] == true {
                            "Existing write outcome requires recovery"
                        } else {
                            "Existing work must be resolved before queue delivery"
                        },
                    )
                    .await?;
            }
            continue;
        }
        if app
            .store
            .get(&format!("manager_pending:{s}"))
            .await?
            .is_some()
        {
            continue;
        }
        if let Some(interrupt) = q["entries"].as_array().unwrap().iter().find(|e| {
            e["choice"] == "interrupt"
                && e["status"] == "pending"
                && e["review_status"] == "acceptable"
        }) {
            if let Some(target) = interrupt["target_run_id"].as_str() {
                if let Some(mut parent) = app.store.get(&format!("run:{target}")).await? {
                    if matches!(status(&parent), "cancelled" | "failed") {
                        if parent["recovery_abandoned"] == true {
                            crate::agent_loop::settle_abandoned_interrupt(&mut parent)?;
                        }
                        if !crate::agent_loop::settle_followup_interrupt(app, &mut parent).await? {
                            let current = app.store.followup_queue(&s).await?;
                            if current["held"] != true
                                && current["entries"].as_array().unwrap().iter().any(|e| {
                                    e["choice"] == "interrupt"
                                        && e["status"] == "pending"
                                        && e["review_status"] == "acceptable"
                                        && (e["target_run_id"].is_null()
                                            || e["target_run_id"] == parent["id"])
                                })
                            {
                                app.store
                                    .followup_hold(
                                        &s,
                                        "Interrupted work must be resolved before message delivery",
                                    )
                                    .await?;
                            }
                        }
                        // A blocked targeted handoff must never fall through to
                        // fresh consumption that drops its original tool checkpoint.
                        continue;
                    }
                }
            }
        }
        if q["held"] == true {
            continue;
        }
        let latest = app.store.followup_last_run(&s).await?;
        if latest.as_ref().is_some_and(|r| {
            matches!(status(r), "failed" | "cancelled")
                && r["interrupted_by"].is_null()
                && q["resumed_after_run_id"] != r["id"]
        }) {
            app.store
                .followup_hold(
                    &s,
                    "Previous run stopped or failed; resume the queue when ready",
                )
                .await?;
            continue;
        }
        let choice = if q["entries"].as_array().unwrap().iter().any(|x| {
            x["choice"] == "interrupt"
                && x["status"] == "pending"
                && x["review_status"] == "acceptable"
        }) {
            "interrupt"
        } else {
            "queue"
        };
        let Some(e) = q["entries"].as_array().unwrap().iter().find(|e| {
            e["status"] == "pending"
                && e["review_status"] == "acceptable"
                && (e["choice"] == choice || (choice == "queue" && e["choice"] == "steer"))
        }) else {
            continue;
        };
        let agent = e["agent_id"].as_str().unwrap_or("shared_default");
        let access = app
            .store
            .live_access(app.store.current_user_id(), agent)
            .await?;
        if !access.admitted() {
            app.store
                .followup_hold(&s, "Agent access unavailable")
                .await?;
            continue;
        }
        if active.len() >= app.runtime.read().await.scheduler.max_unresolved_runs
            || active.len() >= access.max_active_runs() as usize
        {
            continue;
        }
        let mut next = e["template"].clone();
        if app.run_candidate(&next).await.is_err() {
            app.store
                .followup_hold(&s, "Model unavailable; restore access then resume queue")
                .await?;
            continue;
        }
        next["id"] = json!(successor_id(app,&s,&e)?);
        next["created_at"] = json!(now());
        next["updated_at"] = json!(now());
        // An interrupt selected just as its original run completed becomes the next
        // input; only that entry is consumed, ordinary queue entries remain pending.
        if choice == "interrupt" {
            let mut input = app.store.context(&s).await?;
            webagent_engine::runtime::run::pair_unresolved_calls(&mut input);
            next["input"] = json!(input);
            next["context_hydration_pending"] = json!(false);
        }
        app.store
            .followup_consume(&s, choice, &mut next, None)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    #[tokio::test]
    async fn inactive_ledgers_cannot_block_other_sessions_or_create_orphan_runs() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        for sid in ["a_archived", "b_deleted", "c_active"] {
            app.store
                .put(
                    &format!("session:{sid}"),
                    &json!({"id":sid,"status":"active","agent_id":"shared_default"}),
                )
                .await
                .unwrap();
            let e = json!({"id":format!("message_{sid}"),"message_id":format!("user:message_{sid}"),"session_id":sid,"agent_id":"shared_default","revision":0,"choice":"queue","status":"pending","review_status":"acceptable","content":"pending","context_content":"pending","attachments":[]});
            app.store
                .followup_submit(sid, e, vec![], "submit", "digest", &json!({}))
                .await
                .unwrap();
        }
        app.store
            .put(
                "session:a_archived",
                &json!({"id":"a_archived","status":"archived"}),
            )
            .await
            .unwrap();
        sqlx::query("DELETE FROM docs WHERE key='session:b_deleted'")
            .execute(&app.store.pool)
            .await
            .unwrap();
        app.store
            .put("control", &json!({"shutoff":true}))
            .await
            .unwrap();
        scheduler_tick(&app).await.unwrap();
        assert_eq!(
            app.store.followup_queue("c_active").await.unwrap()["held"],
            true
        );
        for sid in ["a_archived", "b_deleted"] {
            assert_eq!(app.store.followup_queue(sid).await.unwrap()["held"], false);
            assert!(app
                .store
                .followup_hold(sid, "must not dispatch")
                .await
                .is_err());
        }
        assert!(app.store.active_runs().await.unwrap().is_empty());
    }
}
