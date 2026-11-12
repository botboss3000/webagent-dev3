//! The chat's Broker chip API: the broker tasks a session's builds and checks ran (the
//! engine records them while it follows each job), one task's jobs with their stage
//! snapshots, clearing finished tasks from the chip, and its manual Stop and Cancel. The
//! session router has already scoped `app` to the session and checked read or write access.
use super::*;
#[cfg(feature = "administrator")]
use crate::tools::administrator::{host_fingerprint, Host};
use tokio_util::sync::CancellationToken;
use webagent_engine::runtime::build_tasks::record_key;

/// Job states in which the broker still runs, or may still run, the job.
const ACTIVE: &[&str] = &["queued", "running", "needs_decision"];
/// An active job's snapshot older than this is read again from the broker. A job the
/// engine is not following (its agent was woken and has not waited again) only refreshes
/// this way.
const STALE_SECONDS: u64 = 30;
/// Most jobs one request refreshes.
const REFRESH_LIMIT: usize = 5;

fn active(job: &Value) -> bool {
    job["status"].as_str().is_some_and(|s| ACTIVE.contains(&s))
}

fn jobs(task: &Value) -> &[Value] {
    task["jobs"].as_array().map_or(&[][..], Vec::as_slice)
}

async fn record(app: &App, sid: &str) -> Result<Value> {
    Ok(app
        .store
        .get(&record_key(sid))
        .await?
        .filter(|record| record["tasks"].is_array())
        .unwrap_or_else(|| json!({"tasks":[]})))
}

/// The caller's broker identity for jobs a run of this session started, if the caller
/// may run `build_work` there now (Administrator ability permissions).
#[cfg(feature = "administrator")]
async fn identity(app: &App, sid: &str, run_id: &str) -> Option<Value> {
    let fingerprint = host_fingerprint(app, sid, run_id, "build_work")
        .await
        .ok()?;
    Some(webagent_administration_host::trusted(
        crate::shared_chat::actor(app),
        sid,
        &json!(fingerprint),
    ))
}

/// Without the Administrator tier no caller has a broker identity.
#[cfg(not(feature = "administrator"))]
async fn identity(_: &App, _: &str, _: &str) -> Option<Value> {
    None
}

/// Read stale snapshots of active jobs again from the broker; true when any changed.
async fn refresh(app: &App, sid: &str, record: &mut Value, only_task: Option<&str>) -> bool {
    let now = now();
    let mut stale = Vec::new();
    for (t, task) in record["tasks"].as_array().into_iter().flatten().enumerate() {
        if only_task.is_some_and(|id| task["task_id"] != id) {
            continue;
        }
        for (j, job) in jobs(task).iter().enumerate() {
            let fresh = job["snapshot_at"].as_u64().unwrap_or(0) + STALE_SECONDS > now;
            if active(job) && !fresh {
                stale.push((t, j));
            }
        }
    }
    let mut changed = false;
    for (t, j) in stale.into_iter().take(REFRESH_LIMIT) {
        let job = &record["tasks"][t]["jobs"][j];
        let (Some(job_id), Some(run_id)) = (job["job_id"].as_str(), job["run_id"].as_str()) else {
            continue;
        };
        let Some(trusted) = identity(app, sid, run_id).await else {
            continue;
        };
        let reply = crate::tools::execute_host_trusted(
            "v3_build_progress",
            &json!({"job_id":job_id}),
            sid,
            &app.build_socket,
            Some(trusted),
            CancellationToken::new(),
        )
        .await;
        if let Some(output) = progress_output(reply) {
            let job = &mut record["tasks"][t]["jobs"][j];
            job["status"] = output["status"].clone();
            job["snapshot"] = output;
            job["snapshot_at"] = json!(now);
            changed = true;
        }
    }
    changed
}

/// A progress reply's output when the broker answered with one.
fn progress_output(reply: Result<Value>) -> Option<Value> {
    let reply = reply.ok()?;
    (reply["status"] == "success" && reply["output"]["status"].is_string())
        .then(|| reply["output"].clone())
}

