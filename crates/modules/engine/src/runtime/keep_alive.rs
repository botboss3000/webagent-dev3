//! Experimental keep-alive: shortly before a session's provider cache times out, resend its
//! last request with a tiny question appended, so the provider keeps the cached start.
//! Nothing is saved to history. A bridge answers from a fork of the chat's harness session,
//! so the chat's own session is not changed either.
//!
//! Three records per session live in its owner's database, so pings survive a restart:
//! `keep_alive:{sid}` (timing), `keep_alive_request:{sid}` (the last request) and
//! `keep_alive_visit:{sid}` (written by the app when the user opens the session). Each holds
//! `session_id`, so deleting the session erases them.
use super::{config, models::ModelRecords, ports::*, Runtime};
use crate::context::abilities::Catalog;
use crate::context_compaction::KeepAlive;
use crate::ports::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use webagent_models::provider::ProviderSelection;

/// How long before the cache timeout a ping goes out (at most half the timeout).
const LEAD: Duration = Duration::from_secs(120);
/// How often the service looks for sessions due a ping.
pub const TICK: Duration = Duration::from_secs(30);
/// The appended question; its answer is dropped.
const PING: &str = "Keep-alive check: reply with just ok. Do not call tools.";

pub fn timing_key(session: &str) -> String {
    format!("keep_alive:{session}")
}
pub fn request_key(session: &str) -> String {
    format!("keep_alive_request:{session}")
}
pub fn visit_key(session: &str) -> String {
    format!("keep_alive_visit:{session}")
}

/// One session's last request, kept only while its agent has keep-alive on.
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

/// Save a real model call's request for later pings.
pub async fn record(store: &impl ScopedStorage, session: &str, mut request: Request, now: u64) {
    if let Some(run) = request.token_run.as_object_mut() {
        run.remove("input");
        run.remove("_activity_items");
    }
    let saved = json!({"session_id":session,"selection":request.selection,"input":request.input,
        "tools":request.tools,"instructions":request.instructions,"effort":request.effort,
        "token_run":request.token_run});
    let timing = json!({"session_id":session,"run_id":request.run_id,"agent_id":request.agent_id,
        "timeout_seconds":request.timeout.as_secs(),"last_call":now,"real_call":now,"visit":false});
    let result = async {
        store.put(&request_key(session), &saved).await?;
        store.put(&timing_key(session), &timing).await
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(session_id = %session, error = %error, "keep-alive request not saved");
    }
}

/// What the session's state says about pinging.
#[derive(Debug, PartialEq, Eq)]
pub enum State {
    /// The run is live; a real call will refresh the cache.
    Live,
    /// The user dismissed the session's flag: stop for good.
    Dismissed,
    /// Settled; whether this state's switch is on.
    Settled(bool),
}

