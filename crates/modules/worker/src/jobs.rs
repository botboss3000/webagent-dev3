//! The worker's table of acknowledged calls (stream D, step W1). A caller that sends
//! `"ack": true` gets one acknowledgement line the moment its call is registered here and
//! before anything runs; when that line cannot be written the call is marked not started
//! and nothing runs, so "no acknowledgement seen" means "no effect". The result of each
//! acknowledged call is kept for a while: a caller that lost its answer asks for it again
//! with `"resume": true`, and the same job id never runs twice in one worker process.
//!
//! Memory only, never on disk. Only results are kept: never the request, its arguments or
//! its `trusted` part (vault values, cookies, Git tokens).
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// How long finished calls are kept and how much of them. A caller sends the live values
/// from `config/worker/transport.json` in the request's `job_table`; these defaults apply
/// until one does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// How long a finished call's result is kept.
    pub ttl: Duration,
    /// Most calls kept; running calls are never dropped.
    pub max_jobs: usize,
    /// Larger results are not kept (a resume then finds nothing).
    pub max_result_bytes: usize,
    /// Most result bytes kept in all.
    pub max_total_bytes: usize,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(600),
            max_jobs: 1024,
            max_result_bytes: 1024 * 1024,
            max_total_bytes: 64 * 1024 * 1024,
        }
    }
}

impl Bounds {
    /// `self` with each field `value` sets, clamped to its safe range (the descriptor
    /// bounds in `config/worker/transport.settings.json`). Missing or invalid fields keep
    /// their current value.
    #[must_use]
    pub fn with(self, value: &Value) -> Self {
        let field = |key: &str, min: u64, max: u64| value[key].as_u64().map(|n| n.clamp(min, max));
        let size = |n: u64| usize::try_from(n).unwrap_or(usize::MAX);
        Self {
            ttl: field("ttl_seconds", 10, 3600).map_or(self.ttl, Duration::from_secs),
            max_jobs: field("max_jobs", 16, 65_536).map_or(self.max_jobs, size),
            max_result_bytes: field("max_result_bytes", 1024, 2 * 1024 * 1024)
                .map_or(self.max_result_bytes, size),
            max_total_bytes: field("max_total_bytes", 1024 * 1024, 512 * 1024 * 1024)
                .map_or(self.max_total_bytes, size),
        }
    }
}

enum State {
    /// The acknowledgement could not be written: nothing ran. A retry with the same id
    /// may run it.
    NotStarted(Instant),
    /// Acknowledged and running; waiting resumes subscribe and wake when it is dropped.
    Running(watch::Sender<()>),
    /// Finished at the instant given; `None` when the result was too large to keep.
    Finished(Option<Value>, usize, Instant),
}

struct Job {
    workspace: String,
    name: String,
    state: State,
}

struct Table {
    jobs: HashMap<String, Job>,
    bytes: usize,
    bounds: Bounds,
}

/// The calls one worker process acknowledged, by job id.
pub struct Jobs {
    instance: String,
    table: Mutex<Table>,
}

/// What a request with an id the worker may already know gets.
pub enum Admission<'a> {
    /// A new call (or one whose acknowledgement failed before): acknowledge, then run.
    Run(Entry<'a>),
    /// A resume, or a repeat of a known id: never run; answer with the earlier outcome.
    Resume(Resume),
}

/// The earlier outcome a resume answers with.
pub enum Resume {
    Done(Value),
    /// Still running: wait for it.
    Wait(watch::Receiver<()>),
    Unknown,
}

/// The result a resume gets when the worker keeps nothing for the id, name and workspace.
#[must_use]
pub fn unknown() -> Value {
    json!({"status":"failed","error_kind":"unknown_job",
        "output":"The tool worker keeps no result for this call"})
}

/// What a call that ended without a result (its caller hung up, its task panicked) is
/// recorded as: its command or browser was stopped part-way, so its effect is unknown.
fn stopped() -> Value {
    json!({"status":"failed","uncertain":true,
        "output":"Stopped when its caller disconnected"})
}

impl Default for Jobs {
    fn default() -> Self {
        Self {
            instance: uuid::Uuid::new_v4().to_string(),
            table: Mutex::new(Table {
                jobs: HashMap::new(),
                bytes: 0,
                bounds: Bounds::default(),
            }),
        }
    }
}

