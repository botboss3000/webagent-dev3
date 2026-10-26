//! Optional System-role naming: no chat publication, tools or context mutation.
use crate::*;
pub(crate) mod config;
/// Validate administrator naming settings with the same parser used by queued jobs.
pub(crate) fn validate_settings(value:&Value)->Result<()> {webagent_chat::naming::validate_settings_at(value,None,&crate::live_assets::fallback_directory())}
pub(crate) use webagent_chat::naming::{fallback, validate_source_settings};
use webagent_chat::naming::clean;
fn dispatch(session: &Value, state: &model_selection::FailoverState) -> Result<Value> {
    webagent_chat::naming::dispatch_at(session, state, None, &crate::live_assets::fallback_directory())
}
/// Recheck naming claim, participation, shared-chat access, model authorization and global shutdown before using a private title result.
async fn permitted(app: &App, session: &Value, run: &Value) -> bool {
    !app.stop.is_cancelled()
        && app.shutoff().await.is_ok_and(|value| !value)
        && app
            .store
            .naming_owned(
                session["id"].as_str().unwrap_or(""),
                session["auto_naming"]["claim"].as_str().unwrap_or(""),
            )
            .await
            .unwrap_or(false)
        && app.store.participation_allows_run(run).await.unwrap_or(false)
        && crate::shared_chat::live_access(&app.store,run["requester_user_id"].as_str().unwrap_or(app.store.current_user_id()),
            run["session_id"].as_str().unwrap_or(""),run["agent_id"].as_str().unwrap_or("shared_default")).await.is_ok_and(|access|access.admitted())
        && app.run_model_authorized(run).await
}
/// Reuse the matching pinned naming role or claim-fence a freshly authorized selection plan without changing the chat run's plan.
async fn prepare_plan(app: &App, session: &Value) -> Result<Value> {
    let settings = config::for_session(session)?;
    let existing = serde_json::from_value::<model_selection::SelectionPlan>(
        session["auto_naming"]["system_plan"].clone()).ok();
    if existing.as_ref().is_some_and(|plan| plan.role == settings.model_role) {
        return Ok(session.clone());
    }
    let agent = session["agent_id"].as_str().ok_or_else(|| anyhow!("Missing naming agent"))?;
    let mut plan = app.store.model_catalog().await?.resolve_role(agent, settings.model_role)?;
    app.pin_model_plan(&mut plan).await?;
    if !app.store.naming_pin_plan(session["id"].as_str().unwrap_or(""),
        session["auto_naming"]["claim"].as_str().unwrap_or(""), &json!(plan)).await? {
        return Err(anyhow!("Naming claim unavailable"));
    }
    let mut pinned = session.clone();
    pinned["auto_naming"]["system_plan"] = json!(plan);
    Ok(pinned)
}
// @code-index {"version":1,"feature":"provider-projection","aliases":["provider formatting","pure provider component"],"purpose":"Naming caller prepares provider projection before admission. Config: config/prompts.json -> session_naming (snapshotted per job); engine controls/descriptor fence dispatch.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"config/engine-components.json","pointers":["/disabled","/revision"]},{"path":"plugins/engine/provider-projection/component.json","pointers":["/interface_version","/implementation_version","/limits"]},{"path":"config/prompts.json","pointers":["/session_naming/prompt","/session_naming/model_role","/session_naming/attempts_per_job","/session_naming/retry_delay_ms","/session_naming/request_timeout_seconds","/session_naming/recovery_retry_delay_seconds","/session_naming/max_job_attempts"]}],"opportunities":[]}}
/// Generate a private title from the job's pinned settings and safe failover plan.
/// Calls CallDraft::prepare before dispatch admission; rechecks naming/model/component authority before the first network poll.
async fn generate(app: &App, original: &Value) -> Result<String> {
    let pinned = prepare_plan(app, original).await?;
    let session = &pinned;
    let settings = config::for_session(session)?;
    let sid = session["id"]
        .as_str()
        .ok_or_else(|| anyhow!("Missing naming session"))?;
    let input = vec![json!({"role":"user","content":format!(
        "Here is the first user message of a chat session:\n\n{}\n\nGive a short title for this session.",
        session["auto_naming"]["source"].as_str().unwrap_or("Attachments"))})];
    let mut state = model_selection::FailoverState::default();
    for attempt in 0..settings.attempts_per_job {
        let run = dispatch(session, &state)?;
        if !permitted(app, session, &run).await {
            return Err(anyhow!("Naming authority unavailable"));
        }
        let selection: provider::ProviderSelection =
            serde_json::from_value(run["provider_selection"].clone())?;
        let response = match app.tokens_for(&selection, &run).await {
            Ok(tokens) => {
                if !permitted(app, session, &run).await {
                    return Err(anyhow!("Naming authority unavailable"));
                }
                let (sender, mut receiver) = tokio::sync::mpsc::channel(32);
                let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
                let cancel = app.stop.child_token();
                let request_session = format!("{sid}:title");
                let operation = async {
                    let selection = &selection;
                    let tokens = &tokens;
                    let cancel = cancel.clone();
                    // Projection may use a disposable component; finish IPC before dispatch admission.
                    let engine_revision = crate::engine_components::catalog()?.revision.clone();
                    let engine_controls = crate::engine_components::controls(&app.store.root)?.revision;
                    let prepared = crate::provider::CallDraft::new(
                        selection,
                        input.clone(),
                        Vec::new(),
                        &request_session,
                        &settings.prompt,
                    ).prepare(&app.store.root, &cancel).await?;
                    let dispatch_guard = app.gate.lock().await;
                    if !permitted(app, session, &run).await {
                        return Err(anyhow!("Naming authority unavailable"));
                    }
                    if cancel.is_cancelled()
                        || crate::engine_components::catalog()?.revision != engine_revision
                        || crate::engine_components::controls(&app.store.root)?.revision != engine_controls
                        || !crate::engine_components::component_available(
                            &app.store.root, crate::engine_components::PROJECTION)
                    {
                        return Err(anyhow!("Provider component or authority changed"));
                    }
                    // Poll the actual transport once while authority writers remain fenced.
                    let mut completion = Box::pin(async {
                        let ready = prepared.bind_credentials(tokens, &cancel)?;
                        app.provider.dispatch(ready, sender, cancel).await
                    });
                    let mut immediate = None;
                    futures_util::future::poll_fn(|cx| {
                        if let std::task::Poll::Ready(result) =
                            std::future::Future::poll(completion.as_mut(), cx)
                        {
                            immediate = Some(result);
                        }
                        std::task::Poll::Ready(())
                    }).await;
                    drop(dispatch_guard);
                    match immediate {
                        Some(result) => result,
                        None => completion.await,
                    }
                };
                tokio::pin!(operation);
                let deadline = tokio::time::sleep(Duration::from_secs(settings.request_timeout_seconds));
                tokio::pin!(deadline);
                let mut authority = tokio::time::interval(Duration::from_secs(1));
                let result = loop {
                    tokio::select! {
                        result=&mut operation=>break result,
                        _=cancel.cancelled()=>break Err(anyhow!("Naming stopped")),
                        _=&mut deadline=>break Err(anyhow!("Naming timed out")),
                        _=authority.tick()=>{
                            if !permitted(app,session,&run).await {
                                break Err(anyhow!("Naming authority unavailable"));
                            }
                        }
                    }
                };
                cancel.cancel();
                drain.abort();
                result
            }
            Err(error) => Err(error),
        };
        match response {
            Ok(response) => {
                let text = response["output"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|item| item["type"] == "message")
                    .flat_map(|item| item["content"].as_array().into_iter().flatten())
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("");
                let title = clean(&text);
                if !title.is_empty() && permitted(app, session, &run).await {
                    return Ok(title);
                }
            }
            Err(error) => {
                let mut failed = run.clone();
                // Same safe backup policy, independent of the chat run's cursor.
                if model_failure(&mut failed, &error).is_ok() {
                    let next: model_selection::FailoverState =
                        serde_json::from_value(failed["failover_state"].clone())?;
                    if next.candidate_index > state.candidate_index {
                        state = next;
                    }
                }
            }
        }
        if attempt + 1 < settings.attempts_per_job {
            tokio::select! {_=app.stop.cancelled()=>return Err(anyhow!("Naming stopped")),
            _=tokio::time::sleep(Duration::from_millis(settings.retry_delay_ms))=>{}}
        }
    }
    Err(anyhow!("Title generation unavailable"))
}
/// Own and renew one naming lease under session-epoch supervision; finish only its claim and preserve failed writes for recovery.
async fn work(app: App, sid: String, recovery: bool) -> Result<()> {
    let Some(session) = app.store.naming_claim(&sid, recovery).await? else {
        return Ok(());
    };
    let mut app=app;
    let requester=session["auto_naming"]["requester_user_id"].as_str().unwrap_or(app.store.current_user_id()).to_owned();
    app.chat_actor=Some(requester.clone());
    let access=crate::shared_chat::resolve(&app,&sid,&requester).await?;
    if !access.can_write() {return Err(anyhow!("Naming requester unavailable"));}
    app.chat_access=Some(access);
    let claim = session["auto_naming"]["claim"]
        .as_str()
        .ok_or_else(|| anyhow!("Missing naming claim"))?
        .to_owned();
    let operation = app.session_operation(&sid,app.stop.child_token(),generate(&app, &session));
    tokio::pin!(operation);
    let mut renew = tokio::time::interval(Duration::from_secs(10));
    let result = loop {
        tokio::select! {
            result=&mut operation=>break result,
            _=app.stop.cancelled()=>return Ok(()),
            _=renew.tick()=>{
                if !app.store.naming_renew(&sid,&claim).await? {return Ok(());}
            }
        }
    };
    // Failed writes leave the claim durable for lease recovery.
    app.store
        .naming_finish(&sid, &claim, result.as_deref().ok())
        .await?;
    Ok(())
}
/// Schedule bounded per-user naming workers and periodic lease recovery, then abort/join workers on process shutdown.
pub(crate) async fn service(app: App) {
    let mut workers = tokio::task::JoinSet::new();
    let mut active = std::collections::HashSet::new();
    let mut next_recovery = now() + 30;
    loop {
        if app.stop.is_cancelled() {
            break;
        }
        while let Some(result) = workers.try_join_next() {
            if let Ok(id) = result {
                active.remove(&id);
            }
        }
        let recovery = now() >= next_recovery;
        if recovery {
            next_recovery = now() + 60;
        }
        let mut recovered = 0;
        if let Ok(users) = agent_loop::user_apps(&app).await {
            for user in users {
                if let Ok(ids) = user.store.naming_candidates(recovery).await {
                    for sid in ids {
                        if workers.len() >= 2 || (recovery && recovered >= 10) {
                            break;
                        }
                        let id = format!("{}:{sid}", user.store.current_user_id());
                        if !active.insert(id.clone()) {
                            continue;
                        }
                        let cloned = user.clone();
                        workers.spawn(async move {
                            if let Err(_error) = work(cloned, sid, recovery).await {
                                tracing::warn!("Session naming deferred");
                            }
                            id
                        });
                        if recovery {
                            recovered += 1;
                        }
                    }
                }
            }
        }
        tokio::select! {_=app.stop.cancelled()=>break,_=tokio::time::sleep(Duration::from_millis(500))=>{}}
    }
    workers.abort_all();
    while workers.join_next().await.is_some() {}
}
#[cfg(test)]
mod tests {
    use super::*;
    /// Verify placeholder and generated-title bounds preserve complete Unicode characters and reject empty output.
    #[test]
    fn placeholders_and_cleaning_are_unicode_safe() {
        assert_eq!(fallback("  hello\n world  "), "New: hello world");
        assert_eq!(fallback(""), "New: Attachments");
        assert_eq!(
            fallback(&"🦀".repeat(70)),
            format!("New: {}", "🦀".repeat(60))
        );
        assert_eq!(
            clean("\n \"A Useful Title!\"\nIgnore this"),
            "A Useful Title"
        );
        assert!(clean(&"🦀".repeat(80)).len() <= 200);
        assert_eq!(clean(""), "");
    }
}

