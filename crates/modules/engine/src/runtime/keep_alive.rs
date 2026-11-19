//! Experimental keep-alive: shortly before a session's provider cache times out, resend its
//! last request with a tiny question appended, so the provider keeps the cached start.
//! Nothing is saved to history. A bridge answers from a fork of the chat's harness session,
//! so the chat's own session is not changed either.
use super::{config, models::ModelRecords, ports::*, Runtime};
use crate::context::abilities::Catalog;
use crate::context_compaction::KeepAlive;
use crate::ports::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use webagent_models::provider::ProviderSelection;

/// Sessions whose last request is kept for pinging; past this the oldest is dropped.
const MAX_SESSIONS: usize = 32;
/// How long before the cache timeout a ping goes out (at most half the timeout).
const LEAD: Duration = Duration::from_secs(120);
/// How often the service looks for sessions due a ping.
pub const TICK: Duration = Duration::from_secs(30);
/// The appended question; its answer is dropped.
const PING: &str = "Keep-alive check: reply with just ok. Do not call tools.";

/// One session's last request, kept only while its agent has keep-alive on.
#[derive(Clone)]
pub struct Request {
    pub run_id: String,
    pub agent_id: String,
    pub selection: ProviderSelection,
    pub input: Vec<Value>,
    pub tools: Vec<Value>,
    pub instructions: String,
    pub effort: Option<String>,
    /// The run without its history, for credential lookup.
    pub token_run: Value,
    pub timeout: Duration,
}

struct Entry {
    request: Request,
    /// Last call that reached the provider: a real one or a ping.
    last_call: Instant,
    /// Last real model call; pings stop `limit_minutes` after it.
    real_call: Instant,
}

fn entries() -> &'static Mutex<HashMap<String, Entry>> {
    static ENTRIES: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    ENTRIES.get_or_init(Default::default)
}

fn lock() -> std::sync::MutexGuard<'static, HashMap<String, Entry>> {
    entries()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Remember a real model call's request for later pings.
pub fn record(session: &str, mut request: Request) {
    if let Some(run) = request.token_run.as_object_mut() {
        run.remove("input");
        run.remove("_activity_items");
    }
    let now = Instant::now();
    let mut guard = lock();
    if !guard.contains_key(session) && guard.len() >= MAX_SESSIONS {
        if let Some(oldest) = guard
            .iter()
            .min_by_key(|(_, entry)| entry.real_call)
            .map(|(key, _)| key.clone())
        {
            guard.remove(&oldest);
        }
    }
    guard.insert(
        session.to_owned(),
        Entry {
            request,
            last_call: now,
            real_call: now,
        },
    );
}

/// Stop pinging a session.
pub fn forget(session: &str) {
    lock().remove(session);
}

/// When a ping is due after the last call: the lead before the timeout, at most half of it.
fn ping_after(timeout: Duration) -> Duration {
    timeout.saturating_sub(LEAD.min(timeout / 2))
}

/// Sessions due a ping at `now`, given each agent's limit. Sessions past their limit are
/// dropped. `limit` returns None when the agent has keep-alive off, which drops it too.
pub fn due(now: Instant, limit: impl Fn(&str) -> Option<Duration>) -> Vec<(String, Request)> {
    let mut guard = lock();
    let mut due = Vec::new();
    guard.retain(|session, entry| {
        let Some(limit) = limit(&entry.request.agent_id) else {
            return false;
        };
        if now.saturating_duration_since(entry.real_call) >= limit {
            return false;
        }
        if now.saturating_duration_since(entry.last_call) >= ping_after(entry.request.timeout) {
            due.push((session.clone(), entry.request.clone()));
        }
        true
    });
    due
}

/// Note a ping that reached the provider, so the next waits a full interval again.
pub fn pinged(session: &str, run_id: &str) {
    if let Some(entry) = lock().get_mut(session) {
        if entry.request.run_id == run_id {
            entry.last_call = Instant::now();
        }
    }
}

/// Agents with keep-alive on whose session state allows a ping: Some(true) ping,
/// Some(false) stop for good, None skip this time (the run is live).
pub fn state_allows(settings: &KeepAlive, run: &Value, session: &Value) -> Option<bool> {
    if session["flag_dismissed"] == true {
        return Some(false);
    }
    Some(match run["status"].as_str().unwrap_or("") {
        "completed" => match session["flag"].as_str() {
            Some("needs_input" | "waiting") => settings.needs_input,
            _ => settings.completed,
        },
        "waiting_approval" => settings.needs_input,
        "stopped" | "cancelled" => settings.stopped,
        "failed" | "paused" | "rejected" | "unavailable" => settings.error,
        _ => return None,
    })
}

/// The ping request: the last request with the question appended, so every byte before
/// it matches what the provider cached.
pub fn ping_input(input: &[Value]) -> Vec<Value> {
    let mut input = input.to_vec();
    input.push(json!({"role":"user","content":PING}));
    input
}

/// One line per ping in the server log, beside the `model call cache` lines.
pub fn log_ping(session: &str, run_id: &str, response: &anyhow::Result<Value>) {
    match response {
        Ok(value) => {
            let usage = &value["usage"];
            tracing::info!(
                session_id = %session,
                run_id = %run_id,
                input_tokens = usage["input_tokens"]
                    .as_u64()
                    .or_else(|| usage["prompt_tokens"].as_u64())
                    .unwrap_or(0),
                cached_tokens = super::background_compaction::cached_tokens(value).unwrap_or(0),
                "keep-alive ping"
            );
        }
        Err(error) => {
            tracing::info!(session_id = %session, run_id = %run_id, error = %error, "keep-alive ping failed");
        }
    }
}

