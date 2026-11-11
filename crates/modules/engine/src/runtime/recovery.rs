use super::build_tasks;
use super::config;
use super::models::ModelRecords;
use super::ports::{
    AgentPolicy, ChatAuthority, ContextDiagnostics, CredentialBindings, FollowupQueue,
    InstallationAccess, MainGuidance, ModelDiagnostics, ModuleBindings, NativePolicy, PromptSlots,
    ProviderUsage, RunObservation, RunPresentation, ScopedStorage, SkillSources, StartupPolicy,
};
use super::Runtime;
use crate::context::abilities::Catalog;
use crate::ports::{AdmissionEffects, ChatEffects, NotificationEffects, ToolJournal, UsageEffects};
use crate::tools;
use crate::{now, status};
use anyhow::bail;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
impl<
        S: ScopedStorage
            + ModelRecords
            + ToolJournal
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
            + super::native::NativeDispatch<S, C, B, H::Binding>,
    > Runtime<S, C, B, A, H, M, X>
{
    // Reopen unfinished runs after interruption; hold uncertain writes for review.

    pub fn reconcile_late_tool_result(run: &mut Value, result: Result<Value>) -> Result<bool> {
        if run["uncertain"] != true {
            return Ok(false);
        }
        let value = match result {
            Ok(value)
                if !matches!(
                    value["status"].as_str(),
                    Some("cancelled" | "timeout" | "uncertain")
                ) =>
            {
                value
            }
            _ => return Ok(false),
        };
        Self::append_tool(run, &value)?;
        run["uncertain"] = json!(false);
        if status(run) == "recovery" {
            run["status"] = json!(if run["error"] == "Cancelled by user" {
                "cancelled"
            } else {
                "stopped"
            });
        }
        Ok(true)
    }

    fn pending_build_finish(run: &Value) -> bool {
        matches!(
            run["pending"]["name"].as_str(),
            Some("v3_build_work" | "build_work")
        ) && run["pending"]["arguments"]["action"] == "finish"
            && run["pending"]["state"] == "executing"
    }
    /// A pending broker call whose job the run is waiting on: a finish, a queued check, or
    /// a `wait` that follows a job an earlier call returned early from.
    fn pending_build_job(run: &Value) -> bool {
        matches!(
            run["pending"]["name"].as_str(),
            Some("v3_build_work" | "build_work")
        ) && matches!(
            run["pending"]["arguments"]["action"].as_str(),
            Some("finish" | "check" | "wait")
        ) && run["pending"]["state"] == "executing"
    }
    pub fn attach_external_build_result(run: &mut Value, result: &Value) -> Result<()> {
        if status(run) != "waiting_external"
            || !Self::pending_build_job(run)
            || run["pending"]["external_job"]["kind"] != "v3_build"
        {
            bail!("No pending external build result");
        }
        Self::append_tool(run, result)?;
        run["status"] = json!("queued");
        run["error"] = Value::Null;
        Ok(())
    }
    /// Record one poll of a build-waiting run's job for the Broker chip and act on it. An
    /// alert (slow, stalled, broker unreadable) posts one chat line and returns the waiting
    /// call early, so the agent decides; a job that ends without completing posts a line,
    /// and its end becomes the call's result.
    async fn follow_build(app: &Self, run: &mut Value, poll: Result<BuildPoll>) -> Result<()> {
        let rid = run["id"].as_str().unwrap_or_default().to_owned();
        let sid = run["session_id"].as_str().unwrap_or_default().to_owned();
        let now = now();
        match poll {
            Ok(BuildPoll::Waiting(None)) => Ok(()),
            Ok(BuildPoll::Waiting(Some(progress))) => {
                let Some(job_id) = progress["job_id"].as_str().map(str::to_owned) else {
                    return Ok(());
                };
                let seen = run.clone();
                let (alert, resumed, task, unnamed) =
                    build_tasks::update(app.store.records(), &sid, |record| {
                        let job = job_entry(record, &seen, &job_id, now);
                        let resumed = build_tasks::broker_answered(job);
                        build_tasks::snapshot(job, &progress, now);
                        let alert = build_tasks::alert(job, &progress);
                        let task = build_tasks::task_of(record, &job_id);
                        let unnamed = task
                            .as_deref()
                            .is_some_and(|task| !build_tasks::named(record, task));
                        build_tasks::trim(record);
                        (alert, resumed, task, unnamed)
                    })
                    .await?;
                if let Some(task) = task.as_deref().filter(|_| unnamed) {
                    let summary = tokio::time::timeout(
                        Duration::from_secs(3),
                        Self::task_summary(app, run, task),
                    )
                    .await;
                    if let Ok(Ok(Some(summary))) = summary {
                        build_tasks::update(app.store.records(), &sid, |record| {
                            build_tasks::name_task(record, task, &summary);
                        })
                        .await?;
                    }
                }
                if let Some(text) = resumed {
                    Self::build_line(app, run, &job_id, "resumed", &text).await?;
                }
                if let Some((reason, text)) = alert {
                    Self::build_line(app, run, &job_id, reason.as_str(), &text).await?;
                    let result =
                        build_tasks::early_result(&reason, &text, &job_id, task.as_deref());
                    Self::return_build_result(app, run, &result).await?;
                }
                Ok(())
            }
            Ok(BuildPoll::Done(result)) => {
                let output = &result["output"];
                if let Some(job_id) = output["job_id"].as_str() {
                    let seen = run.clone();
                    build_tasks::update(app.store.records(), &sid, |record| {
                        let job = job_entry(record, &seen, job_id, now);
                        build_tasks::broker_answered(job);
                        if output["stages"].is_object() {
                            build_tasks::snapshot(job, &output["stages"], now);
                        }
                        job["status"] = output["status"].clone();
                        build_tasks::trim(record);
                    })
                    .await?;
                    if let Some(text) = build_tasks::ended_line(output) {
                        Self::build_line(app, run, job_id, "ended", &text).await?;
                    }
                }
                Self::return_build_result(app, run, &result).await
            }
            Err(error) => {
                tracing::warn!(run_id=%rid,"Build receipt reconciliation deferred: {error}");
                let Some(job_id) = run["pending"]["external_job"]["job_id"]
                    .as_str()
                    .map(str::to_owned)
                else {
                    return Ok(());
                };
                let seen = run.clone();
                let (alert, task) = build_tasks::update(app.store.records(), &sid, |record| {
                    let alert = build_tasks::broker_failed(job_entry(record, &seen, &job_id, now));
                    (alert, build_tasks::task_of(record, &job_id))
                })
                .await?;
                if let Some((reason, text)) = alert {
                    Self::build_line(app, run, &job_id, reason.as_str(), &text).await?;
                    let result =
                        build_tasks::early_result(&reason, &text, &job_id, task.as_deref());
                    Self::return_build_result(app, run, &result).await?;
                }
                Ok(())
            }
        }
    }
    /// A user message arrived during a build wait: give the build call a stand-in result
    /// and continue the run; the job is followed as a detached task and its result
    /// delivered later. False when no task could be recorded (the wait continues).
    async fn detach_build(app: &Self, run: &mut Value) -> Result<bool> {
        let pending = run["pending"].clone();
        let args = pending["arguments"].clone();
        let Some(task_id) = app
            .background_detach_record(run, &args, pending.clone())
            .await?
        else {
            return Ok(false);
        };
        let name = pending["name"]
            .as_str()
            .unwrap_or("v3_build_work")
            .to_owned();
        let call_id = pending["call_id"].as_str().unwrap_or("").to_owned();
        tools::retry_guard::mark_running(run, &name, &args, &call_id, &task_id, &A::retry(&name));
        let job = pending["external_job"]["job_id"]
            .as_str()
            .unwrap_or("unknown");
        let result = json!({"status":"running","task_id":task_id,"output":format!(
            "Build job {job} is still running as background task {task_id}: the user sent a \
             message, so you can answer it now. You get a background notice when the job \
             ends or a stage runs slow or stalls. Do not submit this build again.")});
        Self::append_tool(run, &result)?;
        run["status"] = json!("queued");
        run["error"] = Value::Null;
        let rid = run["id"].as_str().unwrap_or_default().to_owned();
        app.store
            .save_run_override(
                run,
                "tool_result",
                &json!({"run_id":rid,"name":name,"result":result}),
            )
            .await?;
        Ok(true)
    }
    /// Follow background build tasks: keep the Broker chip current, report a slow or stalled
    /// stage, and report the job's end as a background notice that wakes the agent. After a
    /// session stop the task is quiet: the chip still updates but no notice is sent.
    async fn follow_detached_builds(app: &Self) -> Result<()> {
        for mut task in app.store.background_scan(None).await? {
            if task["detached"] != true
                || task["status"] != "running"
                || !task["external_pending"].is_object()
            {
                continue;
            }
            let sid = task["session_id"].as_str().unwrap_or_default().to_owned();
            let follow = json!({"id":task["run_id"],"session_id":sid,
                "pending":task["external_pending"]});
            let poll =
                tokio::time::timeout(Duration::from_secs(3), Self::build_result(app, &follow))
                    .await;
            let now = now();
            match poll {
                Ok(Ok(BuildPoll::Waiting(Some(progress)))) => {
                    let Some(job_id) = progress["job_id"].as_str().map(str::to_owned) else {
                        continue;
                    };
                    let (alert, job_task) =
                        build_tasks::update(app.store.records(), &sid, |record| {
                            let job = job_entry(record, &follow, &job_id, now);
                            build_tasks::broker_answered(job);
                            build_tasks::snapshot(job, &progress, now);
                            let alert = build_tasks::alert(job, &progress);
                            let job_task = build_tasks::task_of(record, &job_id);
                            build_tasks::trim(record);
                            (alert, job_task)
                        })
                        .await?;
                    if let Some((reason, text)) = alert.filter(|_| task["quiet"] != true) {
                        let reason = reason.as_str();
                        app.stage_background_notice(
                            &sid,
                            &format!("build:{job_id}:{reason}:{now}"),
                            json!({"source":"build","title":job_title(&follow),"task":job_task,
                            "status":reason,"job_id":job_id,"run_id":task["run_id"],"reply":text}),
                        )
                        .await?;
                    }
                }
                Ok(Ok(BuildPoll::Done(result))) => {
                    let output = &result["output"];
                    let job_id = output["job_id"].as_str().unwrap_or("unknown").to_owned();
                    let job_task = build_tasks::update(app.store.records(), &sid, |record| {
                        let job = job_entry(record, &follow, &job_id, now);
                        build_tasks::broker_answered(job);
                        if output["stages"].is_object() {
                            build_tasks::snapshot(job, &output["stages"], now);
                        }
                        job["status"] = output["status"].clone();
                        let job_task = build_tasks::task_of(record, &job_id);
                        build_tasks::trim(record);
                        job_task
                    })
                    .await?;
                    let reply = build_tasks::ended_line(output)
                        .unwrap_or_else(|| ended_summary(output, &job_id));
                    let revision = task["revision"].as_u64().unwrap_or(0);
                    task["result"] = result.clone();
                    task["status"] = json!("completed");
                    task["completed_at"] = json!(now);
                    task["lease_until"] = json!(0);
                    task["delivery_run_id"] = json!("background_notice");
                    if !app.store.background_cas(&task, Some(revision)).await? {
                        continue;
                    }
                    app.store
                        .background_delivered(&task, "background_notice")
                        .await?;
                    if task["quiet"] == true {
                        continue;
                    }
                    app.stage_background_notice(&sid, &format!("build:{job_id}:ended"),
                        json!({"source":"build","title":job_title(&follow),"task":job_task,
                        "status":output["status"].as_str().unwrap_or("ended"),"job_id":job_id,
                        "run_id":task["run_id"],"task_id":task["id"],"reply":reply,"result":result}))
                        .await?;
                }
                _ => {}
            }
        }
        Ok(())
    }
    /// Post one build chat line, keyed by job, kind and time so each is posted once.
    async fn build_line(
        app: &Self,
        run: &mut Value,
        job_id: &str,
        kind: &str,
        text: &str,
    ) -> Result<()> {
        let rid = run["id"].as_str().unwrap_or_default().to_owned();
        let short = job_id.get(..8).unwrap_or(job_id);
        let message_id = format!("working:{rid}:build:{short}:{kind}:{}", now());
        app.store
            .save_run_override_observed(
                run,
                "working_message",
                &json!({"id":message_id,"text":text}),
            )
            .await
    }
    /// Give the waiting build call its result and queue the run so the agent continues.
    async fn return_build_result(app: &Self, run: &mut Value, result: &Value) -> Result<()> {
        let rid = run["id"].as_str().unwrap_or_default().to_owned();
        Self::attach_external_build_result(run, result)?;
        app.store
            .save_run_override(
                run,
                "tool_result",
                &json!({"run_id":rid,"name":"v3_build_work","result":result}),
            )
            .await
    }
    /// The trusted identity and socket a build-waiting run reads its broker job with.
    async fn broker_identity(app: &Self, run: &Value) -> Result<(Value, std::path::PathBuf)> {
        let sid = run["session_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing session"))?;
        let rid = run["id"].as_str().ok_or_else(|| anyhow!("Missing run"))?;
        let finish = run["pending"]["arguments"]["action"] == "finish";
        let authority = match (run["pending"]["name"] == "build_work", finish) {
            (true, false) => "build_work",
            (true, true) => "build_work_finish",
            (false, false) => "v3_build_work",
            (false, true) => "v3_build_work_finish",
        };
        let fingerprint = app
            .native
            .host_fingerprint(app.native_view(), sid, rid, authority)
            .await?;
        let trusted = json!({"user_id":app.store.current_user_id(),"session_id":sid,
        "lease_until":now()+30,"access_fingerprint":fingerprint});
        Ok((trusted, app.native.recovery_build_socket(app.native_view())))
    }
    /// The broker summary of one of the run's tasks, for the Broker chip's task name.
    async fn task_summary(app: &Self, run: &Value, task: &str) -> Result<Option<String>> {
        let sid = run["session_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing session"))?;
        let (trusted, socket) = Self::broker_identity(app, run).await?;
        let status = crate::tools::transport::execute_host_trusted(
            "v3_build_work",
            &json!({"action":"status","task_id":task}),
            sid,
            &socket,
            Some(trusted),
            CancellationToken::new(),
        )
        .await?;
        Ok(status["output"]["own"]
            .as_array()
            .and_then(|items| items.iter().find(|item| item["task_id"] == task))
            .and_then(|item| item["summary"].as_str().map(str::to_owned)))
    }
    async fn build_result(app: &Self, run: &Value) -> Result<BuildPoll> {
        let sid = run["session_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing session"))?;
        let task = finish_task(run);
        let (trusted, socket_path) = Self::broker_identity(app, run).await?;
        let socket = &socket_path;
        let mut job_id = run["pending"]["external_job"]["job_id"]
            .as_str()
            .map(str::to_owned);
        if job_id.is_none() {
            // The broker may have admitted finish just before the loop stopped.
            // Resolve its task receipt; never submit another finish.
            let task = task.ok_or_else(|| anyhow!("Missing build task"))?;
            let status = crate::tools::transport::execute_host_trusted(
                "v3_build_work",
                &json!({"action":"status","task_id":task}),
                sid,
                socket,
                Some(trusted.clone()),
                CancellationToken::new(),
            )
            .await?;
            if status["status"] != "success" {
                bail!("Build task receipt unavailable");
            }
            let item = status["output"]["own"]
                .as_array()
                .and_then(|items| items.iter().find(|item| item["task_id"] == task));
            let Some(item) = item else {
                return Ok(BuildPoll::Done(json!({"status":"error","output":{
                "task_id":task,"status":"not_submitted","reason":"Task receipt unavailable; no build was submitted"}})));
            };
            if item["inputs_sha256"] != run["pending"]["arguments"]["expected_inputs_sha256"]
                && item["status"] == "closed"
            {
                bail!("Build task digest changed");
            }
            job_id = item["job_id"].as_str().map(str::to_owned);
            if job_id.is_none() && item["status"] == "open" {
                return Ok(BuildPoll::Done(json!({"status":"error","output":{
                "task_id":task,"status":"not_submitted","reason":"Task remains open; no build was submitted"}})));
            }
            if job_id.is_none() {
                return Ok(BuildPoll::Waiting(None));
            }
        }
        let Some(job_id) = job_id else {
            return Ok(BuildPoll::Waiting(None));
        };
        let response = crate::tools::transport::execute_host_trusted(
            "v3_worker_job",
            &json!({"job_id":job_id}),
            sid,
            socket,
            Some(trusted.clone()),
            CancellationToken::new(),
        )
        .await?;
        if response["status"] != "success" {
            bail!("Build job receipt unavailable");
        }
        let job = &response["output"];
        let state = job["status"].as_str().unwrap_or("");
        // Stage timings only; a missing table never holds back the result.
        let progress = crate::tools::transport::execute_host_trusted(
            "v3_build_progress",
            &json!({"job_id":job_id}),
            sid,
            socket,
            Some(trusted.clone()),
            CancellationToken::new(),
        )
        .await
        .ok()
        .filter(|reply| reply["status"] == "success")
        .map(|reply| reply["output"].clone());
        let terminal = matches!(
            state,
            "completed"
                | "failed"
                | "stale"
                | "cancelled"
                | "uncertain"
                | "interrupted"
                | "needs_decision"
        );
        // The broker commits a completed release and frees its claims right after it
        // finishes; wait briefly so the result carries that commit.
        let closing = state == "completed"
            && job["operation"] != "check"
            && job["rebuild_closeout"].is_null()
            && job["finished_at"]
                .as_f64()
                .is_some_and(|at| (now() as f64) - at < CLOSEOUT_WAIT_SECONDS);
        if !terminal || closing {
            return Ok(BuildPoll::Waiting(progress));
        }
        let mut output = json!({"task_id":task,"job_id":job_id,"status":state,
        "phase":job["phase"],"reason":job["reason"],"log":job["log"],"synchronization":job["synchronization"],
        "stages":progress,"closeout":job["rebuild_closeout"]});
        if state == "completed" {
            let health = crate::tools::transport::execute_host_trusted(
                "v3_build_status",
                &json!({}),
                sid,
                socket,
                Some(trusted),
                CancellationToken::new(),
            )
            .await?;
            if health["status"] != "success" {
                return Ok(BuildPoll::Waiting(None));
            }
            let containers = &health["output"]["containers"];
            output["containers"] = containers.clone();
            output["verified_healthy"] = json!(["api", "loop"]
                .iter()
                .all(|role| containers[*role]["state"] == "running"
                    && containers[*role]["health"] == "healthy"));
        }
        Ok(BuildPoll::Done(json!({"status":"success","output":output})))
    }
    /// A planned restart's mark (`restart-cause.json` in the control directory): written by
    /// the loop on SIGTERM, or first by the broker with its deploy job and owning session.
    /// Startup recovery deletes it, so a later crash is never mistaken for a planned restart.
    fn restart_cause(app: &Self) -> Option<Value> {
        let path = app.handles.control_dir.join("restart-cause.json");
        serde_json::from_slice::<Value>(&std::fs::read(path).ok()?)
            .ok()
            .filter(Value::is_object)
    }
    /// Window during which a cancellation-caused context pause is still reclaimed.
    const CANCELLED_PAUSE_RECLAIM_SECS: i64 = 24 * 60 * 60;
    /// Runs paused only because their context preparation was cancelled by a
    /// restart (before cancellation was distinguished from context faults).
    /// Preparation precedes any model dispatch or tool effect, and a new chat
    /// message supersedes paused runs, so a surviving one is a safe resume point.
    pub fn cancelled_context_pause(run: &Value, now: i64) -> bool {
        status(run) == "paused"
            && run["component_pause"].is_null()
            && run["uncertain"] != true
            && run["pending"]["state"] != "executing"
            && !super::batch::executing_write::<A>(run)
            && run["error"].as_str().is_some_and(|error| {
                error.starts_with("Context preparation failed at ")
                    && error.ends_with(": Engine call cancelled")
            })
            && run["updated_at"]
                .as_i64()
                .is_some_and(|at| now - at <= Self::CANCELLED_PAUSE_RECLAIM_SECS)
    }
    // After a restart, put safe unfinished work back in the queue. An interrupted
    // external write is held for review because its effect may already have happened.
    pub async fn recover(app: &Self, startup: bool) -> Result<()> {
        let _guard = app.handles.gate.lock().await;
        // Only boot recovery obeys the optional resume control. Uncertain effects
        // are always classified, and ordinary lease recovery retains its behavior.
        let resume = if !startup {
            true
        } else {
            match app.native.startup_resume(app.store.records()).await {
                Ok(enabled) => enabled,
                Err(_) => {
                    tracing::error!("Startup resume configuration unavailable; safe interrupted runs remain stopped");
                    false
                }
            }
        };
        for user_app in app.user_views(startup).await? {
            for view in user_app.store.active_runs().await? {
                let Some(id) = view["id"].as_str() else {
                    continue;
                };
                if startup && status(&view) == "paused" {
                    let mut run = user_app.run(id).await?;
                    if !Self::cancelled_context_pause(&run, now() as i64) {
                        continue;
                    }
                    if !resume
                        || app.shutoff().await?
                        || !user_app.run_model_authorized(&run).await
                        || !user_app
                            .chat
                            .participation_allows_run(user_app.store.records(), &run)
                            .await?
                    {
                        continue;
                    }
                    run["status"] = json!("queued");
                    run["error"] = json!("Recovered after interruption");
                    user_app.save_override(&mut run).await?;
                    continue;
                }
                if !matches!(status(&view), "running" | "waiting_external")
                    || (!startup && app.active.lock().await.contains_key(id))
                {
                    continue;
                }
                let mut run = user_app.run(id).await?;
                if status(&run) == "waiting_external"
                    || (status(&run) == "running" && Self::pending_build_finish(&run))
                {
                    if status(&run) == "running" {
                        run["pending"]["external_job"] = json!({"kind":"v3_build",
                    "task_id":run["pending"]["arguments"]["task_id"],
                    "expected_inputs_sha256":run["pending"]["arguments"]["expected_inputs_sha256"],
                    "submitted_at":now()});
                        run["status"] = json!("waiting_external");
                        run["error"] = Value::Null;
                        user_app.save_override(&mut run).await?;
                    }
                    // Following the job only reads its receipt, so a model that is not yet
                    // authorized (as just after a deploy restarts this loop) does not stop it;
                    // the scheduler re-checks the model before the run continues.
                    let stop = if !resume {
                        Some("startup resume disabled")
                    } else if app.shutoff().await? {
                        Some("shutoff")
                    } else {
                        None
                    };
                    if let Some(reason) = stop {
                        tracing::warn!(run_id = %id, reason, "Build-waiting run stopped at recovery");
                        run["status"] = json!("stopped");
                        run["error"] = json!("Build result awaits authorized resume");
                        user_app.save_override(&mut run).await?;
                        continue;
                    }
                    if status(&run) == "waiting_external"
                        && user_app.message_pending(&run).await.unwrap_or(false)
                        && Self::detach_build(&user_app, &mut run).await?
                    {
                        continue;
                    }
                    let poll = tokio::time::timeout(
                        Duration::from_secs(3),
                        Self::build_result(&user_app, &run),
                    )
                    .await
                    .unwrap_or_else(|_| Err(anyhow!("Broker read timed out")));
                    Self::follow_build(&user_app, &mut run, poll).await?;
                    continue;
                }
                if status(&run) != "running" {
                    continue;
                }
                if (run["pending"]["state"] == "executing"
                    && !A::is_read(run["pending"]["name"].as_str().unwrap_or("")))
                    || super::batch::executing_write::<A>(&run)
                {
                    user_app
                        .actions
                        .interrupt_run(user_app.store.records(), &run)
                        .await?;
                    // A restart cut off a write: the agent gets a result naming the cause
                    // and asking it to check for a partial effect; the repeat guard
                    // remembers the call. No write waits for a review card.
                    match Self::restart_cause(app) {
                        Some(cause) => {
                            run["restart_interruption"] =
                                json!({"kind":"runtime_restart","cause":cause,"recorded_at":now()});
                        }
                        None if run["restart_interruption"].is_null() => {
                            run["restart_interruption"] =
                                json!({"kind":"unexpected_restart","recorded_at":now()});
                        }
                        None => {}
                    }
                    super::batch::settle_cut_off_writes::<A>(&mut run);
                    let review = false;
                    let name = run["pending"]["name"].as_str().unwrap_or("").to_owned();
                    if run["pending"]["state"] == "executing" && !A::is_read(&name) {
                        let retry = A::retry(&name);
                        let call_id = run["pending"]["call_id"].as_str().unwrap_or("").to_owned();
                        let args = run["pending"]["arguments"].clone();
                        tools::retry_guard::record(&mut run, &name, &args, &call_id, None, &retry);
                        let result = super::batch::cut_off_result(&run, &retry);
                        Self::append_tool(&mut run, &result)?;
                    }
                    run["status"] = json!("recovery");
                    run["uncertain"] = json!(true);
                    run["error"] = json!("Tool outcome is uncertain after interruption");
                    if !review {
                        run["uncertain"] = json!(false);
                        let permitted = resume
                            && !app.shutoff().await?
                            && user_app.run_model_authorized(&run).await
                            && user_app
                                .chat
                                .participation_allows_run(user_app.store.records(), &run)
                                .await?;
                        run["status"] = json!(if permitted { "queued" } else { "stopped" });
                        run["error"] = Value::Null;
                    } else if user_app
                        .actions
                        .recover_results(user_app.store.records(), &mut run)
                        .await?
                    {
                        run["uncertain"] = json!(false);
                        let permitted = resume
                            && !app.shutoff().await?
                            && user_app.run_model_authorized(&run).await
                            && user_app
                                .chat
                                .participation_allows_run(user_app.store.records(), &run)
                                .await?;
                        run["status"] = json!(if permitted { "queued" } else { "stopped" });
                        run["error"] =
                            json!("Recovered committed action result without replaying its effect");
                    }
                } else {
                    let allowed = user_app.run_model_authorized(&run).await;
                    run["status"] = json!(if !resume || app.shutoff().await? || !allowed {
                        "stopped"
                    } else {
                        "queued"
                    });
                    if run["pending"]["state"] == "executing" {
                        run["pending"]["state"] = json!("requested");
                    }
                    if let Some(items) = run["batch"].as_array_mut() {
                        for item in items {
                            if item["state"] == "executing" {
                                item["state"] = json!("requested");
                                item["pending"]["state"] = json!("requested");
                            }
                        }
                    }
                    // Keep a recorded failure visible while the run retries. A clean
                    // resume clears the pause note, so the restart leaves no trace.
                    if !resume {
                        run["error"] =
                            json!("Automatic startup resume is disabled; explicit resume required");
                    } else if !run["error"]
                        .as_str()
                        .is_some_and(|e| e.starts_with("Retrying"))
                    {
                        run["error"] = Value::Null;
                    }
                }
                user_app.save_override(&mut run).await?;
            }
            Self::follow_detached_builds(&user_app).await?;
        }
        if startup {
            let _ = std::fs::remove_file(app.handles.control_dir.join("restart-cause.json"));
        }
        Ok(())
    }
}

