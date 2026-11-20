//! Answers to chat panels and requested compaction. A panel answer is checked against the
//! stored panel and admitted as the user's message in the same transaction that marks the
//! panel answered. `/compact` stores a fold request the run loop starts in the background;
//! on an idle chat it starts a compaction-only run.
use super::{
    bad, conflict, json, now, required, status, terminal, uuid, ApiError, App, Json, Result,
    StatusCode, Value,
};

type Answer = std::result::Result<(StatusCode, Json<Value>), ApiError>;

/// Longest `/compact` focus, in characters.
const MAX_FOCUS: usize = 300;

fn only_fields(body: &Value, allowed: &[&str]) -> std::result::Result<(), ApiError> {
    if body
        .as_object()
        .is_none_or(|fields| fields.keys().any(|key| !allowed.contains(&key.as_str())))
    {
        return Err(bad("Unknown request field"));
    }
    Ok(())
}

/// Whether session `sid` has a run that is not finished.
async fn busy(app: &App, sid: &str) -> Result<bool> {
    Ok(app
        .store
        .active_runs()
        .await?
        .iter()
        .any(|run| run["session_id"] == sid && !terminal(status(run))))
}

/// How long `/compact` waits for a run that is ending: a reply is streamed before its run
/// turns terminal, so a command typed right after it would otherwise be scheduled for a
/// model call that never comes.
const SETTLE: std::time::Duration = std::time::Duration::from_secs(2);
const SETTLE_STEP: std::time::Duration = std::time::Duration::from_millis(100);

/// Whether `sid` is still busy after waiting up to [`SETTLE`] for its runs to finish.
async fn busy_after_settling(app: &App, sid: &str) -> Result<bool> {
    let deadline = tokio::time::Instant::now() + SETTLE;
    while busy(app, sid).await? {
        if tokio::time::Instant::now() >= deadline {
            return Ok(true);
        }
        tokio::time::sleep(SETTLE_STEP).await;
    }
    Ok(false)
}

/// The compaction settings `/compact` plans with: the app file plus the session agent's
/// Identity override, as the run loop resolves them.
async fn effective_compaction(app: &App, sid: &str) -> Result<crate::context_compaction::Config> {
    let defaults = crate::context_compaction::Config::read(&app.compaction_config_path)?;
    let agent = super::session_agent_id(app, sid).await?;
    let store = app.store.for_agent(&agent).await?;
    match crate::context_compaction::agent_thresholds(&store, &agent).await? {
        Some(thresholds) => thresholds.apply(&defaults),
        None => Ok(defaults),
    }
}

/// Refuses a second fold while one is requested or running.
async fn compaction_free(app: &App, sid: &str) -> std::result::Result<(), ApiError> {
    let request = app
        .store
        .get(&format!("compaction_request:{sid}"))
        .await?
        .unwrap_or(Value::Null);
    let job = app
        .store
        .get(&format!("context_compaction_job:{sid}"))
        .await?
        .unwrap_or(Value::Null);
    if request["state"] == "pending" || job["state"] == "running" {
        return Err(conflict("A compaction is already running"));
    }
    Ok(())
}

/// Store a `/compact` fold request.
async fn request_fold(app: &App, sid: &str, source: &str, focus: &str, until: Value) -> Result<()> {
    app.store
        .put(
            &format!("compaction_request:{sid}"),
            &json!({"state":"pending","source":source,"focus":focus,"until":until,
                "requested_at":now()}),
        )
        .await
}

/// `POST /sessions/{id}/components/{cid}/answer`: the user's answer to an open choice or
/// form panel at `revision`. The server builds the message text from the stored panel.
pub(crate) async fn answer(app: App, sid: String, cid: String, body: Value) -> Answer {
    only_fields(
        &body,
        &["answer", "revision", "client_message_id", "attachment_ids"],
    )?;
    let request_id = required(&body, "client_message_id", 128)?.to_owned();
    let revision = body["revision"]
        .as_u64()
        .ok_or_else(|| bad("revision required"))?;
    let panel = app
        .store
        .base_component(&sid, &cid)
        .await
        .map_err(|_| bad("Invalid panel"))?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Panel not found".into()))?;
    let card = &panel["artifact"];
    if card["lifecycle"]["user_state"].is_string() || card["lifecycle"]["dismissed"] == true {
        return Err(conflict("Panel already answered"));
    }
    if panel["revision"].as_u64() != Some(revision) {
        return Err(conflict("Panel changed; reload it before answering"));
    }
    let text = webagent_native_tools::task::answer_text(card, &body["answer"])
        .map_err(|error| bad(&error.to_string()))?;
    let mut message = json!({"text":text,"client_message_id":request_id,
        "component_id":cid,"component_revision":revision,"component_answer":body["answer"]});
    if let Some(ids) = body.get("attachment_ids") {
        message["attachment_ids"] = ids.clone();
    }
    let sent = super::chat::send_message(app.clone(), sid.clone(), message).await?;
    let _ = app
        .store
        .base_component_changed(&sid, &cid, revision + 1)
        .await;
    Ok(sent)
}

