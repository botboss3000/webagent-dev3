//! Engine-owned dispatcher claims, leases, startup recovery and fail-stop supervision.
use super::{config, models::ModelRecords, ports::*, watchdog, Runtime};
use crate::context::abilities::Catalog;
use crate::context::memory;
use crate::ports::*;
use crate::{now, status, uuid};
use anyhow::Result;
use serde_json::{json, Value};
use std::{sync::atomic::Ordering, time::Duration};
impl<
        S: ScopedStorage
            + ModelRecords
            + ToolJournal
            + SessionRecords
            + ChatEffects
            + AdmissionEffects
            + NotificationEffects
            + UsageEffects
            + RunObservation
            + RunPresentation,
        C: config::SchemaCatalog,
        B: Catalog,
        A: super::actions::Actions<S> + Clone + PartialEq,
        H: ChatAuthority<S>,
        M: ModuleBindings<S>,
        X: AgentPolicy<S>
            + NativePolicy
            + CredentialBindings<S>
            + SkillSources<S>
            + PromptSlots<S>
            + MainGuidance<S>
            + ModelDiagnostics<S>
            + InstallationAccess<S>
            + ContextDiagnostics<S>
            + ProviderUsage
            + StartupPolicy<S>
            + FollowupQueue<S>
            + PublicationObservation<S, B>
            + super::native::NativeDispatch<S, C, B, H::Binding>,
    > Runtime<S, C, B, A, H, M, X>
{
    //! Claim queued runs and renew their leases while the loop is alive.

    // A frozen Auto release can inherit runs paused by the previous approval policy.
    // Only an undispatched tool is requeued; the claimed run checks live Deny again.
    pub async fn requeue_auto_approval(app: &Self, id: &str) -> Result<()> {
        if app.active.lock().await.contains_key(id) {
            return Ok(());
        }
        let mut run = app.run(id).await?;
        let sid = run["session_id"].as_str().unwrap_or("").to_owned();
        if status(&run) != "waiting_approval" {
            return Ok(());
        }
        let mut restricted = Vec::new();
        fn requires_specific_approval(pending: &Value) -> bool {
            !pending["authorization"].is_null() || pending["new_intent_confirmation"] == true
        }
        if requires_specific_approval(&run["pending"]) {
            restricted.push(run["pending"].clone());
        }
        if let Some(batch) = run["batch"].as_array() {
            restricted.extend(
                batch
                    .iter()
                    .map(|item| item["pending"].clone())
                    .filter(requires_specific_approval),
            );
        }
        if !restricted.is_empty() {
            let requester = run["requester_user_id"]
                .as_str()
                .unwrap_or(app.store.current_user_id());
            let agent = run["agent_id"].as_str().unwrap_or("shared_default");
            let access = app
                .chat
                .live_access(app.store.records(), requester, &sid, agent)
                .await;
            let mut actor_app = app.clone();
            actor_app.chat_actor = Some(requester.to_owned());
            let controls = actor_app.ability_controls(&sid).await;
            let mut invalid = !app
                .chat
                .participation_allows_run(app.store.records(), &run)
                .await?
                || !app.run_model_authorized(&run).await
                || access.as_ref().map_or(true, |access| !access.admitted());
            for pending in restricted {
                let revisions_valid = match (&controls, &access) {
                    (Ok((policy, mode, visibility, state)), Ok(access)) => {
                        pending["policy_version"] == policy["version"]
                            && pending["mode_revision"].as_u64().unwrap_or(0) == *mode
                            && pending["visibility_revision"] == visibility["revision"]
                            && pending["ability_state_revision"] == state["revision"]
                            && pending["access_revision"] == json!(access.revision)
                    }
                    _ => false,
                };
                if !revisions_valid
                    || !app
                        .native
                        .valid(app.store.records(), &run, &pending)
                        .await?
                {
                    invalid = true;
                    break;
                }
            }
            if invalid {
                fn cancel(pending: &mut Value) {
                    if matches!(pending["state"].as_str(), Some("waiting" | "requested")) {
                        pending["state"] = json!("cancelled");
                        pending["decision"] = Value::Null;
                        pending["human_approved"] = json!(false);
                    }
                }
                cancel(&mut run["pending"]);
                if let Some(batch) = run["batch"].as_array_mut() {
                    for item in batch {
                        cancel(&mut item["pending"]);
                    }
                }
                run["status"] = json!("stopped");
                run["error"] =
                    json!("Specific action authorization changed; request a fresh action");
                app.save(&mut run).await?;
            }
            // Credential ownership and an additional matching intent always require
            // their specific human approval; Auto must not rewrite its stable ID.
            return Ok(());
        }
        if status(&run) != "waiting_approval"
            || run["pending"]["state"] != "waiting"
            || !app
                .session_mode(&sid)
                .await
                .is_ok_and(|mode| mode["mode"] == "write" && mode["execution_mode"] == "auto")
        {
            return Ok(());
        }
        run["pending"]["id"] = json!(uuid());
        run["pending"]["state"] = json!("requested");
        run["pending"]["decision"] = Value::Null;
        run["status"] = json!("queued");
        app.save(&mut run).await
    }
    // Retire compacted reasoning outside the run loop. Each user-store job commits
    // atomically and remains queued after a failed attempt or process restart.
    async fn reasoning_cleanup_worker(app: Self) {
        loop {
            if app.stop.is_cancelled() {
                break;
            }
            match app.user_views(false).await {
                Ok(users) => {
                    for user in users {
                        if app.stop.is_cancelled() {
                            break;
                        }
                        if let Err(error) = user.store.cleanup_one_reasoning().await {
                            tracing::warn!("Reasoning cleanup deferred: {error}");
                        }
                    }
                }
                Err(error) => tracing::warn!("Reasoning cleanup user scan deferred: {error}"),
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
    // This is the loop container's dispatcher: announce that it is alive, notice
    // queued work, claim a lease, and start at most the configured number of runs.
    #[cfg(test)]
    pub async fn scheduler(app: Self) {
        Self::scheduler_with_progress(app, watchdog::Progress::new()).await;
    }

    pub fn scheduler_with_progress(
        app: Self,
        progress: watchdog::Progress,
    ) -> impl std::future::Future<Output = ()> + Send {
        async move {
            use watchdog::Stage;
            let wake_listener =
                crate::send_future(crate::wake::listener(&app.control_dir, app.stop.clone()))
                    .await
                    .ok();
            let naming_app = app.clone();
            let naming = tokio::spawn(async move {
                crate::send_future(naming_app.native.naming_service(naming_app.native_view())).await
            });
            let cleanup = tokio::spawn(Self::reasoning_cleanup_worker(app.clone()));
            let manager_app = app.clone();
            let manager = tokio::spawn(async move {
                crate::send_future(
                    manager_app
                        .native
                        .manager_service(manager_app.native_view()),
                )
                .await
            });
            let modules_app = app.clone();
            let modules = tokio::spawn(async move {
                crate::send_future(
                    modules_app
                        .native
                        .modules_service(modules_app.native_view()),
                )
                .await
            });
            let background = tokio::spawn(app.clone().background_service());
            let owner = uuid();
            let heartbeat_app = app.clone();
            let heartbeat_owner = owner.clone();
            let heartbeat = tokio::spawn(async move {
                loop {
                    if heartbeat_app.stop.is_cancelled() {
                        break;
                    }
                    let _ = crate::send_future(
                        heartbeat_app.store.write_loop_heartbeat(&heartbeat_owner),
                    )
                    .await;
                    tokio::select! {_=heartbeat_app.stop.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(2))=>{}}
                }
            });
            let mut last_heartbeat = 0;
            let mut last_recovery = 0;
            loop {
                if app.stop.is_cancelled() || !app.ready.load(Ordering::Acquire) {
                    break;
                }
                // Adopt scheduler and cache limits without replacing active runs.
                match config::Config::<C>::read(&app.runtime_path) {
                    Ok(config) => {
                        let mut previous = crate::send_future(app.runtime.write()).await;
                        if previous.cache.document_budget_bytes
                            != config.cache.document_budget_bytes
                            || previous.cache.context_budget_bytes
                                != config.cache.context_budget_bytes
                        {
                            app.store.configure_cache(
                                config.cache.document_budget_bytes,
                                config.cache.context_budget_bytes,
                            );
                        }
                        let background_changed = previous.scheduler.background_rescan_interval_ms
                            != config.scheduler.background_rescan_interval_ms;
                        *previous = config;
                        drop(previous);
                        if background_changed {
                            crate::send_future(crate::wake::notify(&app.control_dir)).await;
                        }
                    }
                    Err(_) => {
                        tracing::error!("Live runtime configuration rejected");
                        crate::send_future(tokio::time::sleep(Duration::from_secs(1))).await;
                        continue;
                    }
                }
                app.native
                    .record_source_adoption(app.store.records(), &app.abilities);
                let _ = crate::components::adoption(
                    crate::ports::AgentMetadata::root(app.store.records()),
                    "loop",
                );
                progress.at(Stage::Heartbeat);
                if now().saturating_sub(last_heartbeat) >= 2 {
                    if let Err(error) =
                        crate::send_future(app.store.write_loop_heartbeat(&owner)).await
                    {
                        tracing::error!("Loop heartbeat failed: {error}");
                    }
                    last_heartbeat = now();
                }
                if now() - last_recovery
                    >= crate::send_future(app.runtime.read())
                        .await
                        .scheduler
                        .recovery_interval_ms
                        .div_ceil(1000)
                {
                    progress.at(Stage::Recovery);
                    if crate::send_future(Self::recover(&app, false))
                        .await
                        .is_err()
                    {
                        tracing::error!("Recovery scan failed");
                    }
                    last_recovery = now();
                }
                progress.at(Stage::DispatchGate);
                let _guard = crate::send_future(app.handles.gate.lock()).await;
                if let Ok(false) = crate::send_future(app.shutoff()).await {
                    progress.at(Stage::UserScan);
                    let users = match crate::send_future(app.user_views(false)).await {
                        Ok(users) => users,
                        Err(error) => {
                            tracing::error!("User queue scan failed: {}", error);
                            Vec::new()
                        }
                    };
                    for user_app in users {
                        progress.at(Stage::Commands);
                        if let Err(error) =
                            crate::send_future(user_app.store.acknowledge_loop_commands()).await
                        {
                            tracing::error!("Loop command validation failed: {error}");
                            continue;
                        }
                        if let Err(error) = crate::send_future(
                            user_app.native.followup_tick(user_app.native_view()),
                        )
                        .await
                        {
                            tracing::warn!("Followup queue reconciliation deferred: {error}");
                        }
                        if let Err(error) =
                            crate::send_future(user_app.store.drain_message_queues()).await
                        {
                            tracing::warn!("Message queue drain deferred: {error}");
                        }
                        progress.at(Stage::RunScan);
                        if let Ok(runs) = crate::send_future(user_app.store.active_runs()).await {
                            for view in runs {
                                let id = view["id"].as_str().unwrap().to_owned();
                                if !crate::send_future(app.active.lock())
                                    .await
                                    .contains_key(&id)
                                    && crate::send_future(user_app.interrupt_requested(&id))
                                        .await
                                        .unwrap_or(false)
                                {
                                    let Ok(mut interrupted) =
                                        crate::send_future(user_app.run(&id)).await
                                    else {
                                        continue;
                                    };
                                    if let Err(error) =
                                        crate::send_future(Self::settle_followup_interrupt(
                                            &user_app,
                                            &mut interrupted,
                                        ))
                                        .await
                                    {
                                        tracing::warn!(run_id=%id,"Followup interruption deferred: {error}");
                                    }
                                    continue;
                                }
                                if status(&view) == "paused" {
                                    if let Err(_) =
                                        crate::send_future(user_app.component_restore(&id)).await
                                    {
                                        tracing::warn!("Engine component restoration deferred");
                                    }
                                    continue;
                                }
                                if status(&view) == "waiting_approval" {
                                    if let Err(error) = crate::send_future(
                                        Self::requeue_auto_approval(&user_app, &id),
                                    )
                                    .await
                                    {
                                        tracing::warn!(run_id=%id,"Auto approval reconciliation deferred: {error}");
                                    }
                                    continue;
                                }
                                if status(&view) != "queued" {
                                    continue;
                                }
                                if !crate::components::available(crate::ports::AgentMetadata::root(
                                    user_app.store.records(),
                                )) {
                                    if let Ok(mut run) = crate::send_future(user_app.run(&id)).await
                                    {
                                        let _ =
                                            crate::send_future(user_app.component_pause(&mut run))
                                                .await;
                                    }
                                    continue;
                                }
                                progress.at(Stage::Claim);
                                let mut run = match crate::send_future(user_app.run(&id)).await {
                                    Ok(r) => r,
                                    Err(_) => continue,
                                };
                                let mut active = crate::send_future(app.active.lock()).await;
                                if active.contains_key(&id) {
                                    continue;
                                }
                                let ceiling = crate::send_future(app.runtime.read())
                                    .await
                                    .scheduler
                                    .max_active_runs;
                                if ceiling != 0 && active.len() >= ceiling {
                                    break;
                                }
                                let permit = match app.slots.clone().try_acquire_owned() {
                                    Ok(p) => p,
                                    Err(_) => break,
                                };
                                let cancel = app.stop.child_token();
                                active.insert(id.clone(), cancel.clone());
                                drop(active);
                                if run["context_hydration_pending"] == true {
                                    let requester = run["requester_user_id"]
                                        .as_str()
                                        .unwrap_or(user_app.store.current_user_id());
                                    let context = if requester == user_app.store.current_user_id() {
                                        crate::send_future(
                                            user_app
                                                .store
                                                .context(run["session_id"].as_str().unwrap()),
                                        )
                                        .await
                                    } else {
                                        crate::send_future(user_app.store.shared_model_context(
                                            run["session_id"].as_str().unwrap(),
                                        ))
                                        .await
                                    };
                                    match context {
                                        Ok(mut input) => {
                                            // Keep saved notes beside the latest user turn as source data.
                                            if requester == user_app.store.current_user_id() {
                                                memory::insert(
                                                    &mut input,
                                                    run["memory_context"].as_str(),
                                                );
                                            }
                                            super::run::pair_unresolved_calls(&mut input);
                                            run["input"] = json!(input);
                                            run["context_hydration_pending"] = json!(false);
                                        }
                                        Err(error) => {
                                            tracing::warn!(run_id=%id,"Context hydration failed: {}",error);
                                            crate::send_future(app.active.lock()).await.remove(&id);
                                            continue;
                                        }
                                    }
                                }
                                let generation = uuid();
                                if !matches!(
                                    crate::send_future(user_app.store.claim_run(
                                        &mut run,
                                        &owner,
                                        &generation
                                    ))
                                    .await,
                                    Ok(true)
                                ) {
                                    crate::send_future(app.active.lock()).await.remove(&id);
                                    continue;
                                }
                                let mut cloned = user_app.clone();
                                cloned.chat_actor =
                                    run["requester_user_id"].as_str().map(str::to_owned);
                                let requester = cloned.actor().to_owned();
                                match crate::send_future(cloned.chat.resolve(
                                    cloned.store.records(),
                                    run["session_id"].as_str().unwrap_or(""),
                                    &requester,
                                ))
                                .await
                                {
                                    Ok(access) => cloned.chat_access = Some(access),
                                    Err(_) => {
                                        crate::send_future(app.active.lock()).await.remove(&id);
                                        continue;
                                    }
                                }
                                let lease_owner = owner.clone();
                                tokio::spawn(async move {
                                    let epoch = run["lease_epoch"].as_i64().unwrap_or(0);
                                    let watcher_store = cloned.store.clone();
                                    let watcher_id = id.clone();
                                    let watcher_generation = generation.clone();
                                    let watcher_cancel = cancel.clone();
                                    let interrupt_app = cloned.clone();
                                    let interrupt_id = id.clone();
                                    let interrupt_cancel = cancel.clone();
                                    let interrupt_watcher = tokio::spawn(async move {
                                        loop {
                                            tokio::select! {
                                                _=interrupt_cancel.cancelled()=>break,
                                                _=tokio::time::sleep(Duration::from_millis(250))=>{},
                                            }
                                            match crate::send_future(
                                                interrupt_app.interrupt_requested(&interrupt_id),
                                            )
                                            .await
                                            {
                                                Ok(true) => {
                                                    interrupt_cancel.cancel();
                                                    break;
                                                }
                                                Ok(false) => {}
                                                Err(error) => {
                                                    tracing::warn!(run_id=%interrupt_id,"Interrupt observation deferred: {error}")
                                                }
                                            }
                                        }
                                    });
                                    let watcher = tokio::spawn(async move {
                                        loop {
                                            crate::send_future(tokio::time::sleep(
                                                Duration::from_secs(5),
                                            ))
                                            .await;
                                            if watcher_cancel.is_cancelled() {
                                                break;
                                            }
                                            let renewed =
                                                crate::send_future(watcher_store.renew_run_lease(
                                                    &watcher_id,
                                                    &watcher_generation,
                                                    &lease_owner,
                                                    epoch,
                                                ))
                                                .await
                                                .unwrap_or(false);
                                            if !renewed {
                                                watcher_cancel.cancel();
                                                break;
                                            }
                                        }
                                    });
                                    if let Err(error) =
                                        crate::send_future(cloned.session_operation(
                                            run["session_id"].as_str().unwrap_or(""),
                                            cancel.clone(),
                                            Self::execute_run(
                                                cloned.clone(),
                                                id.clone(),
                                                generation,
                                                cancel.clone(),
                                            ),
                                        ))
                                        .await
                                    {
                                        tracing::warn!(run_id=%id,"Run interrupted: {}",error);
                                    }
                                    // Join paths that return early (approval and parallel tools)
                                    // also settle durable interruption after their awaits finish.
                                    {
                                        let _settle_guard =
                                            crate::send_future(cloned.handles.gate.lock()).await;
                                        if let Ok(mut final_run) =
                                            crate::send_future(cloned.run(&id)).await
                                        {
                                            if let Err(error) =
                                                crate::send_future(Self::settle_followup_interrupt(
                                                    &cloned,
                                                    &mut final_run,
                                                ))
                                                .await
                                            {
                                                tracing::warn!(run_id=%id,"Followup handoff deferred: {error}");
                                            }
                                        }
                                    }
                                    cancel.cancel();
                                    interrupt_watcher.abort();
                                    watcher.abort();
                                    crate::send_future(cloned.active.lock()).await.remove(&id);
                                    drop(permit);
                                    crate::send_future(crate::wake::notify(&cloned.control_dir))
                                        .await;
                                });
                            }
                        }
                    }
                }
                drop(_guard);
                progress.at(Stage::Idle);
                let fallback = if std::env::var("V3_DISPATCH_POLL_ONLY").as_deref() == Ok("1") {
                    crate::send_future(app.runtime.read())
                        .await
                        .scheduler
                        .poll_interval_ms
                } else {
                    5000
                };
                crate::send_future(crate::wake::wait(fallback, &app.stop)).await;
            }
            if let Some(listener) = wake_listener {
                listener.abort();
            }
            naming.abort();
            cleanup.abort();
            manager.abort();
            background.abort();
            modules.abort();
            heartbeat.abort();
        }
    }
}