impl Jobs {
    /// This worker process's id, sent in every acknowledgement so a caller sees a restart.
    #[must_use]
    pub fn instance(&self) -> &str {
        &self.instance
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Applies the live bounds a request carries (`job_table`), when it carries any.
    pub fn limit(&self, value: &Value) {
        if value.is_object() {
            let mut table = self.lock();
            table.bounds = table.bounds.with(value);
        }
    }

    /// Registers call `id` (or finds it). A resume, a known id or a different name or
    /// workspace for a known id never runs anything.
    pub fn admit(&self, id: &str, workspace: &str, name: &str, resume: bool) -> Admission<'_> {
        let mut guard = self.lock();
        let table = &mut *guard;
        let Some(job) = table.jobs.get_mut(id) else {
            if resume {
                return Admission::Resume(Resume::Unknown);
            }
            let (running, _) = watch::channel(());
            table.jobs.insert(
                id.to_owned(),
                Job {
                    workspace: workspace.to_owned(),
                    name: name.to_owned(),
                    state: State::Running(running),
                },
            );
            return Admission::Run(Entry {
                jobs: self,
                id: id.to_owned(),
                open: true,
            });
        };
        if job.workspace != workspace || job.name != name {
            return Admission::Resume(Resume::Unknown);
        }
        if !resume && matches!(job.state, State::NotStarted(_)) {
            let (running, _) = watch::channel(());
            job.state = State::Running(running);
            return Admission::Run(Entry {
                jobs: self,
                id: id.to_owned(),
                open: true,
            });
        }
        Admission::Resume(match &job.state {
            State::Running(running) => Resume::Wait(running.subscribe()),
            State::Finished(Some(result), ..) => Resume::Done(result.clone()),
            State::NotStarted(_) | State::Finished(None, ..) => Resume::Unknown,
        })
    }

    /// The outcome of a resume: the stored result, after waiting for a running call.
    pub async fn outcome(&self, id: &str, resume: Resume) -> Value {
        match resume {
            Resume::Done(result) => result,
            Resume::Unknown => unknown(),
            Resume::Wait(mut finished) => {
                // Ends when the running state (and its sender) is replaced.
                let _ = finished.changed().await;
                match self.lock().jobs.get(id).map(|job| &job.state) {
                    Some(State::Finished(Some(result), ..)) => result.clone(),
                    _ => unknown(),
                }
            }
        }
    }

    fn settle(&self, id: &str, state: State) {
        let mut guard = self.lock();
        let table = &mut *guard;
        let added = match &state {
            State::Finished(_, bytes, _) => *bytes,
            _ => 0,
        };
        if let Some(job) = table.jobs.get_mut(id) {
            job.state = state;
            table.bytes += added;
        }
        evict(table, Instant::now());
    }

    #[cfg(test)]
    fn not_started(&self, id: &str) -> bool {
        matches!(
            self.lock().jobs.get(id).map(|job| &job.state),
            Some(State::NotStarted(_))
        )
    }
}

/// Drops finished and not-started calls past their time, then the oldest of them while the
/// table is over its count or byte bound. Running calls always stay.
fn evict(table: &mut Table, now: Instant) {
    let ttl = table.bounds.ttl;
    let mut freed = 0;
    table.jobs.retain(|_, job| match job.state {
        State::Finished(_, bytes, at) if now.duration_since(at) >= ttl => {
            freed += bytes;
            false
        }
        State::NotStarted(at) if now.duration_since(at) >= ttl => false,
        _ => true,
    });
    table.bytes -= freed;
    while table.jobs.len() > table.bounds.max_jobs || table.bytes > table.bounds.max_total_bytes {
        let oldest = table
            .jobs
            .iter()
            .filter_map(|(id, job)| match job.state {
                State::Finished(_, _, at) | State::NotStarted(at) => Some((at, id)),
                State::Running(_) => None,
            })
            .min()
            .map(|(_, id)| id.clone());
        let Some(oldest) = oldest else { break };
        if let Some(Job {
            state: State::Finished(_, bytes, _),
            ..
        }) = table.jobs.remove(&oldest)
        {
            table.bytes -= bytes;
        }
    }
}

/// One admitted call. Its outcome is stored when it finishes; dropped unfinished (its
/// caller hung up, its task panicked) it is stored as stopped, effect unknown.
pub struct Entry<'a> {
    jobs: &'a Jobs,
    id: String,
    open: bool,
}

impl Entry<'_> {
    /// The acknowledgement could not be written: nothing runs.
    pub fn not_started(mut self) {
        self.open = false;
        self.jobs
            .settle(&self.id, State::NotStarted(Instant::now()));
    }

    /// Stores the call's result before it is written; `bytes` is its reply line's size.
    pub fn finish(mut self, result: &Value, bytes: usize) {
        self.open = false;
        let kept = (bytes <= self.jobs.lock().bounds.max_result_bytes).then(|| result.clone());
        let bytes = if kept.is_some() { bytes } else { 0 };
        self.jobs
            .settle(&self.id, State::Finished(kept, bytes, Instant::now()));
    }
}