/// `POST /sessions/{id}/compact`: fold the oldest history now and keep the agent's tail.
/// On a running chat the fold starts at the next model call; on an idle one it runs as a
/// compaction-only run. Nothing outside the tail worth folding is reported, not queued.
pub(crate) async fn compact(app: App, sid: String, body: Value) -> Answer {
    only_fields(&body, &["focus", "client_request_id"])?;
    let request_id = required(&body, "client_request_id", 128)?.to_owned();
    let focus = body["focus"].as_str().unwrap_or("").trim().to_owned();
    if focus.chars().count() > MAX_FOCUS || focus.contains('\n') {
        return Err(bad("focus must be one line of 300 characters or fewer"));
    }
    // Settle before taking the gate: the ending run needs it to finish.
    busy_after_settling(&app, &sid).await?;
    let _guard = app.gate.lock().await;
    compaction_free(&app, &sid).await?;
    let running = busy(&app, &sid).await?;
    if !running {
        let config = effective_compaction(&app, &sid).await?;
        let history = app.store.context(&sid).await?;
        let plan = crate::context_compaction::plan_command(&history, &config.0);
        if !matches!(plan, Ok(Some(_))) {
            let reason = match plan {
                Err(error) => error.to_string(),
                _ => "nothing outside the recent conversation is large enough to fold".into(),
            };
            return Ok((
                StatusCode::OK,
                Json(
                    json!({"status":"nothing_to_fold","notice":format!("Nothing to compact: {reason}")}),
                ),
            ));
        }
    }
    request_fold(&app, &sid, "command", &focus, Value::Null).await?;
    drop(_guard);
    if running {
        return Ok((
            StatusCode::ACCEPTED,
            Json(json!({"status":"scheduled","boundary":"next_model_call"})),
        ));
    }
    let text = if focus.is_empty() {
        "/compact".to_owned()
    } else {
        format!("/compact {focus}")
    };
    let id = format!("compact-{request_id}");
    let id = if id.len() > 128 { uuid() } else { id };
    super::chat::send_message(
        app,
        sid,
        json!({"text":text,"client_message_id":id,"compaction_only":true}),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stored choice panel drawn by call `call-9` of run `r0`.
    async fn panel(app: &App, sid: &str) {
        let spec = json!({"id":"p1","type":"choice","title":"For Bob","placement":"inline",
            "data":{"prompt":"Which?","options":[{"id":"option-1","label":"Yes"},
                {"id":"option-2","label":"No"}]},"lifecycle":{},"authority":"user_reply"});
        app.store
            .put(
                &format!("base_component:{sid}:p1"),
                &json!({"schema_version":1,"revision":1,"kind":"component","id":"p1",
                    "session_id":sid,"updated_by_run":"r0","created_at":1,"after_seq":0,
                    "origin":{"run_id":"r0","call_id":"call-9"},"artifact":spec}),
            )
            .await
            .unwrap();
    }

    fn last_user_text(context: &[Value]) -> String {
        context
            .iter()
            .rev()
            .find(|item| item["role"] == "user")
            .map(|item| item["content"].to_string())
            .unwrap_or_default()
    }

    /// One answer, checked and worded by the server, commits with its message; the
    /// message names the panel and its call, and the panel names the message.
    #[tokio::test]
    async fn a_panel_takes_one_server_built_answer() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        panel(&app, &sid).await;
        let forged = super::super::chat::send(
            crate::State(app.clone()),
            axum::extract::Path(sid.clone()),
            Json(json!({"text":"For Bob: Yes","client_message_id":"m0","component_id":"p1"})),
        )
        .await;
        assert!(forged.is_err());
        let body = json!({"answer":{"option_id":"option-2"},"revision":1,"client_message_id":"m1"});
        assert!(answer(
            app.clone(),
            sid.clone(),
            "p1".into(),
            json!({"answer":{"option_id":"option-7"},"revision":1,"client_message_id":"m2"})
        )
        .await
        .is_err());
        let (_, Json(sent)) = answer(app.clone(), sid.clone(), "p1".into(), body)
            .await
            .unwrap();
        let run_id = sent["run_id"].as_str().unwrap().to_owned();
        let stored = app.store.base_component(&sid, "p1").await.unwrap().unwrap();
        assert_eq!(stored["revision"], 2);
        assert_eq!(
            stored["artifact"]["lifecycle"]["message_run_id"],
            run_id.as_str()
        );
        let context = app.store.context(&sid).await.unwrap();
        let text = last_user_text(&context);
        assert!(text.contains("For Bob: No"), "{text}");
        assert!(
            text.contains("chat panel p1, shown by tool call call-9"),
            "{text}"
        );
        for revision in [1, 2] {
            let again = json!({"answer":{"option_id":"option-1"},"revision":revision,
                "client_message_id":format!("m3-{revision}")});
            assert!(answer(app.clone(), sid.clone(), "p1".into(), again)
                .await
                .is_err());
        }
    }

    /// A dismissal sends nothing; the agent reads it with the next message, once.
    #[tokio::test]
    async fn a_dismissal_reaches_the_agent_as_a_note() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        panel(&app, &sid).await;
        assert!(app
            .store
            .base_component_settle(&sid, "p1", &json!({"option_id":"option-1"}), "x")
            .await
            .is_err());
        assert!(app
            .store
            .base_component_settle(&sid, "p1", &Value::Null, "[dismissed note]")
            .await
            .unwrap());
        crate::runtime_tests::queued(&app, &sid).await;
        let context = app.store.context(&sid).await.unwrap();
        assert!(last_user_text(&context).contains("[dismissed note]"));
        let notes = app.store.get(&format!("panel_notes:{sid}")).await.unwrap();
        assert_eq!(notes.unwrap()["notes"], json!([]));
    }

    /// A reply is streamed before its run turns terminal: `/compact` sent in that moment
    /// waits for the run to end and answers as on an idle chat, rather than scheduling a
    /// fold for a model call that never comes.
    #[tokio::test]
    async fn compact_right_after_a_reply_waits_for_the_ending_run() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let id = crate::runtime_tests::queued(&app, &sid).await;
        let ending = app.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let mut run = ending.run(&id).await.unwrap();
            run["status"] = json!("completed");
            ending
                .store
                .save_run(&mut run, "run_status", &json!({}))
                .await
                .unwrap();
        });
        let (_, Json(answer)) =
            compact(app.clone(), sid.clone(), json!({"client_request_id":"c1"}))
                .await
                .unwrap();
        assert_eq!(answer["status"], "nothing_to_fold");
        assert!(app
            .store
            .get(&format!("compaction_request:{sid}"))
            .await
            .unwrap()
            .is_none());
    }

    /// `/compact` plans with the session agent's Identity override, not the app file alone.
    #[tokio::test]
    async fn compact_uses_the_agents_compaction_override() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let agent = super::super::session_agent_id(&app, &sid).await.unwrap();
        let store = app.store.for_agent(&agent).await.unwrap();
        let raw: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
            .bind(&agent)
            .fetch_one(&store.agent_pool)
            .await
            .unwrap();
        let mut metadata = store.unseal_agent(&raw).unwrap();
        metadata["compaction_thresholds"] = json!({"trigger_tokens":60000,"tail_tokens":1234});
        sqlx::query("UPDATE agents SET metadata=? WHERE id=?")
            .bind(store.seal_agent(&metadata).unwrap())
            .bind(&agent)
            .execute(&store.agent_pool)
            .await
            .unwrap();
        let config = effective_compaction(&app, &sid).await.unwrap();
        assert_eq!((config.trigger_tokens, config.tail_tokens), (60000, 1234));
    }

    /// `/compact` on a short idle chat reports that nothing can fold; on a busy chat it is
    /// scheduled once, and a second request waits for the first.
    #[tokio::test]
    async fn compact_reports_nothing_to_fold_and_runs_one_at_a_time() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let idle = compact(app.clone(), sid.clone(), json!({"client_request_id":"c1"}))
            .await
            .unwrap();
        assert_eq!(idle.1 .0["status"], "nothing_to_fold");
        assert!(app
            .store
            .get(&format!("compaction_request:{sid}"))
            .await
            .unwrap()
            .is_none());
        assert!(compact(
            app.clone(),
            sid.clone(),
            json!({"client_request_id":"c2","focus":"a\nb"})
        )
        .await
        .is_err());
        crate::runtime_tests::queued(&app, &sid).await;
        let busy = compact(
            app.clone(),
            sid.clone(),
            json!({"client_request_id":"c3","focus":"the API design"}),
        )
        .await
        .unwrap();
        assert_eq!(busy.1 .0["status"], "scheduled");
        let request = app
            .store
            .get(&format!("compaction_request:{sid}"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (request["state"].clone(), request["focus"].clone()),
            (json!("pending"), json!("the API design"))
        );
        assert!(
            compact(app.clone(), sid.clone(), json!({"client_request_id":"c4"}))
                .await
                .is_err()
        );
    }
}
