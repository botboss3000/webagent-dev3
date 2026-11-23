//! Engine steering boundary, send-now detection and stable successor assembly.
//! Chat retains queue persistence, consumption transactions and delegation revalidation.
use super::{config, models, native, ports::*, Runtime};
use crate::digest;
use crate::ports::{AdmissionEffects, ChatEffects, NotificationEffects, UsageEffects};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::future::Future;

/// Existing Chat queue operations always receive the current trusted storage scope.
pub trait FollowupQueue<S: Sync>: Clone + Send + Sync + 'static {
    fn followup_queue<'a>(
        &'a self,
        store: &'a S,
        sid: &'a str,
    ) -> impl Future<Output = Result<Value>> + Send + 'a;
    /// Plain numbers that change whenever the queue changes (`None`: not known, read it).
    fn followup_revision<'a>(
        &'a self,
        _store: &'a S,
        _sid: &'a str,
    ) -> impl Future<Output = Result<Option<[Option<i64>; 2]>>> + Send + 'a {
        async { Ok(None) }
    }
    fn followup_consume<'a>(
        &'a self,
        store: &'a S,
        sid: &'a str,
        choice: &'a str,
        run: &'a mut Value,
    ) -> impl Future<Output = Result<bool>> + Send + 'a;
}

/// The last queue each session's poll decrypted, with the revision it had. A running run
/// asks four times a second whether a message waits; the queue is decrypted again only
/// when its revision moved, so every answer still reflects the stored queue.
type SeenQueues = std::collections::HashMap<String, ([Option<i64>; 2], Value)>;
static SEEN_QUEUES: std::sync::LazyLock<std::sync::Mutex<SeenQueues>> =
    std::sync::LazyLock::new(Default::default);

pub fn successor_id(owner: &str, sid: &str, entry: &Value) -> Result<String> {
    let id = entry["id"]
        .as_str()
        .ok_or_else(|| anyhow!("Followup entry id missing"))?;
    if let Ok(id) = ::uuid::Uuid::parse_str(id) {
        return Ok(id.to_string());
    }
    let hash = digest(&json!([
        "webagent-private-followup-successor-v1",
        owner,
        sid,
        id
    ]));
    // A stable UUID preserves retries and legacy route expectations.
    Ok(::uuid::Uuid::parse_str(&hash[..32])?.to_string())
}

impl<
        S: ScopedStorage
            + ChatEffects
            + AdmissionEffects
            + NotificationEffects
            + UsageEffects
            + RunObservation
            + RunPresentation
            + models::ModelRecords,
        C: config::SchemaCatalog,
        B: Sync,
        A,
        H: ChatAuthority<S>,
        M: ModuleBindings<S>,
        X: AgentPolicy<S> + FollowupQueue<S> + native::NativeDispatch<S, C, B, H::Binding>,
    > Runtime<S, C, B, A, H, M, X>
{
    /// Whether a user message is waiting to enter this run at its next model step.
    /// Running tool calls are detached and undispatched calls skipped when it is.
    pub async fn message_pending(&self, run: &Value) -> Result<bool> {
        Ok(self.pending_steer(run, false).await?.is_some())
    }
    /// Whether anything waits to enter the conversation, background notices included.
    /// A run waiting for approval parks on it so the conversation can carry on.
    pub async fn steer_waiting(&self, run: &Value) -> Result<bool> {
        Ok(self.pending_steer(run, true).await?.is_some())
    }
    /// Whether the user chose "send now" for a waiting message: the current model
    /// call is replaced and a running tool call detaches without grace.
    pub async fn message_forced(&self, run: &Value) -> Result<bool> {
        Ok(self.pending_steer(run, false).await? == Some(true))
    }
    /// `Some(forced)` when an acceptable steer waits; forced if any entry is sent now.
    async fn pending_steer(&self, run: &Value, notices: bool) -> Result<Option<bool>> {
        // A run parked on an approval leaves new messages to the conversation's next run.
        if run["parked"] == true {
            return Ok(None);
        }
        if run["requester_user_id"]
            .as_str()
            .is_some_and(|actor| actor != self.store.current_user_id())
        {
            return Ok(None);
        }
        let sid = run["session_id"].as_str().unwrap_or("");
        let revision = self
            .native
            .followup_revision(self.store.records(), sid)
            .await?;
        let seen_key = format!("{}\0{sid}", self.store.current_user_id());
        let seen = revision.and_then(|revision| {
            let seen = SEEN_QUEUES
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            seen.get(&seen_key)
                .filter(|(at, _)| *at == revision)
                .map(|(_, queue)| queue.clone())
        });
        let q = match seen {
            Some(queue) => queue,
            None => {
                let queue = self
                    .native
                    .followup_queue(self.store.records(), sid)
                    .await?;
                // Read after the revision: a write in between leaves an older revision
                // beside newer contents, so the next poll reads again.
                if let Some(revision) = revision {
                    let mut seen = SEEN_QUEUES
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if seen.len() >= 1024 {
                        seen.clear();
                    }
                    seen.insert(seen_key, (revision, queue.clone()));
                }
                queue
            }
        };
        if q["held"] == true {
            return Ok(None);
        }
        let waiting: Vec<&Value> = q["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| {
                // Background notices join at the next step without cutting off work.
                e["choice"] == "steer"
                    && e["status"] == "pending"
                    && e["review_status"] == "acceptable"
                    && (notices || e["platform_notice"].is_null())
            })
            .collect();
        Ok((!waiting.is_empty()).then(|| waiting.iter().any(|e| e["send_now"] == true)))
    }
    pub async fn apply_steering(&self, run: &mut Value) -> Result<bool> {
        if run["parked"] == true {
            return Ok(false);
        }
        if run["requester_user_id"]
            .as_str()
            .is_some_and(|actor| actor != self.store.current_user_id())
        {
            return Ok(false);
        }
        let s = run["session_id"].as_str().unwrap().to_owned();
        let joined = self
            .native
            .followup_consume(self.store.records(), &s, "steer", run)
            .await?;
        if joined {}
        Ok(joined)
    }
}
