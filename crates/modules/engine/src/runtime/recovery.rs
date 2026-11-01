use super::{config, models::ModelRecords, ports::*, Runtime};
use crate::context::abilities::Catalog;
use crate::ports::*;
use crate::tools::{self, execution as tool_execution};
use crate::{digest, now, status, uuid, workflow};
use anyhow::bail;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use webagent_access as access;
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
    /// A pending broker call whose job the run is waiting on: a finish, or a queued check.
    fn pending_build_job(run: &Value) -> bool {
        matches!(
            run["pending"]["name"].as_str(),
            Some("v3_build_work" | "build_work")
        ) && matches!(
            run["pending"]["arguments"]["action"].as_str(),
            Some("finish" | "check")
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
    async fn build_result(app: &Self, run: &Value) -> Result<BuildPoll> {
        let sid = run["session_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing session"))?;
        let rid = run["id"].as_str().ok_or_else(|| anyhow!("Missing run"))?;
        let task = finish_task(run);
        let check = run["pending"]["arguments"]["action"] == "check";
        let authority = match (run["pending"]["name"] == "build_work", check) {
            (true, true) => "build_work",
            (true, false) => "build_work_finish",
            (false, true) => "v3_build_work",
            (false, false) => "v3_build_work_finish",
        };
        let fingerprint = app
            .native
            .host_fingerprint(app.native_view(), sid, rid, authority)
            .await?;
        let trusted = json!({"user_id":app.store.current_user_id(),"session_id":sid,
        "lease_until":now()+30,"access_fingerprint":fingerprint});
        let socket_path = app.native.recovery_build_socket(app.native_view());
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
        let job_id = job_id.unwrap();
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
            && !check
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
                let id = view["id"].as_str().unwrap();
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
                    if !resume || app.shutoff().await? || !user_app.run_model_authorized(&run).await
                    {
                        run["status"] = json!("stopped");
                        run["error"] = json!("Build result awaits authorized resume");
                        user_app.save_override(&mut run).await?;
                        continue;
                    }
                    match tokio::time::timeout(
                        Duration::from_secs(3),
                        Self::build_result(&user_app, &run),
                    )
                    .await
                    {
                        Ok(Ok(BuildPoll::Waiting(Some(progress)))) => {
                            if let Some((key, text)) = progress_notice(&run, &progress) {
                                run["pending"]["external_job"]["progress_key"] = json!(key);
                                let message_id = format!("working:{id}:build:{key}");
                                user_app
                                    .store
                                    .save_run_override_observed(
                                        &mut run,
                                        "working_message",
                                        &json!({"id":message_id,"text":text}),
                                    )
                                    .await?;
                            }
                        }
                        Ok(Ok(BuildPoll::Done(result))) => {
                            Self::attach_external_build_result(&mut run, &result)?;
                            user_app
                                .store
                                .save_run_override(
                                    &mut run,
                                    "tool_result",
                                    &json!({"run_id":id,"name":"v3_build_work","result":result}),
                                )
                                .await?;
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(run_id=%id,"Build receipt reconciliation deferred: {error}");
                        }
                        _ => {}
                    }
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
                    run["status"] = json!("recovery");
                    run["uncertain"] = json!(true);
                    super::batch::mark_uncertain_writes::<A>(&mut run);
                    run["error"] = json!("Tool outcome is uncertain after interruption");
                    if user_app
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
                    run["error"] = json!(if !resume {
                        "Automatic startup resume is disabled; explicit resume required"
                    } else {
                        "Recovered after interruption"
                    });
                }
                user_app.save_override(&mut run).await?;
            }
        }
        Ok(())
    }
}