/// A short name for a background broker job's notice row: the check kind or "Release build".
fn job_title(follow: &Value) -> String {
    let args = &follow["pending"]["arguments"];
    match args["action"].as_str() {
        Some("check") => format!("check {}", args["check"].as_str().unwrap_or(""))
            .trim()
            .to_owned(),
        Some("finish") => "release build".to_owned(),
        _ => "job".to_owned(),
    }
}

/// Readable notice text for a job that ended without an alert line: status, reason and a
/// bounded log excerpt, never the raw receipt.
fn ended_summary(output: &Value, job_id: &str) -> String {
    let mut text = format!(
        "Job {} {}.",
        job_id.get(..8).unwrap_or(job_id),
        output["status"].as_str().unwrap_or("ended")
    );
    if let Some(reason) = output["reason"].as_str() {
        text.push_str(&format!(
            " {}",
            reason.chars().take(300).collect::<String>()
        ));
    }
    if let Some(log) = output["log"].as_str().filter(|log| !log.is_empty()) {
        text.push_str(&format!(
            "\n\n{}",
            log.chars().take(2000).collect::<String>()
        ));
    }
    text
}

/// Whether a broker reply leaves the call waiting on its job: an accepted finish, or a
/// check or `wait` whose job is still queued or running. The run then waits like it does
/// for a finish, instead of the model polling the job.
pub(crate) fn awaits_broker_job(action: &Value, output: &Value) -> bool {
    match action.as_str() {
        Some("finish") => output["status"] == "closed",
        Some("check" | "wait") => {
            output["job_id"].is_string()
                && matches!(output["status"].as_str(), Some("queued" | "running"))
        }
        _ => false,
    }
}

