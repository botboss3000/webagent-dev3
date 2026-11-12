//! The chat's Broker chip record: the broker tasks a session's builds and checks ran,
//! each job's latest stage snapshot, and the alert state that wakes a waiting agent.
//! `begin`/`reserve` add the task; recovery writes jobs while it follows them; the session
//! API lists and clears it.
//!
//! shortcut: the record is read-modify-written without a revision, so a clear racing a
//! recovery write can be lost (the task shows again); add a revision if that matters.
use super::ports::ScopedStorage;
use anyhow::Result;
use serde_json::{json, Value};

/// Tasks kept per session; the least recently updated drop first.
const MAX_TASKS: usize = 50;
/// Jobs kept per task; the oldest drop first.
const MAX_JOBS: usize = 20;
/// Seconds without reported progress before a running stage reads as stalled.
const STALL_SECONDS: u64 = 900;
/// The longest gap between slow-stage check-ins.
const LATE_EVERY: u64 = 300;
/// Slow-stage check-ins need this many recent jobs behind the stage's range.
const LATE_SAMPLES: u64 = 5;
/// Consecutive failed broker reads before the broker reads as not responding.
pub const UNRESPONSIVE_TRIES: u64 = 3;

/// The record key for one session.
pub fn record_key(sid: &str) -> String {
    crate::key("build_tasks", sid)
}

/// Read, change and write one session's record.
pub async fn update<S: ScopedStorage, T>(
    store: &S,
    sid: &str,
    change: impl FnOnce(&mut Value) -> T,
) -> Result<T> {
    let key = record_key(sid);
    let mut record = store
        .get(&key)
        .await?
        .unwrap_or_else(|| json!({"tasks":[]}));
    if !record["tasks"].is_array() {
        record = json!({"tasks":[]});
    }
    let value = change(&mut record);
    store.put(&key, &record).await?;
    Ok(value)
}

/// The chip's task name: the broker summary without its provenance bracket or a leading
/// `chat <session id>:` the agent may have written.
pub fn task_name(summary: &str) -> String {
    let name = summary.split(" [origin=").next().unwrap_or(summary).trim();
    let name = name
        .strip_prefix("chat ")
        .and_then(|rest| rest.split_once(':'))
        .filter(|(id, _)| !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
        .map_or(name, |(_, rest)| rest.trim());
    name.chars().take(120).collect()
}

/// The task a known job belongs to, if the record has it.
pub fn task_of(record: &Value, job_id: &str) -> Option<String> {
    tasks(record)
        .iter()
        .find(|task| jobs(task).iter().any(|job| job["job_id"] == job_id))
        .and_then(|task| task["task_id"].as_str().map(str::to_owned))
}

/// Whether the record already has a name for `task_id`.
pub fn named(record: &Value, task_id: &str) -> bool {
    tasks(record)
        .iter()
        .any(|task| task["task_id"] == task_id && task["name"].is_string())
}

/// What a recorded job is: a release build or one kind of check.
pub struct JobKind<'a> {
    pub kind: &'a str,
    /// The check that ran (fmt, check, test, clippy, ui_test), for a check.
    pub check: Option<&'a str>,
}

/// The job's entry, created under its task when first seen. A new job on a cleared task
/// shows the task again.
pub fn job<'a>(
    record: &'a mut Value,
    task_id: &str,
    job_id: &str,
    run_id: &str,
    kind: &JobKind<'_>,
    now: u64,
) -> &'a mut Value {
    let index = tasks(record)
        .iter()
        .position(|task| task["task_id"] == task_id)
        .unwrap_or_else(|| {
            push(
                &mut record["tasks"],
                json!({"task_id":task_id,"jobs":[],"created_at":now}),
            )
        });
    let task = &mut record["tasks"][index];
    task["run_id"] = json!(run_id);
    task["updated_at"] = json!(now);
    let at = match jobs(task).iter().position(|job| job["job_id"] == job_id) {
        Some(at) => at,
        None => {
            task["cleared"] = json!(false);
            let entry = json!({"job_id":job_id,"kind":kind.kind,"check":kind.check,
                "status":"queued","created_at":now});
            let at = push(&mut task["jobs"], entry);
            if at >= MAX_JOBS {
                if let Some(list) = task["jobs"].as_array_mut() {
                    list.remove(0);
                }
                at - 1
            } else {
                at
            }
        }
    };
    let job = &mut task["jobs"][at];
    job["run_id"] = json!(run_id);
    job["updated_at"] = json!(now);
    job
}