impl Drop for Entry<'_> {
    fn drop(&mut self) {
        if self.open {
            let result = stopped();
            let bytes = result.to_string().len();
            self.jobs.settle(
                &self.id,
                State::Finished(Some(result), bytes, Instant::now()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WS: &str = "2244991a-c2e3-415b-ae5c-054570c91d32";

    fn run<'a>(jobs: &'a Jobs, id: &str) -> Entry<'a> {
        match jobs.admit(id, WS, "file_write", false) {
            Admission::Run(entry) => entry,
            Admission::Resume(_) => panic!("{id} should run"),
        }
    }

    async fn resumed(jobs: &Jobs, id: &str, name: &str, workspace: &str) -> Value {
        match jobs.admit(id, workspace, name, true) {
            Admission::Resume(resume) => jobs.outcome(id, resume).await,
            Admission::Run(_) => panic!("a resume never runs"),
        }
    }

    #[tokio::test]
    async fn a_known_id_never_runs_again_and_a_resume_gets_the_stored_result() {
        let jobs = Jobs::default();
        let result = json!({"status":"success","output":"written"});
        run(&jobs, "a").finish(&result, 40);
        // The same id again, with or without resume, answers the stored result.
        for resume in [false, true] {
            match jobs.admit("a", WS, "file_write", resume) {
                Admission::Resume(r) => assert_eq!(jobs.outcome("a", r).await, result),
                Admission::Run(_) => panic!("ran twice"),
            }
        }
        // Unknown id, other name, other workspace: unknown_job, nothing runs.
        assert_eq!(resumed(&jobs, "b", "file_write", WS).await, unknown());
        assert_eq!(resumed(&jobs, "a", "file_read", WS).await, unknown());
        let other = "3355aa2b-d3f4-426c-bf6d-165d81da2e43";
        assert_eq!(resumed(&jobs, "a", "file_write", other).await, unknown());
        assert!(matches!(
            jobs.admit("a", other, "file_write", false),
            Admission::Resume(Resume::Unknown)
        ));
    }

    #[tokio::test]
    async fn a_repeat_while_running_waits_for_the_first_result() {
        let jobs = std::sync::Arc::new(Jobs::default());
        let entry = run(&jobs, "a");
        let waiting = match jobs.admit("a", WS, "file_write", false) {
            Admission::Resume(resume @ Resume::Wait(_)) => resume,
            _ => panic!("a repeat of a running call waits"),
        };
        let waiter = {
            let jobs = std::sync::Arc::clone(&jobs);
            tokio::spawn(async move { jobs.outcome("a", waiting).await })
        };
        let result = json!({"status":"success","output":"once"});
        entry.finish(&result, 30);
        assert_eq!(waiter.await.expect("waiter"), result);
    }

    #[test]
    fn a_failed_acknowledgement_runs_nothing_and_a_retry_may_run_it() {
        let jobs = Jobs::default();
        run(&jobs, "a").not_started();
        assert!(jobs.not_started("a"));
        // A resume finds nothing to report; a retry with the same id runs it.
        assert!(matches!(
            jobs.admit("a", WS, "file_write", true),
            Admission::Resume(Resume::Unknown)
        ));
        run(&jobs, "a").finish(&json!({"status":"success"}), 20);
        assert!(!jobs.not_started("a"));
    }

    #[tokio::test]
    async fn a_call_dropped_unfinished_is_stored_as_stopped_and_uncertain() {
        let jobs = Jobs::default();
        drop(run(&jobs, "a"));
        let stored = resumed(&jobs, "a", "file_write", WS).await;
        assert_eq!(stored["uncertain"], true);
        assert_eq!(stored["output"], "Stopped when its caller disconnected");
    }

    #[tokio::test]
    async fn retention_drops_only_old_or_excess_finished_calls() {
        let jobs = Jobs::default();
        jobs.limit(&json!({"max_jobs":16,"max_result_bytes":1024,"max_total_bytes":0}));
        // Clamped: the total byte bound is at least 1 MiB.
        assert_eq!(jobs.lock().bounds.max_total_bytes, 1024 * 1024);
        let running = run(&jobs, "running");
        for n in 0..20 {
            run(&jobs, &format!("done-{n}")).finish(&json!({"n":n}), 10);
        }
        {
            let table = jobs.lock();
            assert_eq!(table.jobs.len(), 16);
            assert!(table.jobs.contains_key("running"));
            assert!(!table.jobs.contains_key("done-0"), "the oldest goes first");
            assert!(table.jobs.contains_key("done-19"));
            assert_eq!(table.bytes, 15 * 10);
        }
        // Too large to keep: a resume finds nothing, and no bytes are counted.
        run(&jobs, "large").finish(&json!({"big":true}), 4096);
        assert_eq!(resumed(&jobs, "large", "file_write", WS).await, unknown());
        // Past the time bound every finished call goes; the running one stays.
        let later = Instant::now() + Duration::from_secs(601);
        evict(&mut jobs.lock(), later);
        let table = jobs.lock();
        assert_eq!(table.jobs.len(), 1);
        assert_eq!(table.bytes, 0);
        drop(table);
        drop(running);
    }

    #[test]
    fn bounds_clamp_each_field_and_keep_missing_ones() {
        let base = Bounds::default();
        assert_eq!(base.with(&json!({})), base);
        let set = base.with(&json!({"ttl_seconds":1,"max_jobs":1_000_000,"max_result_bytes":"x"}));
        assert_eq!(set.ttl, Duration::from_secs(10));
        assert_eq!(set.max_jobs, 65_536);
        assert_eq!(set.max_result_bytes, base.max_result_bytes);
    }
}