/// The task a pending finish named: the legacy task_id, or a returned Broker ID, which is
/// the task's label. A finish that continued the current work names neither; its job ID,
/// recorded when the broker accepted it, is enough to follow it.
fn finish_task(run: &Value) -> Option<&str> {
    let args = &run["pending"]["arguments"];
    args["task_id"]
        .as_str()
        .or_else(|| args["broker_id"].as_str().filter(|id| *id != "new"))
}

/// The Broker chip entry for `job_id`: under the task the record already files it under,
/// else the task the pending call names. A check records which check it ran.
fn job_entry<'a>(record: &'a mut Value, run: &Value, job_id: &str, now: u64) -> &'a mut Value {
    let args = &run["pending"]["arguments"];
    let task = build_tasks::task_of(record, job_id)
        .or_else(|| finish_task(run).map(str::to_owned))
        .unwrap_or_else(|| format!("job-{}", job_id.get(..8).unwrap_or(job_id)));
    let check = args["action"] == "check";
    let kind = build_tasks::JobKind {
        kind: if check { "check" } else { "build" },
        check: check.then(|| args["check"].as_str().unwrap_or("check")),
    };
    let rid = run["id"].as_str().unwrap_or_default();
    build_tasks::job(record, &task, job_id, rid, &kind, now)
}

