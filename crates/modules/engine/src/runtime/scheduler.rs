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
    /// Returns when the wait expires (`u64::MAX`: never) for a run that was waiting, so the
    /// pass can store the account's earliest expiry; `None` for a run that was not waiting.
    pub async fn requeue_auto_approval(app: &Self, id: &str) -> Result<Option<u64>> {
        if app.active.lock().await.contains_key(id) {
            return Ok(None);
        }
        let mut run = app.run(id).await?;
        if status(&run) != "waiting_approval" {
            return Ok(None);
        }
        let expiry = config::Config::<C>::read(&app.runtime_path)?
            .tools
            .approval_expiry_seconds;
        let expires_at = if expiry > 0 && run["pending"]["decision"].is_null() {
            run["pending"]["waiting_since"]
                .as_u64()
                .map_or(u64::MAX, |since| since.saturating_add(expiry))
        } else {
            u64::MAX
        };
        // A run this pass requeues or stops was written, so the account is evaluated again
        // and the next pass recomputes its expiry; reporting the old one only visits early.
        Self::requeue_waiting(app, run, expiry)
            .await
            .map(|()| Some(expires_at))
    }
    async fn requeue_waiting(app: &Self, mut run: Value, expiry: u64) -> Result<()> {
        let sid = run["session_id"].as_str().unwrap_or("").to_owned();
        if run["parked"] != true && app.steer_waiting(&run).await? {
            return app.park_approval(run).await;
        }
        if run["parked"] == true && app.parked_call_cancelled(&run).await? {
            run["pending"]["decision"] = json!("deny");
            run["status"] = json!("queued");
            return app.save(&mut run).await;
        }
        // An approval left undecided past the configured expiry counts as declined; the
        // run continues and the agent is told not to retry.
        if expiry > 0
            && run["pending"]["decision"].is_null()
            && run["pending"]["waiting_since"]
                .as_u64()
                .is_some_and(|since| crate::now() >= since.saturating_add(expiry))
        {
            run["pending"]["decision"] = json!("deny");
            run["pending"]["expired"] = json!(true);
            run["status"] = json!("queued");
            let public = app.store.records().present_run(&run);
            return app
                .store
                .save_run_observed(&mut run, "run_status", &public)
                .await;
        }
        let mut restricted = Vec::new();
        fn requires_specific_approval(pending: &Value) -> bool {
            !pending["authorization"].is_null()
                || crate::tools::retry_guard::needs_specific_approval(pending)
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
                    (Ok((mode, visibility, state)), Ok(access)) => {
                        pending["mode_revision"].as_u64().unwrap_or(0) == *mode
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
                .is_ok_and(|mode| mode["execution_mode"] == "auto")
        {
            return Ok(());
        }
        run["pending"]["id"] = json!(uuid());
        run["pending"]["state"] = json!("requested");
        run["pending"]["decision"] = Value::Null;
        run["status"] = json!("queued");
        app.save(&mut run).await
    }
    /// Offer paused runs to `component_restore`: every account's when `every` (the start-up
    /// sweep), else only the accounts this process paused a run in since the last sweep
    /// (C9: a component outage under load opened all accounts under the dispatch gate). An
    /// account whose read failed or that still holds a paused run (one running in this loop
    /// just now) is noted again for the next sweep. True when every account was read; a
    /// failure leaves the sweep pending for the next pass.
    async fn restore_sweep(app: &Self, every: bool) -> bool {
        let users = if every {
            let Ok(users) = crate::send_future(app.user_ids(true)).await else {
                return false;
            };
            users
        } else {
            super::components::take_paused(&app.control_dir)
        };
        let mut complete = true;
        for user in users {
            let restored = crate::send_future(async {
                let view = app.for_user_view(&user).await.ok()?;
                let runs = view.store.scheduler_runs().await.ok()?;
                let mut done = true;
                let mut paused = false;
                for (id, state) in runs {
                    if state != "paused" {
                        continue;
                    }
                    paused = true;
                    if view.component_restore(&id).await.is_err() {
                        tracing::warn!("Engine component restoration deferred");
                        done = false;
                    }
                }
                let left = paused
                    && view
                        .store
                        .scheduler_runs()
                        .await
                        .ok()?
                        .iter()
                        .any(|(_, state)| state == "paused");
                Some((done, left))
            })
            .await;
            match restored {
                Some((true, false)) => {}
                Some((done, _)) => {
                    complete &= done;
                    super::components::note_paused(&app.control_dir, &user);
                }
                None => {
                    complete = false;
                    super::components::note_paused(&app.control_dir, &user);
                }
            }
        }
        complete
    }
    // Retire compacted reasoning outside the run loop. Each user-store job commits
    // atomically and remains queued after a failed attempt or process restart.
    async fn reasoning_cleanup_worker(app: Self) {
        loop {
            if app.stop.is_cancelled() {
                break;
            }
            match app.user_ids(false).await {
                Ok(users) => {
                    for user in users {
                        if app.stop.is_cancelled() {
                            break;
                        }
                        // One account at a time (see `user_ids`).
                        let Ok(user) = app.for_user_view(&user).await else {
                            continue;
                        };
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
            let keep_alive = tokio::spawn(app.clone().keep_alive_service());
            let owner = uuid();
            let heartbeat_app = app.clone();
            let heartbeat_owner = owner.clone();
            let heartbeat = tokio::spawn(async move {
                loop {
                    if heartbeat_app.stop.is_cancelled() {
                        break;
                    }
                    if let Err(error) = crate::send_future(
                        heartbeat_app.store.write_loop_heartbeat(&heartbeat_owner),
                    )
                    .await
                    {
                        tracing::error!("Loop heartbeat failed: {error}");
                    }
                    tokio::select! {_=heartbeat_app.stop.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(2))=>{}}
                }
            });
            let mut last_recovery = 0;
            // Component-paused runs are not work the scan returns: every account is swept once
            // at start; after a component pause in this loop, and when components come back or
            // shutoff ends after the loop saw them away, the accounts this loop paused runs in
            // are swept (C9).
            let mut restore_sweep = true;
            // C9: only the first sweep of this process reads every account.
            let mut full_sweep = true;
            let mut seen_pauses = super::components::COMPONENT_PAUSES.load(Ordering::Acquire);
            // The requester whose run the loop started last; the next pass starts after it.
            let mut last_served: Option<String> = None;
            // C7: each account's own work as the last full pass left it (see `OwnWork`).
            let mut own_work: std::collections::HashMap<String, OwnWork<Self>> =
                std::collections::HashMap::new();
            loop {
                if app.stop.is_cancelled() {
                    break;
                }
                // A drain (SIGTERM cleared `ready`) dispatches and recovers nothing, but this
                // loop stays alive so heartbeats and running runs' lease watchers keep going.
                if !app.ready.load(Ordering::Acquire) {
                    progress.at(Stage::Idle);
                    crate::send_future(crate::wake::wait(1000, &app.stop)).await;
                    continue;
                }
                // Adopt scheduler and cache limits without replacing active runs.
                match config::Config::<C>::read(&app.runtime_path) {
                    Ok(config) => {
                        let mut previous = crate::send_future(app.runtime.write()).await;
                        if previous.cache.context_budget_bytes != config.cache.context_budget_bytes
                        {
                            app.store.configure_cache(config.cache.context_budget_bytes);
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
                // The heartbeat task above is the one writer of the loop heartbeat (every 2 s);
                // a stuck pass shows in the watchdog's stage progress instead.
                progress.at(Stage::Heartbeat);
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
                let shutoff = crate::send_future(app.shutoff()).await;
                let pauses = super::components::COMPONENT_PAUSES.load(Ordering::Acquire);
                if pauses != seen_pauses {
                    seen_pauses = pauses;
                    restore_sweep = true;
                }
                if !matches!(shutoff, Ok(false))
                    || !crate::components::available(crate::ports::AgentMetadata::root(
                        app.store.records(),
                    ))
                {
                    restore_sweep = true;
                } else if restore_sweep {
                    progress.at(Stage::UserScan);
                    let complete = crate::send_future(Self::restore_sweep(&app, full_sweep)).await;
                    full_sweep &= !complete;
                    restore_sweep = !complete;
                }
                // The account scan and the passes over idle accounts no longer hold the loop's
                // process-wide lock; each account's commands, follow-ups, approvals and claims
                // run under it, so no account is handled half-locked.
                drop(_guard);
                if let Ok(false) = shutoff {
                    progress.at(Stage::UserScan);
                    let users = match crate::send_future(app.user_ids(false)).await {
                        Ok(users) => users,
                        Err(error) => {
                            tracing::error!("User queue scan failed: {}", error);
                            Vec::new()
                        }
                    };
                    // Queued runs of every account, claimed after all accounts' own work. Each
                    // account is opened in turn and let go (only queued runs keep a view).
                    let mut candidates = Vec::new();
                    // C7: an account unchanged since its last full pass less than `OWN_WORK`
                    // ago offers its remembered queued runs without being opened; a run its
                    // last claim refused waits for a run to end (a slot release) instead of
                    // being tried on every pass. Component trouble or shutoff forgets all.
                    let releases = RELEASES.load(Ordering::Acquire);
                    if restore_sweep {
                        own_work.clear();
                    }
                    own_work.retain(|user, _| users.contains(user));
                    for user in users {
                        let mark = app.store.contents_mark(&user);
                        if let Some(known) = own_work.get(&user).filter(|known| known.fresh(mark)) {
                            for queued in &known.queued {
                                if known.refused.get(&queued.id) != Some(&releases) {
                                    candidates.push((
                                        queued.requester.clone(),
                                        (
                                            queued.view.clone(),
                                            queued.id.clone(),
                                            queued.run.clone(),
                                        ),
                                    ));
                                }
                            }
                            continue;
                        }
                        own_work.remove(&user);
                        let user_app = match crate::send_future(app.for_user_view(&user)).await {
                            Ok(view) => view,
                            Err(error) => {
                                tracing::warn!(%error, "Account skipped: its storage could not be opened");
                                continue;
                            }
                        };
                        // Taken after opening and before reading: a write meanwhile moves it.
                        let mark = app.store.contents_mark(&user);
                        let mut queued_here = Vec::new();
                        let mut complete = true;
                        progress.at(Stage::DispatchGate);
                        let _account_gate = crate::send_future(app.handles.gate.lock()).await;
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
                            complete = false;
                            tracing::warn!("Followup queue reconciliation deferred: {error}");
                        }
                        if let Err(error) =
                            crate::send_future(user_app.store.drain_message_queues()).await
                        {
                            complete = false;
                            tracing::warn!("Message queue drain deferred: {error}");
                        }
                        progress.at(Stage::RunScan);
                        // The plain run index (no window, no decrypt) names the runs to act on.
                        let runs = crate::send_future(user_app.store.scheduler_runs()).await;
                        complete &= runs.is_ok();
                        if let Ok(runs) = runs {
                            // The account's earliest approval expiry, stored after the pass
                            // so an account with only approval waits rests until then.
                            let mut expires: Option<u64> = None;
                            let mut expiry_known = true;
                            for (id, state) in runs {
                                if state == "paused" {
                                    if let Err(_) =
                                        crate::send_future(user_app.component_restore(&id)).await
                                    {
                                        complete = false;
                                        tracing::warn!("Engine component restoration deferred");
                                    }
                                    continue;
                                }
                                if state == "waiting_approval" {
                                    match crate::send_future(Self::requeue_auto_approval(
                                        &user_app, &id,
                                    ))
                                    .await
                                    {
                                        Ok(Some(at)) => {
                                            expires = Some(expires.map_or(at, |e| e.min(at)));
                                        }
                                        Ok(None) => {}
                                        Err(error) => {
                                            expiry_known = false;
                                            tracing::warn!(run_id=%id,"Auto approval reconciliation deferred: {error}");
                                        }
                                    }
                                    continue;
                                }
                                if state != "queued" {
                                    continue;
                                }
                                if !crate::components::available(crate::ports::AgentMetadata::root(
                                    user_app.store.records(),
                                )) {
                                    complete = false;
                                    if let Ok(mut run) = crate::send_future(user_app.run(&id)).await
                                    {
                                        let _ =
                                            crate::send_future(user_app.component_pause(&mut run))
                                                .await;
                                    }
                                    continue;
                                }
                                let mut run = match crate::send_future(user_app.run(&id)).await {
                                    Ok(r) => r,
                                    Err(_) => {
                                        complete = false;
                                        continue;
                                    }
                                };
                                // A full account starts no new run; runs already going finish.
                                if run["calls"].as_array().is_none_or(Vec::is_empty)
                                    && run["call_index"].as_u64().unwrap_or(0) == 0
                                    && crate::send_future(user_app.store.storage_full())
                                        .await
                                        .unwrap_or(false)
                                {
                                    run["status"] = json!("failed");
                                    run["error"] = json!(STORAGE_FULL);
                                    if crate::send_future(user_app.save(&mut run)).await.is_err() {
                                        tracing::warn!(run_id=%id, "Storage-full refusal deferred");
                                    }
                                    continue;
                                }
                                let requester = run["requester_user_id"]
                                    .as_str()
                                    .unwrap_or(user_app.store.current_user_id())
                                    .to_owned();
                                queued_here.push(Queued {
                                    requester: requester.clone(),
                                    view: user_app.clone(),
                                    id: id.clone(),
                                    run: run.clone(),
                                });
                                candidates.push((requester, (user_app.clone(), id, run)));
                            }
                            // A failed reconciliation leaves the expiry due now: the account
                            // stays visited and the next complete pass stores the real one.
                            let expires = if expiry_known { expires } else { Some(0) };
                            if let Err(error) = crate::send_future(APPROVAL_EXPIRIES.set(
                                ScopedStorage::application_pool(user_app.store.records()),
                                crate::ports::Records::current_user_id(user_app.store.records()),
                                crate::due::APPROVAL,
                                expires,
                            ))
                            .await
                            {
                                complete = false;
                                tracing::warn!("Approval expiry not stored: {error}");
                            }
                        }
                        // Only a pass that finished every step is remembered; a deferred step
                        // is tried again on the next pass.
                        if complete && mark.is_some() {
                            own_work.insert(
                                user,
                                OwnWork {
                                    mark,
                                    at: std::time::Instant::now(),
                                    queued: queued_here,
                                    refused: std::collections::HashMap::new(),
                                },
                            );
                        }
                    }
                    // C5 fair admission: queued runs start in turn over requesters, starting
                    // after the one served last, not in account order. Admission decides each
                    // claim in its writer transaction (requester limit, per-agent cap, slots
                    // kept for administrators); the loop's ceiling still stops the pass.
                    let mut waiting = std::collections::HashSet::new();
                    // Paid-tier priority (plus tier spec 8.3): runs whose slot goes first now
                    // (paid owners' agents, Free runs past their longest wait) are served
                    // before the others, each group in turn over requesters.
                    let first = match candidates.first() {
                        Some((_, (user_app, _, _))) => {
                            let ids: Vec<&str> = candidates
                                .iter()
                                .map(|(_, (_, id, _))| id.as_str())
                                .collect();
                            crate::send_future(first_in_line(
                                ScopedStorage::application_pool(user_app.store.records()),
                                &ids,
                            ))
                            .await
                        }
                        None => std::collections::HashSet::new(),
                    };
                    let (ahead, behind): (Vec<_>, Vec<_>) = candidates
                        .into_iter()
                        .partition(|(_, (_, id, _))| first.contains(id));
                    let mut ordered = fair_order(ahead, last_served.as_deref());
                    ordered.extend(fair_order(behind, last_served.as_deref()));
                    for (requester_key, (user_app, id, mut run)) in ordered {
                        if waiting.contains(&requester_key) {
                            continue;
                        }
                        progress.at(Stage::DispatchGate);
                        let _claim_gate = crate::send_future(app.handles.gate.lock()).await;
                        progress.at(Stage::Claim);
                        let last_served = &mut last_served;
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
                        let capacity = Capacity {
                            active: active.len(),
                            ceiling,
                        };
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
                                    user_app.store.context(run["session_id"].as_str().unwrap()),
                                )
                                .await
                            } else {
                                crate::send_future(
                                    user_app
                                        .store
                                        .shared_model_context(run["session_id"].as_str().unwrap()),
                                )
                                .await
                            };
                            match context {
                                Ok(mut input) => {
                                    // Keep saved notes beside the latest user turn as source data.
                                    if requester == user_app.store.current_user_id() {
                                        memory::insert(&mut input, run["memory_context"].as_str());
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
                            crate::send_future(user_app.store.claim_run_within(
                                &mut run,
                                &owner,
                                &generation,
                                capacity,
                            ))
                            .await,
                            Ok(true)
                        ) {
                            crate::send_future(app.active.lock()).await.remove(&id);
                            // The requester's later runs wait behind this one (their places in
                            // line are later), so they are not tried again in this pass.
                            waiting.insert(requester_key);
                            // Nor in later passes, until a run ends or its account changes.
                            if let Some(known) = own_work.get_mut(
                                crate::ports::Records::current_user_id(user_app.store.records()),
                            ) {
                                known.refused.insert(id.clone(), releases);
                            }
                            continue;
                        }
                        *last_served = Some(requester_key.clone());
                        let mut cloned = user_app.clone();
                        cloned.chat_actor = run["requester_user_id"].as_str().map(str::to_owned);
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
                            let watcher = tokio::spawn(async move {
                                loop {
                                    crate::send_future(tokio::time::sleep(Duration::from_secs(5)))
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
                            if let Err(error) = crate::send_future(cloned.session_operation(
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
                                let _settle_guard =
                                    crate::send_future(cloned.handles.gate.lock()).await;
                                if let Ok(mut failed) = crate::send_future(cloned.run(&id)).await {
                                    if note_interrupt(&mut failed, &error.to_string()) {
                                        if let Err(error) =
                                            crate::send_future(cloned.save(&mut failed)).await
                                        {
                                            tracing::warn!(run_id=%id,"Interrupt note not saved: {error}");
                                        }
                                    }
                                }
                            }
                            cancel.cancel();
                            watcher.abort();
                            crate::send_future(cloned.active.lock()).await.remove(&id);
                            drop(permit);
                            // A run slot is free: runs waiting on a refused claim try again.
                            RELEASES.fetch_add(1, Ordering::AcqRel);
                            crate::send_future(crate::wake::notify_account(
                                &cloned.control_dir,
                                crate::ports::Records::current_user_id(cloned.store.records()),
                                crate::wake::Kind::Run,
                            ))
                            .await;
                        });
                    }
                }
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
            keep_alive.abort();
            modules.abort();
            heartbeat.abort();
        }
    }
}

/// C5 fair order: round-robin over requesters, each requester's runs in the order given
/// (admission order), with a rotating start: the first requester after `last` (the one the
/// loop served most recently, by id, wrapping), so no requester is always first or last.
pub fn fair_order<T>(items: Vec<(String, T)>, last: Option<&str>) -> Vec<(String, T)> {
    let mut lines: std::collections::BTreeMap<String, std::collections::VecDeque<T>> =
        std::collections::BTreeMap::new();
    for (requester, item) in items {
        lines.entry(requester).or_default().push_back(item);
    }
    let mut requesters: Vec<String> = lines.keys().cloned().collect();
    if let Some(last) = last {
        let start = requesters.partition_point(|id| id.as_str() <= last);
        let count = requesters.len().max(1);
        requesters.rotate_left(start % count);
    }
    let mut ordered = Vec::new();
    loop {
        let mut any = false;
        for requester in &requesters {
            if let Some(item) = lines.get_mut(requester).and_then(|line| line.pop_front()) {
                ordered.push((requester.clone(), item));
                any = true;
            }
        }
        if !any {
            return ordered;
        }
    }
}

/// Of the queued runs `ids`, those whose run slot goes first now: a paid owner's agent's,
/// or a Free run past its longest wait (`first_at` on admission's slot table). A failed read
/// puts every run in one group, as before priority existed.
pub async fn first_in_line(
    pool: &sqlx::SqlitePool,
    ids: &[&str],
) -> std::collections::HashSet<String> {
    if ids.is_empty() {
        return std::collections::HashSet::new();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        });
    let list = serde_json::to_string(ids).unwrap_or_else(|_| "[]".into());
    match sqlx::query_scalar::<_, String>("SELECT run_id FROM requester_run_slots WHERE released=0 AND first_at<=? AND run_id IN (SELECT value FROM json_each(?))")
        .bind(now)
        .bind(list)
        .fetch_all(pool)
        .await
    {
        Ok(first) => first.into_iter().collect(),
        Err(error) => {
            tracing::warn!("Run priority not read: {error}");
            std::collections::HashSet::new()
        }
    }
}

/// C7: how long an unchanged account's own work (commands, follow-ups, queue drain, approval
/// expiries, component restores and its queued runs' readiness) is taken as it was. The same
/// as the scheduler's slow fallback, so time-based steps (an approval expiry, a review past
/// its deadline) wait no longer than they did when nothing woke the loop.
const OWN_WORK: std::time::Duration = std::time::Duration::from_secs(5);

/// Runs this loop has ended (each gives back a run slot); a refused claim is tried again
/// once this moves.
static RELEASES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// One queued run an account offered: its requester, the account's view, id and record.
struct Queued<V> {
    requester: String,
    view: V,
    id: String,
    run: Value,
}

/// An account's own work as a full scheduler pass left it.
struct OwnWork<V> {
    /// The account's `contents_mark` taken before that pass read it.
    mark: Option<u64>,
    at: std::time::Instant,
    queued: Vec<Queued<V>>,
    /// Runs whose claim was refused, with [`RELEASES`] at that pass.
    refused: std::collections::HashMap<String, u64>,
}

impl<V> OwnWork<V> {
    /// Still as that pass left it: nothing written since (same known mark) and recent.
    fn fresh(&self, mark: Option<u64>) -> bool {
        self.mark.is_some() && self.mark == mark && self.at.elapsed() < OWN_WORK
    }
}

/// Approval expiries this loop stored, so an unchanged one is not written on every pass.
static APPROVAL_EXPIRIES: std::sync::LazyLock<crate::due::Written> =
    std::sync::LazyLock::new(Default::default);

/// Failed executions of one turn before the run pauses instead of being recovered again.
const MAX_INTERRUPTS: u64 = 3;
/// Shown on a new run the account's full storage kept from starting.
const STORAGE_FULL: &str = "Storage is full, so this run did not start. Free space from the storage meter on the Sessions page, then send again.";

/// Record a failed execution on an active run so the chat shows it. A run that fails the same
/// turn `MAX_INTERRUPTS` times pauses with the error; a new turn starts the count again.
fn note_interrupt(run: &mut Value, error: &str) -> bool {
    if !matches!(status(run), "running" | "queued") {
        return false;
    }
    let count = if run["interrupts"]["turn"] == run["turns"] {
        run["interrupts"]["count"].as_u64().unwrap_or(0) + 1
    } else {
        1
    };
    run["interrupts"] = json!({"turn":run["turns"],"count":count});
    if count >= MAX_INTERRUPTS {
        run["status"] = json!("paused");
        run["error"] = json!(format!("Paused after {count} failed attempts: {error}"));
    } else {
        run["error"] = json!(format!(
            "Retrying ({count} of {MAX_INTERRUPTS} attempts failed): {error}"
        ));
    }
    true
}

#[cfg(test)]
mod interrupt_tests {
    use super::note_interrupt;
    use serde_json::json;

    #[test]
    fn requesters_are_served_in_turn_from_a_rotating_start() {
        let queued = |pairs: &[(&str, u32)]| -> Vec<(String, u32)> {
            pairs.iter().map(|(r, n)| ((*r).to_owned(), *n)).collect()
        };
        // Account order put "a" (three runs) before the guest "z"; turns alternate instead.
        let items = queued(&[("a", 1), ("a", 2), ("a", 3), ("b", 4), ("z", 5)]);
        let order: Vec<u32> = super::fair_order(items.clone(), None)
            .into_iter()
            .map(|(_, n)| n)
            .collect();
        assert_eq!(order, vec![1, 4, 5, 2, 3]);
        // After "a" was served, the next pass starts with "b"; after "z", it wraps to "a".
        let after_a: Vec<u32> = super::fair_order(items.clone(), Some("a"))
            .into_iter()
            .map(|(_, n)| n)
            .collect();
        assert_eq!(after_a, vec![4, 5, 1, 2, 3]);
        let after_z: Vec<String> = super::fair_order(items, Some("z"))
            .into_iter()
            .map(|(r, _)| r)
            .collect();
        assert_eq!(after_z[0], "a");
        assert!(super::fair_order::<u32>(Vec::new(), Some("a")).is_empty());
    }

    #[test]
    fn repeated_failures_on_one_turn_pause_the_run() {
        let mut run = json!({"status":"running","turns":4});
        assert!(note_interrupt(&mut run, "boom"));
        assert_eq!(run["error"], "Retrying (1 of 3 attempts failed): boom");
        assert!(note_interrupt(&mut run, "boom"));
        assert_eq!(run["status"], "running");
        assert!(note_interrupt(&mut run, "boom"));
        assert_eq!(run["status"], "paused");
        assert_eq!(run["error"], "Paused after 3 failed attempts: boom");
    }

    #[test]
    fn a_new_turn_restarts_the_count_and_settled_runs_are_left_alone() {
        let mut run = json!({"status":"running","turns":4,"interrupts":{"turn":4,"count":2}});
        run["turns"] = json!(5);
        note_interrupt(&mut run, "boom");
        assert_eq!(run["interrupts"]["count"], 1);
        assert_eq!(run["status"], "running");
        let mut cancelled = json!({"status":"cancelled","turns":5});
        assert!(!note_interrupt(&mut cancelled, "boom"));
        assert!(cancelled.get("error").is_none());
    }
}