/// Append to a JSON array (made one if it is not) and return the new item's index.
fn push(list: &mut Value, item: Value) -> usize {
    if !list.is_array() {
        *list = json!([]);
    }
    match list.as_array_mut() {
        Some(items) => {
            items.push(item);
            items.len() - 1
        }
        None => 0,
    }
}

/// Drop the least recently updated tasks past the per-session limit.
pub fn trim(record: &mut Value) {
    if let Some(list) = record["tasks"].as_array_mut() {
        list.sort_by_key(|task| std::cmp::Reverse(task["updated_at"].as_u64().unwrap_or(0)));
        list.truncate(MAX_TASKS);
    }
}

/// Name a task once its broker summary is known.
pub fn name_task(record: &mut Value, task_id: &str, summary: &str) {
    if let Some(list) = record["tasks"].as_array_mut() {
        for task in list.iter_mut().filter(|task| task["task_id"] == task_id) {
            task["name"] = json!(task_name(summary));
        }
    }
}

/// Show a task on the chip before it has a job, named from its broker summary, so a chat
/// shows its task from `begin` or `reserve` on. A known task keeps its jobs and only gains
/// a name it lacked.
pub fn note_task(record: &mut Value, task_id: &str, summary: Option<&str>, now: u64) {
    if !tasks(record).iter().any(|task| task["task_id"] == task_id) {
        push(
            &mut record["tasks"],
            json!({"task_id":task_id,"jobs":[],"created_at":now,"updated_at":now}),
        );
    }
    if let Some(summary) = summary.filter(|_| !named(record, task_id)) {
        name_task(record, task_id, summary);
    }
}

/// Store a fresh broker stage snapshot on a job.
pub fn snapshot(job: &mut Value, progress: &Value, now: u64) {
    if let Some(status) = progress["status"].as_str() {
        job["status"] = json!(status);
    }
    job["snapshot"] = progress.clone();
    job["snapshot_at"] = json!(now);
}

/// Why a waiting build call returns early to its agent.
#[derive(Debug, PartialEq, Eq)]
pub enum Reason {
    /// A stage ran past its p95, or another check-in after that is due.
    Slow,
    /// The live step reported nothing for 15 minutes.
    Stalled,
    /// The broker did not answer several reads in a row.
    Unresponsive,
}

impl Reason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Slow => "slow",
            Self::Stalled => "stalled",
            Self::Unresponsive => "broker_unresponsive",
        }
    }
}