/// Whether a broker reply leaves the call waiting on its job: an accepted finish, or a
/// check that is still queued or running. The run then waits like it does for a finish,
/// with stage lines, instead of the model polling the job.
pub(crate) fn awaits_broker_job(action: &Value, output: &Value) -> bool {
    match action.as_str() {
        Some("finish") => output["status"] == "closed",
        Some("check") => {
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

/// How long a finished release's result waits for the broker's commit and claim release.
const CLOSEOUT_WAIT_SECONDS: f64 = 120.0;

/// What one poll of an in-app build's broker job found.
enum BuildPoll {
    /// The job ended; this becomes the pending finish call's result.
    Done(Value),
    /// Still queued or running, with the broker's stage table when it has one.
    Waiting(Option<Value>),
}

/// The chat line for a build stage change, keyed so each change is posted once. A
/// verdict change counts, so a stage running past its normal range is reported too.
fn progress_notice(run: &Value, progress: &Value) -> Option<(String, String)> {
    let job = progress["job_id"].as_str()?;
    let short = job.get(..8).unwrap_or(job);
    let stage = progress["stage"].as_str().unwrap_or("queued");
    let verdict = progress["verdict"].as_str().unwrap_or("");
    let finished = progress["finished_stages"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    let key = format!("{short}:{}:{stage}:{verdict}", finished.len()).replace(' ', "-");
    if run["pending"]["external_job"]["progress_key"] == key.as_str() {
        return None;
    }
    let mut text = format!("Build {short}: ");
    if progress["stage_count"]
        .as_u64()
        .is_some_and(|count| count > 0)
    {
        text += &format!(
            "stage {} of {}, ",
            progress["stage_index"], progress["stage_count"]
        );
    }
    text += stage;
    if let Some(seconds) = progress["stage_seconds"].as_u64().filter(|s| *s > 0) {
        text += &format!(" running {}", minutes(seconds));
    }
    if !progress["stage_normal"].is_null() {
        text += &format!(" (normal {})", range(&progress["stage_normal"]));
    }
    text += &format!(", {verdict}.");
    if let Some(last) = finished.last() {
        text += &format!(
            " Last finished: {} in {} (normal {}).",
            last["stage"].as_str().unwrap_or("stage"),
            minutes(last["seconds"].as_u64().unwrap_or(0)),
            range(&last["normal"])
        );
    }
    if let [low, high] = progress["remaining_seconds"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
    {
        text += &format!(
            " About {}–{} left.",
            minutes(low.as_u64().unwrap_or(0)),
            minutes(high.as_u64().unwrap_or(0))
        );
    }
    Some((key, text))
}

fn minutes(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m", (seconds + 30) / 60)
    }
}

/// A stage's normal p50–p90 range, or a note that its class has no history yet.
fn range(normal: &Value) -> String {
    match normal.as_array().map(Vec::as_slice) {
        Some([low, high]) => format!(
            "{}–{}",
            minutes(low.as_u64().unwrap_or(0)),
            minutes(high.as_u64().unwrap_or(0))
        ),
        _ => "no history".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(stage: &str, finished: usize, verdict: &str) -> Value {
        let done: Vec<Value> = (0..finished)
            .map(|_| json!({"stage":"compile tests","seconds":480,"normal":[540,1140]}))
            .collect();
        json!({"job_id":"0420b7ab-6855-409a-9dc3-6ab5efc3890f","stage":stage,
            "stage_index":finished + 1,"stage_count":6,"stage_seconds":125,
            "stage_normal":[300,600],"verdict":verdict,"finished_stages":done,
            "remaining_seconds":[900,3300]})
    }

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

    #[test]
    fn each_stage_change_is_posted_once() {
        let mut run = json!({"pending":{"external_job":{"kind":"v3_build"}}});
        let (key, text) = progress_notice(&run, &progress("run tests", 2, "on track")).unwrap();
        assert_eq!(
            text,
            "Build 0420b7ab: stage 3 of 6, run tests running 2m (normal 5m–10m), on track. \
             Last finished: compile tests in 8m (normal 9m–19m). About 15m–55m left."
        );
        assert!(!key.contains(' '));
        run["pending"]["external_job"]["progress_key"] = json!(key);
        assert!(progress_notice(&run, &progress("run tests", 2, "on track")).is_none());
        assert!(progress_notice(&run, &progress("run tests", 2, "slow, past p90")).is_some());
        assert!(progress_notice(&run, &progress("release build", 3, "on track")).is_some());
    }

    #[test]
    fn a_queued_job_without_history_still_reads_plainly() {
        let run = json!({"pending":{"external_job":{}}});
        let queued = json!({"job_id":"abc","stage":"queued","stage_index":1,"stage_count":0,
            "stage_seconds":0,"stage_normal":null,"verdict":"no baseline","finished_stages":[]});
        let (_, text) = progress_notice(&run, &queued).unwrap();
        assert_eq!(text, "Build abc: queued, no baseline.");
        assert!(progress_notice(&run, &json!({})).is_none());
    }
}