/// What a chip row shows for a task: the newest job's state, and a warning when the
/// broker stopped answering or the running stage stalled.
fn summary(task: &Value) -> Value {
    let newest = jobs(task).last().cloned().unwrap_or(Value::Null);
    let warning = if newest["broker"] == "unresponsive" {
        Some("broker_unresponsive")
    } else if active(&newest) && newest["snapshot"]["idle_seconds"].as_u64().unwrap_or(0) > 900 {
        Some("stalled")
    } else {
        None
    };
    json!({"task_id":task["task_id"],"name":task["name"],"status":newest["status"],
        "kind":newest["kind"],"warning":warning,"updated_at":task["updated_at"],
        "abandoned":task["abandoned"]==true,"jobs":jobs(task).len()})
}

/// Broker tasks whose newest job is still active, as `{task_id,name}`: a user's session
/// stop leaves them building.
pub(crate) async fn building(app: &App, sid: &str) -> Result<Vec<Value>> {
    let record = record(app, sid).await?;
    Ok(newest_first(&record)
        .into_iter()
        .filter(|task| jobs(task).last().is_some_and(active))
        .map(|task| json!({"task_id":task["task_id"],"name":task["name"]}))
        .collect())
}

fn newest_first(record: &Value) -> Vec<&Value> {
    let mut tasks: Vec<&Value> = record["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|task| task["cleared"] != true)
        .collect();
    tasks.sort_by_key(|task| std::cmp::Reverse(task["updated_at"].as_u64().unwrap_or(0)));
    tasks
}

/// The session's broker tasks, newest first, without cleared ones.
pub(crate) async fn list(app: &App, sid: &str) -> Api {
    let mut record = record(app, sid).await?;
    if refresh(app, sid, &mut record, None).await {
        app.store.put(&record_key(sid), &record).await?;
    }
    let tasks: Vec<Value> = newest_first(&record).into_iter().map(summary).collect();
    Ok(Json(json!({"tasks":tasks,"now":now()})))
}

fn find<'a>(record: &'a Value, task_id: &str) -> Result<&'a Value, ApiError> {
    record["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|task| task["task_id"] == task_id)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Broker task not found".into()))
}

/// One task with its jobs (newest first) and stage snapshots, and whether the caller may
/// stop or cancel them. `now` lets the card count from the server's clock.
pub(crate) async fn detail(app: &App, sid: &str, task_id: &str, can_write: bool) -> Api {
    let mut record = record(app, sid).await?;
    find(&record, task_id)?;
    if refresh(app, sid, &mut record, Some(task_id)).await {
        app.store.put(&record_key(sid), &record).await?;
    }
    let task = find(&record, task_id)?;
    let run_id = task["run_id"].as_str().unwrap_or_default();
    let can_control = can_write && identity(app, sid, run_id).await.is_some();
    let mut view = summary(task);
    let mut list: Vec<Value> = jobs(task).to_vec();
    list.reverse();
    for job in &mut list {
        if let Some(object) = job.as_object_mut() {
            // Alert bookkeeping is the engine's; the card needs only the snapshot.
            for key in ["alerted", "stalled", "failures", "run_id"] {
                object.remove(key);
            }
        }
    }
    view["jobs"] = json!(list);
    Ok(Json(
        json!({"task":view,"can_control":can_control,"now":now()}),
    ))
}

/// Hide finished tasks from the chip: one task, or every finished one. The broker's own
/// records are untouched.
pub(crate) async fn clear(app: &App, sid: &str, task_id: Option<&str>) -> Api {
    let mut record = record(app, sid).await?;
    if let Some(task_id) = task_id {
        if jobs(find(&record, task_id)?).iter().any(active) {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Only a finished task can be cleared".into(),
            ));
        }
    }
    let mut cleared = 0;
    for task in record["tasks"].as_array_mut().into_iter().flatten() {
        let selected = task_id.map_or(true, |id| task["task_id"] == id);
        if selected && task["cleared"] != true && !jobs(task).iter().any(active) {
            task["cleared"] = json!(true);
            cleared += 1;
        }
    }
    app.store.put(&record_key(sid), &record).await?;
    Ok(Json(json!({"cleared":cleared})))
}

/// A broker reply as the chip's answer: its output, or a conflict naming the refusal.
#[cfg(feature = "administrator")]
fn answer(reply: Result<Value>) -> Api {
    let reply = reply.map_err(|_| {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "The broker did not answer; try again".into(),
        )
    })?;
    if reply["status"] == "success" {
        return Ok(Json(reply["output"].clone()));
    }
    let message = reply["output"]
        .as_str()
        .or_else(|| reply["error"].as_str())
        .unwrap_or("The broker refused the request");
    Err(ApiError(
        StatusCode::CONFLICT,
        message.chars().take(300).collect(),
    ))
}