#[cfg(test)]
mod acceptance_tests {
    use super::*;
    /// Verify the configured naming deadline exhausts a job while preserving its fallback and durable chat rows.
    #[tokio::test]
    async fn configured_call_timeout_exhausts_without_chat_publication() {
        let (root, mut app) = fixture().await;
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let capture = seen.clone();
        let router = Router::new().route("/responses", post(move |Json(_): Json<Value>| {
            let capture = capture.clone();
            async move {
                capture.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::future::pending::<Json<Value>>().await
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        app.provider = provider::Provider::for_test(format!("http://{addr}/responses"));
        let mut settings = config::defaults().unwrap();
        settings.request_timeout_seconds = 1;
        settings.attempts_per_job = 1;
        settings.max_job_attempts = 1;
        write_settings(root.path(), &settings);
        let sid = create(&app, None).await;
        let mut run = accept(&app, &sid, "Timeout keeps fallback").await;
        finish(&app, &mut run).await;
        let before = app.store.chat_snapshot(&sid).await.unwrap();
        work(app.clone(), sid.clone(), false).await.unwrap();
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(saved(&app, &sid).await["auto_naming"]["stage"], "exhausted");
        assert_eq!(saved(&app, &sid).await["title"], "New: Timeout keeps fallback");
        assert_eq!(app.store.chat_snapshot(&sid).await.unwrap(), before);
        server.abort();
    }
    /// Write a private fixture's naming configuration while preserving the required base prompt field.
    fn write_settings(root: &std::path::Path, settings: &config::Config) {
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::write(root.join("config/prompts.json"), serde_json::to_vec(
            &json!({"session_naming":settings, "base_instructions":"keep"})).unwrap()).unwrap();
    }
    /// Verify queued settings stay pinned through capped retries while a later job observes new configuration.
    #[tokio::test]
    async fn settings_snapshot_retry_cap_and_next_job_reload() {
        let (root, app) = fixture().await;
        let mut settings = config::defaults().unwrap();
        settings.prompt = "First naming prompt".into();
        settings.max_job_attempts = 2;
        settings.recovery_retry_delay_seconds = 7;
        write_settings(root.path(), &settings);
        let sid = create(&app, None).await;
        let mut run = accept(&app, &sid, "Keep my fallback").await;
        finish(&app, &mut run).await;
        assert_eq!(saved(&app, &sid).await["auto_naming"]["config"]["prompt"], "First naming prompt");
        settings.prompt = "Second naming prompt".into();
        settings.max_job_attempts = 5;
        write_settings(root.path(), &settings);
        let first = app.store.naming_claim(&sid, false).await.unwrap().unwrap();
        assert_eq!(first["auto_naming"]["config"]["max_job_attempts"], 2);
        let before = now();
        app.store.naming_finish(&sid, first["auto_naming"]["claim"].as_str().unwrap(), None).await.unwrap();
        let mut failed = saved(&app, &sid).await;
        assert!(failed["auto_naming"]["retry_at"].as_u64().unwrap() >= before + 7);
        failed["auto_naming"]["retry_at"] = json!(0);
        app.store.put(&format!("session:{sid}"), &failed).await.unwrap();
        let second = app.store.naming_claim(&sid, true).await.unwrap().unwrap();
        app.store.naming_finish(&sid, second["auto_naming"]["claim"].as_str().unwrap(), None).await.unwrap();
        let exhausted = saved(&app, &sid).await;
        assert_eq!(exhausted["auto_naming"]["stage"], "exhausted");
        assert_eq!(exhausted["auto_naming"]["attempts"], 2);
        assert_eq!(exhausted["title"], "New: Keep my fallback");
        assert!(app.store.naming_candidates(true).await.unwrap().is_empty());
        assert!(app.store.naming_claim(&sid, true).await.unwrap().is_none());
        let next = create(&app, None).await;
        let mut run = accept(&app, &next, "New configuration").await;
        finish(&app, &mut run).await;
        assert_eq!(saved(&app, &next).await["auto_naming"]["config"]["prompt"], "Second naming prompt");
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(reopened.get(&format!("session:{sid}")).await.unwrap().unwrap()
            ["auto_naming"]["stage"], "exhausted");
    }
    /// Verify a configured naming role has its own durable plan and stale claims cannot replace it.
    #[tokio::test]
    async fn configured_role_plan_is_independent_and_claim_fenced() {
        let (root, app) = fixture().await;
        let mut settings = config::defaults().unwrap();
        settings.model_role = model_selection::ModelRole::Standard;
        write_settings(root.path(), &settings);
        let sid = create(&app, None).await;
        let mut run = accept(&app, &sid, "Use configured title role").await;
        let system = run["system_selection_plan"].clone();
        finish(&app, &mut run).await;
        let claimed = app.store.naming_claim(&sid, false).await.unwrap().unwrap();
        let pinned = prepare_plan(&app, &claimed).await.unwrap();
        assert_eq!(pinned["auto_naming"]["system_plan"]["role"], "standard");
        assert_eq!(saved(&app, &sid).await["auto_naming"]["system_plan"],
            pinned["auto_naming"]["system_plan"]);
        assert_eq!(run["system_selection_plan"], system);
        assert!(!app.store.naming_pin_plan(&sid, "stale", &pinned["auto_naming"]["system_plan"]).await.unwrap());
        assert_eq!(dispatch(&pinned, &model_selection::FailoverState::default()).unwrap()
            ["selection_plan"]["role"], "standard");
    }
    /// Verify legacy queued work gains a settings snapshot and an expired final lease cannot publish a late title.
    #[tokio::test]
    async fn expired_final_claim_is_exhausted_and_legacy_job_gets_snapshot() {
        let (_root, app) = fixture().await;
        let sid = create(&app, None).await;
        let mut run = accept(&app, &sid, "Recover bounded naming").await;
        finish(&app, &mut run).await;
        let mut queued = saved(&app, &sid).await;
        queued["auto_naming"].as_object_mut().unwrap().remove("config");
        queued["auto_naming"]["attempts"] = json!(2);
        app.store.put(&format!("session:{sid}"), &queued).await.unwrap();
        let claimed = app.store.naming_claim(&sid, false).await.unwrap().unwrap();
        assert_eq!(claimed["auto_naming"]["attempts"], 3);
        assert_eq!(claimed["auto_naming"]["config"]["max_job_attempts"], 3);
        let mut expired = claimed.clone();
        expired["auto_naming"]["lease_until"] = json!(0);
        app.store.put(&format!("session:{sid}"), &expired).await.unwrap();
        assert!(app.store.naming_claim(&sid, true).await.unwrap().is_none());
        assert_eq!(saved(&app, &sid).await["auto_naming"]["stage"], "exhausted");
        assert!(app.store.naming_candidates(true).await.unwrap().is_empty());
        assert!(!app.store.naming_finish(&sid, claimed["auto_naming"]["claim"].as_str().unwrap(), Some("Late title")).await.unwrap());
    }

    /// Verify a participation generation change withdraws naming authority permanently from the old claim.
    #[tokio::test]
    async fn deactivation_cancels_naming_claim_and_reactivation_cannot_publish_late_title() {
        let (_root,app)=fixture().await;
        let sid=create(&app,None).await;
        let mut run=accept(&app,&sid,"NAMING-SENTINEL").await;
        finish(&app,&mut run).await;
        let claimed=app.store.naming_claim(&sid,false).await.unwrap().unwrap();
        let naming_run=dispatch(&claimed,&model_selection::FailoverState::default()).unwrap();
        assert_eq!(naming_run["requester_user_id"],run["requester_user_id"]);
        assert_eq!(naming_run["participation_generation"],run["participation_generation"]);
        let claim=claimed["auto_naming"]["claim"].as_str().unwrap();
        app.store.set_participation(&sid,app.store.current_user_id(),false,0,"off").await.unwrap();
        app.store.set_participation(&sid,app.store.current_user_id(),true,1,"on").await.unwrap();
        assert!(!permitted(&app,&claimed,&naming_run).await);
        assert!(!app.store.naming_finish(&sid,claim,Some("Late private title")).await.unwrap());
        assert!(app.store.naming_claim(&sid,true).await.unwrap().is_none());
        work(app.clone(),sid.clone(),true).await.unwrap();
        let saved=saved(&app,&sid).await;
        assert_eq!(saved["auto_naming"]["stage"],"cancelled");
        assert!(saved["auto_naming"]["source"].is_null());
        assert_ne!(saved["title"],"Late private title");
    }

    use crate::runtime_tests::fixture;
    /// Create a private test session through the real session API with an optional initial title.
    async fn create(app: &App, title: Option<Value>) -> String {
        let mut body = json!({"client_request_id":uuid()});
        if let Some(title) = title {
            body["title"] = title;
        }
        crate::api::create_session(State(app.clone()), Json(body))
            .await
            .unwrap()
            .0["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    /// Admit one test message through the real API and verify an idempotent retry does not change session metadata.
    async fn accept(app: &App, sid: &str, text: &str) -> Value {
        let body = json!({"text":text,"client_message_id":uuid()});
        let (_, Json(result)) =
            crate::api::send(State(app.clone()), Path(sid.into()), Json(body.clone()))
                .await
                .unwrap();
        let before = app.store.session_read_summary(sid).await.unwrap();
        let _ = crate::api::send(State(app.clone()), Path(sid.into()), Json(body))
            .await
            .unwrap();
        assert_eq!(before, app.store.session_read_summary(sid).await.unwrap());
        app.run(result["run_id"].as_str().unwrap()).await.unwrap()
    }
    /// Persist a terminal test run so naming eligibility observes the normal completed-run checkpoint.
    async fn finish(app: &App, run: &mut Value) {
        run["status"] = json!("completed");
        run["turns"] = json!(1);
        app.store
            .save_run(
                run,
                "model_output",
                &json!({"output":[],"terminal_text":"One final reply"}),
            )
            .await
            .unwrap();
    }
    /// Read the persisted session fixture to assert durable naming state rather than local copies.
    async fn saved(app: &App, sid: &str) -> Value {
        app.store
            .get(&format!("session:{sid}"))
            .await
            .unwrap()
            .unwrap()
    }

    /// Invalid optional naming settings cannot undo terminal publication or overwrite a locked title.
    #[tokio::test]
    async fn invalid_naming_configuration_preserves_terminal_commit_once_and_locked_title() {
        let (root, app) = fixture().await;
        let sid = create(&app, None).await;
        let mut run = accept(&app, &sid, "Keep the completed reply").await;
        let locked = create(&app, Some(json!("My locked title"))).await;
        let mut locked_run = accept(&app, &locked, "Keep this title too").await;
        let before_locked = saved(&app, &locked).await["auto_naming"].clone();
        let invalid = br#"{"base_instructions":"keep","session_naming":{"prompt":"private-invalid-marker","max_job_attempts":0}}"#;
        std::fs::write(root.path().join("config/prompts.json"), invalid).unwrap();

        for (id, terminal) in [(&sid, &mut run), (&locked, &mut locked_run)] {
            finish(&app, terminal).await;
            assert_eq!(app.run(terminal["id"].as_str().unwrap()).await.unwrap()["status"], "completed");
            let before = app.store.chat_snapshot(id).await.unwrap();
            let replies: Vec<_> = before.0.iter().filter(|row| row["role"] == "assistant").collect();
            assert_eq!(replies.len(), 1);
            assert_eq!(replies[0]["content"], "One final reply");
            assert_eq!(replies[0]["run_id"], terminal["id"]);
            let session = saved(&app, id).await;
            finish(&app, terminal).await;
            assert_eq!(app.store.chat_snapshot(id).await.unwrap(), before);
            assert_eq!(saved(&app, id).await, session);
            assert!(app.store.get(&format!("session_naming:{id}")).await.unwrap().is_none());
        }
        let skipped = saved(&app, &sid).await;
        assert_eq!(skipped["title"], "New: Keep the completed reply");
        assert_eq!(skipped["auto_naming"]["stage"], "skipped");
        assert_eq!(skipped["auto_naming"]["last_error"], "Title configuration unavailable");
        assert_eq!(skipped["auto_naming"]["claim"], Value::Null);
        assert_eq!(skipped["auto_naming"]["lease_until"], 0);
        assert_eq!(skipped["auto_naming"]["attempts"], 0);
        assert!(app.store.naming_claim(&sid, true).await.unwrap().is_none());
        assert_eq!(saved(&app, &locked).await["title"], "My locked title");
        assert_eq!(saved(&app, &locked).await["auto_naming"], before_locked);
        assert_eq!(std::fs::read(root.path().join("config/prompts.json")).unwrap(), invalid);
    }

    /// Legacy configuration loading observes participation/lease fences before retiring unavailable work.
    #[tokio::test]
    async fn legacy_naming_configuration_skip_preserves_participation_and_claim_fences() {
        let (root, app) = fixture().await;
        for recovery in [false, true] {
            // Start with a valid job, then model its legacy unsnapshotted state.
            write_settings(root.path(), &config::defaults().unwrap());
            let sid = create(&app, None).await;
            let mut run = accept(&app, &sid, "Legacy title fallback").await;
            finish(&app, &mut run).await;
            assert!(app.store.get(&format!("session_naming:{sid}")).await.unwrap().is_some());
            let mut session = saved(&app, &sid).await;
            session["auto_naming"]["config"] = Value::Null;
            session["auto_naming"]["claim"] = json!("legacy-claim");
            let generation = session["auto_naming"]["participation_generation"].as_u64().unwrap();
            session["participation"]["generation"] = json!(generation + 1);
            app.store.put(&format!("session:{sid}"), &session).await.unwrap();
            std::fs::write(root.path().join("config/prompts.json"), b"{private-invalid-marker").unwrap();
            assert!(app.store.naming_claim(&sid, recovery).await.unwrap().is_none());
            assert_eq!(saved(&app, &sid).await, session);
            assert!(app.store.get(&format!("session_naming:{sid}")).await.unwrap().is_some());
            session["participation"]["generation"] = json!(generation);
            if recovery {
                session["auto_naming"]["stage"] = json!("generating");
                session["auto_naming"]["lease_until"] = json!(now() + 300);
                app.store.put(&format!("session:{sid}"), &session).await.unwrap();
                assert!(app.store.naming_claim(&sid, true).await.unwrap().is_none());
                assert_eq!(saved(&app, &sid).await, session);
                assert!(app.store.get(&format!("session_naming:{sid}")).await.unwrap().is_some());
            }
            session["auto_naming"]["lease_until"] = json!(0);
            app.store.put(&format!("session:{sid}"), &session).await.unwrap();
            assert!(app.store.naming_claim(&sid, recovery).await.unwrap().is_none());
            let skipped = saved(&app, &sid).await;
            assert_eq!(skipped["auto_naming"]["stage"], "skipped");
            assert_eq!(skipped["auto_naming"]["last_error"], "Title configuration unavailable");
            assert_eq!(skipped["auto_naming"]["claim"], Value::Null);
            assert_eq!(skipped["auto_naming"]["lease_until"], 0);
            assert_eq!(skipped["auto_naming"]["attempts"], session["auto_naming"]["attempts"]);
            assert_eq!(skipped["title"], session["title"]);
            assert_eq!(skipped["auto_naming"]["source"], session["auto_naming"]["source"]);
            assert_eq!(skipped["metadata_revision"].as_u64().unwrap(),
                session["metadata_revision"].as_u64().unwrap() + 1);
            assert!(app.store.get(&format!("session_naming:{sid}")).await.unwrap().is_none());
            assert!(!app.store.naming_renew(&sid, "legacy-claim").await.unwrap());
            assert!(!app.store.naming_finish(&sid, "legacy-claim", Some("Stale title")).await.unwrap());
            assert!(app.store.naming_claim(&sid, true).await.unwrap().is_none());
            assert_eq!(saved(&app, &sid).await, skipped);
        }
    }

    /// Verify first-message fallback/title state is atomic and explicit custom names remain protected.
    #[tokio::test]
    async fn defaults_custom_names_and_atomic_first_message() {
        let (_root, app) = fixture().await;
        for title in [None, Some(Value::Null), Some(json!(" \n "))] {
            let sid = create(&app, title).await;
            assert_eq!(saved(&app, &sid).await["title"], "New chat");
            let mut run = accept(&app, &sid, "  My first\n message is the source  ").await;
            let session = saved(&app, &sid).await;
            assert_eq!(session["title"], "New: My first message is the source");
            assert_eq!(session["auto_naming"]["stage"], "waiting_turn");
            assert_eq!(
                session["auto_naming"]["source"],
                "My first\n message is the source"
            );
            assert_eq!(session["auto_naming"]["run_id"], run["id"]);
            assert_eq!(session["metadata_revision"], 1);
            finish(&app, &mut run).await;
        }
        let sid = create(&app, Some(json!("  My Name  "))).await;
        let mut run = accept(&app, &sid, "please replace this").await;
        finish(&app, &mut run).await;
        assert_eq!(saved(&app, &sid).await["title"], "My Name");
        assert!(app.store.naming_claim(&sid, true).await.unwrap().is_none());
        let invalid = crate::api::create_session(
            State(app.clone()),
            Json(json!({"client_request_id":uuid(),"title":2})),
        )
        .await;
        assert!(invalid.is_err());
    }
    /// Verify single-claim naming, manual-title withdrawal and missing-role fallback without durable chat or activity changes.
    #[tokio::test]
    async fn system_only_queue_claim_manual_fence_and_no_chat_side_effects() {
        let (_root, app) = fixture().await;
        let sid = create(&app, None).await;
        let mut run = accept(&app, &sid, "Plan a garden").await;
        finish(&app, &mut run).await;
        let before = app.store.chat_snapshot(&sid).await.unwrap();
        let activity = saved(&app, &sid).await["last_active_at"].clone();
        let (a, b) = tokio::join!(
            app.store.naming_claim(&sid, false),
            app.store.naming_claim(&sid, false)
        );
        assert_eq!(
            usize::from(a.as_ref().unwrap().is_some()) + usize::from(b.as_ref().unwrap().is_some()),
            1
        );
        let claimed = a.unwrap().or(b.unwrap()).unwrap();
        let claim = claimed["auto_naming"]["claim"].as_str().unwrap();
        let patch = store::session::SessionPatch {
            client_request_id: uuid(),
            expected_revision: claimed["metadata_revision"].as_u64().unwrap(),
            title: Some("My Garden".into()),
            pinned: None,
        };
        app.store.patch_session(&sid, &patch).await.unwrap();
        assert!(!app
            .store
            .naming_finish(&sid, claim, Some("Overwritten"))
            .await
            .unwrap());
        assert!(!app.store.naming_renew(&sid, claim).await.unwrap());
        assert_eq!(saved(&app, &sid).await["title"], "My Garden");
        assert_eq!(saved(&app, &sid).await["last_active_at"], activity);
        assert_eq!(app.store.chat_snapshot(&sid).await.unwrap(), before);
        let mut catalog = app.store.model_catalog().await.unwrap();
        catalog
            .agent_models
            .get_mut("shared_default")
            .unwrap()
            .roles
            .remove(&model_selection::ModelRole::System);
        app.store
            .save_model_catalog(&catalog, catalog.revision)
            .await
            .unwrap();
        let sid = create(&app, None).await;
        let mut run = accept(&app, &sid, "No system").await;
        finish(&app, &mut run).await;
        assert_eq!(saved(&app, &sid).await["auto_naming"]["stage"], "skipped");
        assert_eq!(saved(&app, &sid).await["title"], "New: No system");
    }
    /// Verify stale leases cannot publish and recovered naming outcomes/cooldowns survive reopening storage.
    #[tokio::test]
    async fn lease_recovery_failure_cooldown_and_success_survive_reopen() {
        let (root, app) = fixture().await;
        let sid = create(&app, None).await;
        let mut run = accept(&app, &sid, "Recover my name").await;
        finish(&app, &mut run).await;
        let first = app.store.naming_claim(&sid, false).await.unwrap().unwrap();
        let old_claim = first["auto_naming"]["claim"].as_str().unwrap();
        let mut expired = saved(&app, &sid).await;
        expired["auto_naming"]["lease_until"] = json!(0);
        app.store
            .put(&format!("session:{sid}"), &expired)
            .await
            .unwrap();
        assert!(app.store.naming_claim(&sid, false).await.unwrap().is_none());
        let reclaimed = app.store.naming_claim(&sid, true).await.unwrap().unwrap();
        assert!(!app
            .store
            .naming_finish(&sid, old_claim, Some("Old result"))
            .await
            .unwrap());
        let claim = reclaimed["auto_naming"]["claim"].as_str().unwrap();
        assert!(app.store.naming_finish(&sid, claim, None).await.unwrap());
        assert!(app.store.naming_claim(&sid, true).await.unwrap().is_none());
        let mut due = saved(&app, &sid).await;
        due["auto_naming"]["retry_at"] = json!(0);
        app.store
            .put(&format!("session:{sid}"), &due)
            .await
            .unwrap();
        let claimed = app.store.naming_claim(&sid, true).await.unwrap().unwrap();
        assert!(app
            .store
            .naming_finish(
                &sid,
                claimed["auto_naming"]["claim"].as_str().unwrap(),
                Some("Recovered Name")
            )
            .await
            .unwrap());
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(
            reopened
                .get(&format!("session:{sid}"))
                .await
                .unwrap()
                .unwrap()["title"],
            "Recovered Name"
        );
        assert!(reopened.naming_claim(&sid, true).await.unwrap().is_none());
    }
    /// Capture naming requests to verify pinned prompts, first-message-only input and bounded blank-output retries without chat rows.
    #[tokio::test]
    async fn live_provider_uses_first_user_input_and_retries_blank_without_publishing() {
        let (root, mut app) = fixture().await;
        let mut settings = config::defaults().unwrap();
        settings.prompt = "Configured title instruction".into();
        settings.attempts_per_job = 2;
        settings.retry_delay_ms = 0;
        write_settings(root.path(), &settings);
        let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
        let capture = calls.clone();
        let server=Router::new().route("/responses",post(move |Json(body):Json<Value>| {
            let capture=capture.clone();
            async move {
                let mut calls=capture.lock().await;calls.push(body);
                let response=json!({"status":"completed","output":[{"type":"message","role":"assistant",
                    "content":[{"type":"output_text","text":if calls.len()==1 || calls.len()==3 {""}else{"\"Garden Planning!\""}}]}]});
                ([("content-type","text/event-stream")],format!("data: {}\n\n",
                    json!({"type":"response.completed","response":response})))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });
        app.provider = provider::Provider::for_test(format!("http://{addr}/responses"));
        let sid = create(&app, None).await;
        let mut run = accept(&app, &sid, "Plan a vegetable garden").await;
        finish(&app, &mut run).await;
        let before = app.store.chat_snapshot(&sid).await.unwrap();
        assert_eq!(
            app.store.naming_candidates(false).await.unwrap(),
            vec![sid.clone()]
        );
        work(app.clone(), sid.clone(), false).await.unwrap();
        assert!(app.store.naming_candidates(true).await.unwrap().is_empty());
        assert_eq!(saved(&app, &sid).await["title"], "Garden Planning");
        assert_eq!(app.store.chat_snapshot(&sid).await.unwrap(), before);
        let captured = calls.clone();
        let calls = calls.lock().await;
        assert_eq!(calls.len(), 2);
        assert!(calls[0]["input"]
            .to_string()
            .contains("Plan a vegetable garden"));
        assert!(!calls[0]["input"].to_string().contains("One final reply"));
        assert!(calls[0]["tools"].as_array().is_none_or(Vec::is_empty));
        assert!(calls.iter().all(|call| call["instructions"] == "Configured title instruction"));
        drop(calls);
        settings.prompt = "One attempt".into();
        settings.attempts_per_job = 1;
        settings.max_job_attempts = 1;
        write_settings(root.path(), &settings);
        let next = create(&app, None).await;
        let mut run = accept(&app, &next, "Preserve this fallback").await;
        finish(&app, &mut run).await;
        work(app.clone(), next.clone(), false).await.unwrap();
        assert_eq!(saved(&app, &next).await["auto_naming"]["stage"], "exhausted");
        assert_eq!(saved(&app, &next).await["title"], "New: Preserve this fallback");
        let captured = captured.lock().await;
        assert_eq!(captured.len(), 3);
        assert_eq!(captured[2]["instructions"], "One attempt");
        server.abort();
    }
}

#[cfg(test)]
mod attachment_tests {
    use super::*;
    /// Verify empty messages require attachments and naming uses attachment-derived context with the attachment placeholder.
    #[tokio::test]
    async fn empty_text_requires_attachments_and_uses_attachment_source() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::api::create_session(
            State(app.clone()),
            Json(json!({"client_request_id":uuid()})),
        )
        .await
        .unwrap()
        .0["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let bad = crate::api::send(
            State(app.clone()),
            Path(sid.clone()),
            Json(json!({"client_message_id":uuid(),"text":""})),
        )
        .await;
        assert!(bad.is_err());
        assert_eq!(
            app.store.session_read_summary(&sid).await.unwrap()["title"],
            "New chat"
        );
        let _ = crate::api::send(State(app.clone()),Path(sid.clone()),Json(json!({"client_message_id":uuid(),"text":"",
            "attachments":[{"name":"garden.txt","media_type":"text/plain","text":"Vegetable garden layout"}]}))).await.unwrap();
        let session = app
            .store
            .get(&format!("session:{sid}"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session["title"], "New: Attachments");
        assert!(session["auto_naming"]["source"]
            .as_str()
            .unwrap()
            .contains("Vegetable garden layout"));
    }
}

#[cfg(test)]
mod route_tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    /// Verify authenticated browser and messaging API routes produce the same durable naming admission state.
    #[tokio::test]
    async fn browser_and_machine_routes_share_the_same_persisted_flow() {
        let (_root, mut app) = crate::runtime_tests::fixture().await;
        let machine_token = "fixture-machine-token-long-enough-1234";
        app.message_api_token_hash = crate::api::message_api_token_hash(machine_token).unwrap();
        let token = uuid();
        app.store
            .put(
                &format!("auth:{}", digest(&json!(token))),
                &json!({"csrf":"fixture","expires":now()+3600,"generation":"fixture-generation"}),
            )
            .await
            .unwrap();
        for (base, machine) in [("/api/v3", false), ("/api/v3/admin/messaging", true)] {
            let request = |path: String, body: Value| {
                let mut builder = Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("content-type", "application/json");
                if machine {
                    builder = builder.header("authorization", format!("Bearer {machine_token}"));
                } else {
                    builder = builder
                        .header("cookie", format!("v3_session={token}"))
                        .header("x-csrf-token", "fixture");
                }
                builder.body(Body::from(body.to_string())).unwrap()
            };
            let response = crate::api::router(app.clone())
                .oneshot(request(
                    format!("{base}/sessions"),
                    json!({"client_request_id":uuid()}),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let created: Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(created["title"], "New chat");
            let sid = created["id"].as_str().unwrap();
            let response = crate::api::router(app.clone())
                .oneshot(request(
                    format!("{base}/sessions/{sid}/messages"),
                    json!({"client_message_id":uuid(),"text":"A shared naming flow"}),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::ACCEPTED);
            assert_eq!(
                app.store.session_read_summary(sid).await.unwrap()["title"],
                "New: A shared naming flow"
            );
        }
    }
    /// Verify pin-only metadata edits retain automatic naming while an explicit title save withdraws it.
    #[tokio::test]
    async fn pin_edits_keep_naming_but_saving_the_same_title_locks_it() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let Json(created) = crate::api::create_session(
            State(app.clone()),
            Json(json!({"client_request_id":uuid()})),
        )
        .await
        .unwrap();
        let sid = created["id"].as_str().unwrap();
        let patch = store::session::SessionPatch {
            client_request_id: uuid(),
            expected_revision: 0,
            title: None,
            pinned: Some(true),
        };
        app.store.patch_session(sid, &patch).await.unwrap();
        assert_eq!(
            app.store
                .get(&format!("session:{sid}"))
                .await
                .unwrap()
                .unwrap()["auto_naming"]["stage"],
            "awaiting_message"
        );
        let patch = store::session::SessionPatch {
            client_request_id: uuid(),
            expected_revision: 1,
            title: Some("New chat".into()),
            pinned: None,
        };
        app.store.patch_session(sid, &patch).await.unwrap();
        let session = app
            .store
            .get(&format!("session:{sid}"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session["auto_naming"]["stage"], "locked");
        assert_eq!(session["metadata_revision"], 2);
        assert_eq!(session["title"], "New chat");
    }
}

#[cfg(test)]
mod metadata_event_tests {
    use super::*;
    use axum::body::Body;
    use futures_util::StreamExt;
    use tower::ServiceExt;
    /// Verify title metadata events stream without consuming or changing the durable chat cursor.
    #[tokio::test]
    async fn metadata_events_update_names_without_setting_a_chat_cursor() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let Json(created) = crate::api::create_session(
            State(app.clone()),
            Json(json!({"client_request_id":uuid()})),
        )
        .await
        .unwrap();
        let sid = created["id"].as_str().unwrap();
        let token = uuid();
        app.store
            .put(
                &format!("auth:{}", digest(&json!(token))),
                &json!({"csrf":"fixture","expires":now()+3600,"generation":"fixture-generation"}),
            )
            .await
            .unwrap();
        let request = Request::builder()
            .uri(format!("/api/v3/sessions/{sid}/events"))
            .header("cookie", format!("v3_session={token}"))
            .body(Body::empty())
            .unwrap();
        let response = crate::api::router(app.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut stream = response.into_body().into_data_stream();
        let initial = tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let initial = std::str::from_utf8(&initial).unwrap();
        assert!(initial.contains("event: session_updated"));
        assert!(initial.contains("New chat"));
        assert!(!initial.lines().any(|line| line.starts_with("id:")));
        let patch = store::session::SessionPatch {
            client_request_id: uuid(),
            expected_revision: 0,
            title: Some("Saved Name".into()),
            pinned: None,
        };
        app.store.patch_session(sid, &patch).await.unwrap();
        let update = tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let update = std::str::from_utf8(&update).unwrap();
        assert!(update.contains("Saved Name"));
        assert!(!update.lines().any(|line| line.starts_with("id:")));
        assert_eq!(app.store.chat_snapshot(sid).await.unwrap().1, 0);
    }
}