/// Agents of the sessions kept for pinging.
fn agents() -> Vec<String> {
    let mut agents: Vec<String> = lock()
        .values()
        .map(|entry| entry.request.agent_id.clone())
        .collect();
    agents.sort();
    agents.dedup();
    agents
}

impl<
        S: ScopedStorage
            + ModelRecords
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
            + super::native::NativeDispatch<S, C, B, H::Binding>,
    > Runtime<S, C, B, A, H, M, X>
{
    /// Ping kept sessions shortly before their cache times out, until the loop stops.
    pub fn keep_alive_service(self) -> impl std::future::Future<Output = ()> + Send {
        async move {
            let app = self;
            loop {
                tokio::select! {
                    () = app.stop.cancelled() => break,
                    () = tokio::time::sleep(TICK) => {}
                }
                let mut settings = HashMap::new();
                for agent in agents() {
                    let value =
                        crate::context_compaction::agent_thresholds(app.store.records(), &agent)
                            .await
                            .ok()
                            .flatten()
                            .and_then(|thresholds| thresholds.keep_alive)
                            .filter(|keep| keep.enabled);
                    settings.insert(agent, value);
                }
                let limit = |agent: &str| {
                    settings.get(agent).copied().flatten().map(|keep| {
                        Duration::from_secs(
                            60 * keep.limit_minutes.min(KeepAlive::MAX_LIMIT_MINUTES),
                        )
                    })
                };
                for (session, request) in due(Instant::now(), limit) {
                    let Some(keep) = settings.get(&request.agent_id).copied().flatten() else {
                        continue;
                    };
                    let run = app.run(&request.run_id).await.unwrap_or(Value::Null);
                    let record = app
                        .store
                        .get(&format!("session:{session}"))
                        .await
                        .ok()
                        .flatten()
                        .unwrap_or(Value::Null);
                    match state_allows(&keep, &run, &record) {
                        None => continue,
                        Some(false) => {
                            forget(&session);
                            continue;
                        }
                        Some(true) => {}
                    }
                    pinged(&session, &request.run_id);
                    let app = app.clone();
                    tokio::spawn(async move {
                        let response = app.keep_alive_ping(&session, &request).await;
                        log_ping(&session, &request.run_id, &response);
                    });
                }
            }
        }
    }

    /// Send one ping; a bridge answers from a fork of the chat's session.
    async fn keep_alive_ping(&self, session: &str, request: &Request) -> anyhow::Result<Value> {
        let cancel = self.stop.child_token();
        let prepared = crate::provider::CallDraft::new(
            &request.selection,
            ping_input(&request.input),
            request.tools.clone(),
            session,
            &request.instructions,
        )
        .with_reasoning_effort(request.effort.as_deref())
        .with_bridge_fork(request.selection.provider.is_bridge())
        .prepare(
            crate::ports::AgentMetadata::root(self.store.records()),
            &cancel,
        )
        .await?;
        let tokens = self
            .tokens_for(&request.selection, &request.token_run)
            .await?;
        let ready = prepared.bind_credentials(&tokens, &cancel)?;
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<String>(32);
        let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
        let response = self.provider.dispatch(ready, sender, cancel).await;
        let _ = drain.await;
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> KeepAlive {
        serde_json::from_value(json!({"enabled":true})).unwrap()
    }

    #[test]
    fn defaults_ping_completed_and_waiting_sessions_only() {
        let s = settings();
        assert_eq!(s.limit_minutes, 240);
        let open = json!({});
        let state = |status: &str| state_allows(&s, &json!({"status":status}), &open);
        assert_eq!(state("completed"), Some(true));
        assert_eq!(state("waiting_approval"), Some(true));
        assert_eq!(state("stopped"), Some(false));
        assert_eq!(state("failed"), Some(false));
        assert_eq!(state("running"), None);
        let dismissed = json!({"flag":"needs_input","flag_dismissed":true});
        assert_eq!(
            state_allows(&s, &json!({"status":"completed"}), &dismissed),
            Some(false)
        );
    }

    #[test]
    fn pings_go_out_before_the_timeout_and_stop_at_the_limit() {
        assert_eq!(
            ping_after(Duration::from_secs(3600)),
            Duration::from_secs(3480)
        );
        assert_eq!(ping_after(Duration::from_secs(60)), Duration::from_secs(30));
        let session = format!("keep-alive-test-{}", uuid::Uuid::new_v4());
        let agent = format!("agent-{session}");
        record(
            &session,
            Request {
                run_id: "r".into(),
                agent_id: agent.clone(),
                selection: ProviderSelection::parse(&json!({}), "m").unwrap(),
                input: vec![json!({"role":"user","content":"hi"})],
                tools: vec![],
                instructions: String::new(),
                effort: None,
                token_run: json!({"id":"r","input":[1]}),
                timeout: Duration::from_secs(60),
            },
        );
        let mine = |found: Vec<(String, Request)>| found.into_iter().any(|(s, _)| s == session);
        let limit = |id: &str| (id == agent).then_some(Duration::from_secs(3600));
        let now = Instant::now();
        assert!(!mine(due(now, limit)));
        assert!(mine(due(now + Duration::from_secs(31), limit)));
        pinged(&session, "r");
        assert!(!mine(due(Instant::now(), limit)));
        assert!(!mine(due(now + Duration::from_secs(3601), limit)));
        assert!(
            lock().get(&session).is_none(),
            "past the limit it is dropped"
        );
        let input = ping_input(&[json!({"role":"user","content":"hi"})]);
        assert_eq!(input.len(), 2);
        assert_eq!(input[1]["content"], PING);
    }
}