#[cfg(feature = "administrator")]
const DENIED: &str = "Stopping broker work needs Administrator ability permissions here";

/// Stop one job and keep its task, so it can be reopened and fixed.
#[cfg(feature = "administrator")]
pub(crate) async fn stop(app: &App, sid: &str, task_id: &str, job_id: &str) -> Api {
    let record = record(app, sid).await?;
    let task = find(&record, task_id)?;
    let job = jobs(task)
        .iter()
        .find(|job| job["job_id"] == job_id)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Broker job not found".into()))?;
    let run_id = job["run_id"].as_str().unwrap_or_default();
    if identity(app, sid, run_id).await.is_none() {
        return Err(ApiError(StatusCode::FORBIDDEN, DENIED.into()));
    }
    let request_id = format!("chip-stop-{}", crate::uuid());
    answer(
        webagent_administration_host::tools::stop_job(
            &Host(app),
            sid,
            run_id,
            job_id,
            &request_id,
            CancellationToken::new(),
        )
        .await,
    )
}

/// Cancel the whole task: stop its active job, then abandon it.
#[cfg(feature = "administrator")]
pub(crate) async fn cancel(app: &App, sid: &str, task_id: &str) -> Api {
    let record = record(app, sid).await?;
    let task = find(&record, task_id)?;
    let run_id = task["run_id"].as_str().unwrap_or_default().to_owned();
    if identity(app, sid, &run_id).await.is_none() {
        return Err(ApiError(StatusCode::FORBIDDEN, DENIED.into()));
    }
    let job_id = jobs(task)
        .iter()
        .rev()
        .find(|job| active(job))
        .and_then(|job| job["job_id"].as_str())
        .map(str::to_owned);
    let request_id = format!("chip-cancel-{}", crate::uuid());
    let reply = webagent_administration_host::tools::cancel_task(
        &Host(app),
        sid,
        &run_id,
        task_id,
        job_id.as_deref(),
        &request_id,
    )
    .await;
    if reply
        .as_ref()
        .is_ok_and(|reply| reply["output"]["state"] == "abandoned")
    {
        webagent_engine::runtime::build_tasks::update(&app.store, sid, |record| {
            for task in record["tasks"].as_array_mut().into_iter().flatten() {
                if task["task_id"] == task_id {
                    task["abandoned"] = json!(true);
                }
            }
        })
        .await?;
    }
    answer(reply)
}

/// Without the Administrator tier there is no broker work to stop.
#[cfg(not(feature = "administrator"))]
pub(crate) async fn stop(_: &App, _: &str, _: &str, _: &str) -> Api {
    Err(no_broker())
}

/// Without the Administrator tier there is no broker task to cancel.
#[cfg(not(feature = "administrator"))]
pub(crate) async fn cancel(_: &App, _: &str, _: &str) -> Api {
    Err(no_broker())
}

#[cfg(not(feature = "administrator"))]
fn no_broker() -> ApiError {
    ApiError(
        StatusCode::NOT_FOUND,
        "Broker work needs the Administrator tier".into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_show_the_newest_job_and_warn_on_a_stall_or_silent_broker() {
        let task = json!({"task_id":"chip","name":"Broker chip","updated_at":5,"jobs":[
            {"job_id":"a","kind":"check","status":"completed"},
            {"job_id":"b","kind":"build","status":"running","snapshot":{"idle_seconds":901}}]});
        let row = summary(&task);
        assert_eq!(row["status"], "running");
        assert_eq!(row["kind"], "build");
        assert_eq!(row["warning"], "stalled");
        assert_eq!(row["jobs"], 2);
        let mut silent = task.clone();
        silent["jobs"][1]["broker"] = json!("unresponsive");
        assert_eq!(summary(&silent)["warning"], "broker_unresponsive");
        let record = json!({"tasks":[
            {"task_id":"old","updated_at":1},{"task_id":"new","updated_at":9},
            {"task_id":"gone","updated_at":20,"cleared":true}]});
        let order: Vec<&Value> = newest_first(&record)
            .into_iter()
            .map(|task| &task["task_id"])
            .collect();
        assert_eq!(order, [&json!("new"), &json!("old")]);
    }
}