/// The next alert for this snapshot, as buildctl monitor raises them: past p95 (once five
/// recent jobs give the stage a range), then every min(p95, 5 min) while the stage runs;
/// and once per stage when the live step reports nothing for 15 minutes. The job keeps
/// which alerts were raised, so a follow-up `wait` does not raise them again.
pub fn alert(job: &mut Value, progress: &Value) -> Option<(Reason, String)> {
    let short = short_id(progress["job_id"].as_str().unwrap_or(""));
    let idle = progress["idle_seconds"].as_u64().unwrap_or(0);
    let running = progress["running_stages"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for stage in &running {
        let name = stage["stage"].as_str().unwrap_or("stage");
        let seconds = stage["seconds"].as_u64().unwrap_or(0);
        let label = format!("stage {} {name}", stage["id"].as_str().unwrap_or("?"));
        if progress["stage"] == name && idle > STALL_SECONDS && !stalled(job, name) {
            mark_stalled(job, name);
            return Some((
                Reason::Stalled,
                format!(
                    "Build {short}: {label} has reported no progress for {} min \
                     (running {} min).",
                    tenths(idle),
                    tenths(seconds)
                ),
            ));
        }
        let late = stage["late"].as_u64().unwrap_or(0);
        if late == 0 || stage["samples"].as_u64().unwrap_or(0) < LATE_SAMPLES {
            continue;
        }
        let every = late.min(LATE_EVERY);
        let raised = job["alerted"][name].as_u64().unwrap_or(0);
        if seconds >= late + raised * every {
            job["alerted"][name] = json!((seconds - late) / every + 1);
            let normal = match stage["normal"].as_array().map(Vec::as_slice) {
                Some([low, high]) => format!(
                    " Normal {}–{} min.",
                    tenths(low.as_u64().unwrap_or(0)),
                    tenths(high.as_u64().unwrap_or(0))
                ),
                _ => String::new(),
            };
            return Some((
                Reason::Slow,
                format!(
                    "Build {short}: {label} has run {} min, past its normal high of {} min \
                     (p95).{normal}",
                    tenths(seconds),
                    tenths(late)
                ),
            ));
        }
    }
    // Between stages: running, nothing running, and no activity.
    if progress["status"] == "running"
        && running.is_empty()
        && idle > STALL_SECONDS
        && !stalled(job, "")
    {
        mark_stalled(job, "");
        return Some((
            Reason::Stalled,
            format!(
                "Build {short}: no stage has started or reported progress for {} min.",
                tenths(idle)
            ),
        ));
    }
    None
}

fn stalled(job: &Value, stage: &str) -> bool {
    job["stalled"]
        .as_array()
        .is_some_and(|names| names.iter().any(|name| name == stage))
}

fn mark_stalled(job: &mut Value, stage: &str) {
    if !job["stalled"].is_array() {
        job["stalled"] = json!([]);
    }
    if let Some(names) = job["stalled"].as_array_mut() {
        names.push(json!(stage));
    }
}

/// Count a failed broker read (no answer, or the read was refused). Returns the alert when
/// this read makes the broker count as not responding; later failures stay quiet until it answers again.
pub fn broker_failed(job: &mut Value) -> Option<(Reason, String)> {
    let failures = job["failures"].as_u64().unwrap_or(0) + 1;
    job["failures"] = json!(failures);
    if failures < UNRESPONSIVE_TRIES || job["broker"] == "unresponsive" {
        return None;
    }
    job["broker"] = json!("unresponsive");
    let short = short_id(job["job_id"].as_str().unwrap_or(""));
    Some((
        Reason::Unresponsive,
        format!(
            "Build {short}: the broker could not be read {failures} checks in a row; the job's \
             state is unknown."
        ),
    ))
}

/// Clear the failure count after a broker answer. Returns the line to post when the
/// broker had been counted as not responding.
pub fn broker_answered(job: &mut Value) -> Option<String> {
    job["failures"] = json!(0);
    if job["broker"] != "unresponsive" {
        return None;
    }
    job["broker"] = json!("ok");
    let short = short_id(job["job_id"].as_str().unwrap_or(""));
    Some(format!("Build {short}: the broker is answering again."))
}

/// The result a waiting build call returns with when an alert wakes its agent. The job
/// keeps running; the agent chooses to wait again, inspect or stop it.
pub fn early_result(reason: &Reason, text: &str, job_id: &str, task_id: Option<&str>) -> Value {
    json!({"status":"success","output":{"status":"still_running","reason":reason.as_str(),
        "alert":text,"job_id":job_id,"task_id":task_id,
        "next":"The job keeps running. Call build_work with action=wait and this job_id to \
                keep waiting (it returns at the next alert or when the job ends), action=status \
                with job_id to inspect it, or action=cancel with job_id to stop it; the task \
                stays open for a fix."}})
}

/// The chat line for a job that ended without completing, if it did.
pub fn ended_line(output: &Value) -> Option<String> {
    let state = output["status"].as_str()?;
    if !matches!(
        state,
        "failed" | "stale" | "cancelled" | "uncertain" | "interrupted" | "needs_decision"
    ) {
        return None;
    }
    let short = short_id(output["job_id"].as_str().unwrap_or(""));
    let reason = output["reason"]
        .as_str()
        .map(|reason| format!(": {}", reason.chars().take(300).collect::<String>()))
        .unwrap_or_default();
    Some(format!(
        "Build {short} {}{reason}.",
        state.replace('_', " ")
    ))
}

fn short_id(job_id: &str) -> &str {
    job_id.get(..8).unwrap_or(job_id)
}

/// Minutes with one decimal, as the chip shows them.
fn tenths(seconds: u64) -> String {
    format!("{:.1}", seconds as f64 / 60.0)
}

fn tasks(record: &Value) -> &[Value] {
    record["tasks"].as_array().map_or(&[][..], Vec::as_slice)
}

fn jobs(task: &Value) -> &[Value] {
    task["jobs"].as_array().map_or(&[][..], Vec::as_slice)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running(seconds: u64, idle: u64, samples: u64) -> Value {
        json!({"job_id":"0420b7ab-6855-409a-9dc3-6ab5efc3890f","status":"running",
            "stage":"release build","idle_seconds":idle,
            "running_stages":[{"stage":"release build","id":"3b","seconds":seconds,
                "normal":[540,1020],"late":1140,"samples":samples}]})
    }

    #[test]
    fn slow_stages_alert_past_p95_then_every_five_minutes() {
        let mut job = json!({});
        assert!(alert(&mut job, &running(1100, 10, 8)).is_none());
        let (reason, text) = alert(&mut job, &running(1140, 10, 8)).unwrap();
        assert_eq!(reason, Reason::Slow);
        assert_eq!(
            text,
            "Build 0420b7ab: stage 3b release build has run 19.0 min, past its normal high of \
             19.0 min (p95). Normal 9.0–17.0 min."
        );
        // Raised once; the next check-in is 5 minutes later.
        assert!(alert(&mut job, &running(1200, 10, 8)).is_none());
        assert!(alert(&mut job, &running(1440, 10, 8)).is_some());
        assert!(alert(&mut job, &running(1500, 10, 8)).is_none());
        // A stage with few samples has no range to be late against.
        assert!(alert(&mut json!({}), &running(5000, 10, 4)).is_none());
    }

    #[test]
    fn a_stall_alerts_once_per_stage() {
        let mut job = json!({});
        let (reason, text) = alert(&mut job, &running(600, 901, 2)).unwrap();
        assert_eq!(reason, Reason::Stalled);
        assert_eq!(
            text,
            "Build 0420b7ab: stage 3b release build has reported no progress for 15.0 min \
             (running 10.0 min)."
        );
        assert!(alert(&mut job, &running(700, 1000, 2)).is_none());
        let between = json!({"job_id":"abc","status":"running","stage":"package",
            "idle_seconds":1000,"running_stages":[]});
        assert_eq!(alert(&mut job, &between).unwrap().0, Reason::Stalled);
        assert!(alert(&mut job, &between).is_none());
    }

    #[test]
    fn the_broker_counts_as_unresponsive_after_three_misses_until_it_answers() {
        let mut job = json!({"job_id":"0420b7ab-6855"});
        assert!(broker_failed(&mut job).is_none());
        assert!(broker_failed(&mut job).is_none());
        assert_eq!(broker_failed(&mut job).unwrap().0, Reason::Unresponsive);
        assert!(broker_failed(&mut job).is_none());
        assert_eq!(
            broker_answered(&mut job).as_deref(),
            Some("Build 0420b7ab: the broker is answering again.")
        );
        assert!(broker_answered(&mut job).is_none());
    }

    #[test]
    fn jobs_group_under_their_task_and_a_new_job_shows_a_cleared_task() {
        let mut record = json!({"tasks":[]});
        let build = JobKind {
            kind: "build",
            check: None,
        };
        job(&mut record, "chip-task", "job-1", "run-1", &build, 10);
        name_task(
            &mut record,
            "chip-task",
            "Broker chip [origin=claude-code; session=x]",
        );
        record["tasks"][0]["cleared"] = json!(true);
        let check = JobKind {
            kind: "check",
            check: Some("test"),
        };
        job(&mut record, "chip-task", "job-2", "run-2", &check, 20);
        let task = &record["tasks"][0];
        assert_eq!(task["name"], "Broker chip");
        assert_eq!(task["cleared"], false);
        assert_eq!(task["jobs"].as_array().unwrap().len(), 2);
        assert_eq!(task["jobs"][1]["check"], "test");
        assert_eq!(task_of(&record, "job-2").as_deref(), Some("chip-task"));
        assert!(named(&record, "chip-task") && !named(&record, "other"));
    }

    #[test]
    fn names_drop_provenance_and_a_leading_chat_id() {
        assert_eq!(
            task_name("chat f15065ed-0aea-443b: live-refresh the chart [origin=webagent]"),
            "live-refresh the chart"
        );
        assert_eq!(
            task_name("chat about: the toggle"),
            "chat about: the toggle"
        );
        assert_eq!(task_name("Fix the chip"), "Fix the chip");
    }

    #[test]
    fn a_begun_task_shows_named_before_its_first_job() {
        let mut record = json!({"tasks":[]});
        note_task(&mut record, "t", Some("chat ab12: Fix it"), 5);
        assert_eq!(record["tasks"][0]["name"], "Fix it");
        assert_eq!(record["tasks"][0]["jobs"], json!([]));
        note_task(&mut record, "t", Some("Other"), 6);
        assert_eq!(record["tasks"].as_array().map(Vec::len), Some(1));
        assert_eq!(record["tasks"][0]["name"], "Fix it");
    }

    #[test]
    fn only_jobs_that_did_not_complete_post_an_ending_line() {
        assert!(ended_line(&json!({"status":"completed","job_id":"abc"})).is_none());
        assert_eq!(
            ended_line(&json!({"status":"failed","job_id":"0420b7ab-6855",
                "reason":"tests failed"}))
            .as_deref(),
            Some("Build 0420b7ab failed: tests failed.")
        );
    }
}