pub fn state(settings: &KeepAlive, run: &Value, session: &Value) -> State {
    if session["flag_dismissed"] == true {
        return State::Dismissed;
    }
    State::Settled(match run["status"].as_str().unwrap_or("") {
        "completed" => match session["flag"].as_str() {
            Some("needs_input" | "waiting") => settings.needs_input,
            Some("failed") => settings.error,
            _ => settings.completed,
        },
        "waiting_approval" => settings.needs_input,
        "stopped" | "cancelled" => settings.stopped,
        "failed" | "paused" | "rejected" | "unavailable" => settings.error,
        _ => return State::Live,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Wait,
    Ping,
    Stop,
}

/// When a ping is due after the last call: the lead before the timeout, at most half of it.
fn ping_after(timeout: Duration) -> Duration {
    timeout.saturating_sub(LEAD.min(timeout / 2))
}

/// Decide one session at `now` (Unix seconds). A visit overrides an off state switch and
/// the limit, never a dismissed flag; nothing pings once the cache is already gone.
pub fn decide(timing: &Value, now: u64, limit: Duration, state: &State) -> Action {
    let timeout = Duration::from_secs(timing["timeout_seconds"].as_u64().unwrap_or(0));
    let since =
        |field: &str| Duration::from_secs(now.saturating_sub(timing[field].as_u64().unwrap_or(0)));
    let idle = since("last_call");
    if timeout.is_zero() || idle >= timeout || *state == State::Dismissed {
        return Action::Stop;
    }
    if idle < ping_after(timeout) {
        return Action::Wait;
    }
    let visit = timing["visit"] == true;
    match state {
        State::Settled(on) if visit || (*on && since("real_call") < limit) => Action::Ping,
        // Live runs refresh the cache themselves; an off switch or a passed limit waits until
        // the cache is gone, so a visit can still ping.
        _ => Action::Wait,
    }
}

/// A visit counts while the cache is still warm: it lets the next due ping go out.
pub fn apply_visit(timing: &mut Value, visit_at: u64) {
    let timeout = timing["timeout_seconds"].as_u64().unwrap_or(0);
    let last = timing["last_call"].as_u64().unwrap_or(0);
    if visit_at >= last && visit_at < last.saturating_add(timeout) {
        timing["visit"] = json!(true);
    }
}

/// Whether a ping failed because the bridge cannot fork the chat's harness session.
pub fn unforkable(error: &str) -> bool {
    error.contains("cannot be forked")
}

/// The ping request: the last request with the question appended, so every byte before
/// it matches what the provider cached.
pub fn ping_input(input: &[Value]) -> Vec<Value> {
    let mut input = input.to_vec();
    input.push(json!({"role":"user","content":PING}));
    input
}

/// One line per ping in the server log, beside the `model call cache` lines.
pub fn log_ping(session: &str, run_id: &str, visit: bool, response: &anyhow::Result<Value>) {
    match response {
        Ok(value) => {
            let usage = &value["usage"];
            tracing::info!(
                session_id = %session,
                run_id = %run_id,
                visit,
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
    /// Ping saved sessions shortly before their cache times out, until the loop stops.
    pub fn keep_alive_service(self) -> impl std::future::Future<Output = ()> + Send {
        async move {
            let app = self;
            loop {
                tokio::select! {
                    () = app.stop.cancelled() => break,
                    () = tokio::time::sleep(TICK) => {}
                }
                let Ok(views) = app.user_views(true).await else {
                    continue;
                };
                for view in views {
                    if let Err(error) = view.keep_alive_tick().await {
                        tracing::warn!(error = %error, "keep-alive scan failed");
                    }
                }
            }
        }
    }

    /// One pass over one account's saved sessions.
    async fn keep_alive_tick(&self) -> anyhow::Result<()> {
        let keys: Vec<String> =
            sqlx::query_scalar("SELECT key FROM docs WHERE key >= ? AND key < ?")
                .bind("keep_alive:")
                .bind("keep_alive;")
                .fetch_all(ScopedStorage::user_pool(self.store.records()))
                .await?;
        let mut settings: HashMap<String, Option<KeepAlive>> = HashMap::new();
        let now = crate::now();
        for key in keys {
            let Some(mut timing) = self.store.get(&key).await? else {
                continue;
            };
            if timing["stopped"] == true {
                continue;
            }
            let session = timing["session_id"].as_str().unwrap_or_default().to_owned();
            let agent = timing["agent_id"].as_str().unwrap_or_default().to_owned();
            if !settings.contains_key(&agent) {
                let value =
                    crate::context_compaction::agent_thresholds(self.store.records(), &agent)
                        .await
                        .ok()
                        .flatten()
                        .and_then(|thresholds| thresholds.keep_alive)
                        .filter(|keep| keep.enabled);
                settings.insert(agent.clone(), value);
            }
            let Some(keep) = settings[&agent] else {
                self.keep_alive_stop(&session).await?;
                continue;
            };
            if let Some(visit) = self.store.get(&visit_key(&session)).await? {
                if let Some(at) = visit["at"].as_u64().filter(|at| *at > 0) {
                    apply_visit(&mut timing, at);
                    self.store
                        .put(&visit_key(&session), &json!({"session_id":session,"at":0}))
                        .await?;
                }
            }
            let run_id = timing["run_id"].as_str().unwrap_or_default().to_owned();
            // The newest run, not the one behind the saved request: a new message's run counts
            // as live from admission, before its first model call saves a new request.
            let newest = self.store.newest_run_id(&session).await?;
            let run = self
                .run(newest.as_deref().unwrap_or(&run_id))
                .await
                .unwrap_or(Value::Null);
            let record = self
                .store
                .get(&format!("session:{session}"))
                .await?
                .unwrap_or(Value::Null);
            let limit =
                Duration::from_secs(60 * keep.limit_minutes.min(KeepAlive::MAX_LIMIT_MINUTES));
            match decide(&timing, now, limit, &state(&keep, &run, &record)) {
                Action::Stop => self.keep_alive_stop(&session).await?,
                Action::Wait => {
                    if timing["visit"] == true {
                        self.store.put(&key, &timing).await?;
                    }
                }
                Action::Ping => {
                    let visit = timing["visit"] == true;
                    timing["last_call"] = json!(now);
                    timing["visit"] = json!(false);
                    self.store.put(&key, &timing).await?;
                    let Some(saved) = self.store.get(&request_key(&session)).await? else {
                        continue;
                    };
                    let app = self.clone();
                    tokio::spawn(async move {
                        let response = app.keep_alive_ping(&session, &saved).await;
                        log_ping(&session, &run_id, visit, &response);
                        // A bridge that cannot fork the chat (no harness session yet, or the
                        // chat's request has since changed) would fail every later ping too.
                        if let Err(error) = &response {
                            if unforkable(&error.to_string()) {
                                if let Err(error) = app.keep_alive_stop(&session).await {
                                    tracing::warn!(session_id = %session, error = %error, "keep-alive stop failed");
                                }
                            }
                        }
                    });
                }
            }
        }
        Ok(())
    }

    /// Stop pinging a session and drop its saved conversation copy.
    async fn keep_alive_stop(&self, session: &str) -> anyhow::Result<()> {
        self.store
            .put(
                &timing_key(session),
                &json!({"session_id":session,"stopped":true}),
            )
            .await?;
        self.store
            .put(&request_key(session), &json!({"session_id":session}))
            .await
    }

    /// Send one ping; a bridge answers from a fork of the chat's session.
    async fn keep_alive_ping(&self, session: &str, saved: &Value) -> anyhow::Result<Value> {
        let selection: ProviderSelection = serde_json::from_value(saved["selection"].clone())?;
        let input: Vec<Value> = serde_json::from_value(saved["input"].clone())?;
        let tools: Vec<Value> = serde_json::from_value(saved["tools"].clone())?;
        let instructions = saved["instructions"].as_str().unwrap_or_default();
        let cancel = self.stop.child_token();
        let prepared = crate::provider::CallDraft::new(
            &selection,
            ping_input(&input),
            tools,
            session,
            instructions,
        )
        .with_reasoning_effort(saved["effort"].as_str())
        .with_bridge_fork(selection.provider.is_bridge())
        .prepare(
            crate::ports::AgentMetadata::root(self.store.records()),
            &cancel,
        )
        .await?;
        let tokens = self.tokens_for(&selection, &saved["token_run"]).await?;
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
        let of = |status: &str| state(&s, &json!({"status":status}), &open);
        assert_eq!(of("completed"), State::Settled(true));
        assert_eq!(of("waiting_approval"), State::Settled(true));
        assert_eq!(of("stopped"), State::Settled(false));
        assert_eq!(of("failed"), State::Settled(false));
        assert_eq!(of("running"), State::Live);
        assert_eq!(of("queued"), State::Live, "a new message waits for its own call");
        let flagged = |flag: &str| state(&s, &json!({"status":"completed"}), &json!({"flag":flag}));
        assert_eq!(flagged("needs_input"), State::Settled(true));
        assert_eq!(
            flagged("failed"),
            State::Settled(false),
            "an agent's failed flag is an error"
        );
        let dismissed = json!({"flag":"needs_input","flag_dismissed":true});
        assert_eq!(
            state(&s, &json!({"status":"completed"}), &dismissed),
            State::Dismissed
        );
    }

    #[test]
    fn pings_go_out_before_the_timeout_and_stop_once_the_cache_is_gone() {
        assert_eq!(
            ping_after(Duration::from_secs(3600)),
            Duration::from_secs(3480)
        );
        assert_eq!(ping_after(Duration::from_secs(60)), Duration::from_secs(30));
        let hour = Duration::from_secs(3600);
        let timing =
            json!({"timeout_seconds":3600,"last_call":1000,"real_call":1000,"visit":false});
        let on = State::Settled(true);
        assert_eq!(decide(&timing, 1000 + 3000, hour * 4, &on), Action::Wait);
        assert_eq!(decide(&timing, 1000 + 3500, hour * 4, &on), Action::Ping);
        assert_eq!(
            decide(&timing, 1000 + 3600, hour * 4, &on),
            Action::Stop,
            "cache gone"
        );
        assert_eq!(
            decide(&timing, 1000 + 3500, hour * 4, &State::Live),
            Action::Wait
        );
        assert_eq!(
            decide(&timing, 1000 + 3500, hour * 4, &State::Dismissed),
            Action::Stop
        );
        let old = json!({"timeout_seconds":3600,"last_call":1000,"real_call":0,"visit":false});
        assert_eq!(
            decide(&old, 1000 + 3500, Duration::from_secs(60), &on),
            Action::Wait,
            "past the limit"
        );
        assert_eq!(
            decide(&timing, 1000 + 3500, hour, &State::Settled(false)),
            Action::Wait,
            "switch off"
        );
    }

    #[test]
    fn a_visit_while_warm_allows_one_ping_past_switches_and_limit() {
        let mut timing =
            json!({"timeout_seconds":3600,"last_call":1000,"real_call":0,"visit":false});
        apply_visit(&mut timing, 1000 + 3600);
        assert_eq!(
            timing["visit"], false,
            "after the cache timeout a visit is too late"
        );
        apply_visit(&mut timing, 1000 + 300);
        assert_eq!(timing["visit"], true);
        let limit = Duration::from_secs(60);
        assert_eq!(
            decide(&timing, 1000 + 3500, limit, &State::Settled(false)),
            Action::Ping
        );
        assert_eq!(
            decide(&timing, 1000 + 3500, limit, &State::Dismissed),
            Action::Stop
        );
        assert!(unforkable(
            "Bridge client failed: the chat's session cannot be forked: no harness session yet"
        ));
        assert!(!unforkable("Bridge client failed: upstream timeout"));
        let input = ping_input(&[json!({"role":"user","content":"hi"})]);
        assert_eq!(input.len(), 2);
        assert_eq!(input[1]["content"], PING);
    }
}