/// Show a job on the Broker chip as soon as its call starts waiting on it; the first
/// recovery poll adds its stages and task name. The chip is a view, so a failed write is
/// only logged.
pub(super) async fn note_build_job<S: super::ports::ScopedStorage>(
    store: &S,
    run: &Value,
    output: &Value,
) {
    let (Some(sid), Some(job_id)) = (run["session_id"].as_str(), output["job_id"].as_str()) else {
        return;
    };
    let written = build_tasks::update(store, sid, |record| {
        job_entry(record, run, job_id, now());
        build_tasks::trim(record);
    })
    .await;
    if let Err(error) = written {
        tracing::warn!("Broker chip record not written: {error}");
    }
}

/// How long a finished release's result waits for the broker's commit and claim release.
const CLOSEOUT_WAIT_SECONDS: f64 = 120.0;

/// What one poll of an in-app build's broker job found.
enum BuildPoll {
    /// The job ended; this becomes the pending finish call's result.
    Done(Value),
    /// Still queued or running, with the broker's stage table when it has one.
    Waiting(Option<Value>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finishes_and_unfinished_checks_wait_for_their_job() {
        let finish = json!("finish");
        let check = json!("check");
        assert!(awaits_broker_job(
            &finish,
            &json!({"status":"closed","job_id":"j"})
        ));
        assert!(!awaits_broker_job(&finish, &json!({"status":"open"})));
        assert!(awaits_broker_job(
            &check,
            &json!({"status":"queued","job_id":"j"})
        ));
        assert!(awaits_broker_job(
            &check,
            &json!({"status":"running","job_id":"j"})
        ));
        assert!(!awaits_broker_job(
            &check,
            &json!({"status":"completed","job_id":"j"})
        ));
        assert!(!awaits_broker_job(&check, &json!({"status":"queued"})));
        let wait = json!("wait");
        assert!(awaits_broker_job(
            &wait,
            &json!({"status":"running","job_id":"j"})
        ));
        assert!(!awaits_broker_job(
            &wait,
            &json!({"status":"failed","job_id":"j"})
        ));
        assert!(!awaits_broker_job(
            &json!("status"),
            &json!({"status":"queued","job_id":"j"})
        ));
    }

    #[test]
    fn a_finish_names_its_task_by_task_id_or_broker_id() {
        let named = |args: Value| json!({"pending":{"arguments":args}});
        let run = named(json!({"task_id":"t-1"}));
        assert_eq!(finish_task(&run), Some("t-1"));
        let run = named(json!({"broker_id":"broker-abc"}));
        assert_eq!(finish_task(&run), Some("broker-abc"));
        assert_eq!(finish_task(&named(json!({"broker_id":"new"}))), None);
        assert_eq!(finish_task(&named(json!({}))), None);
    }
}
