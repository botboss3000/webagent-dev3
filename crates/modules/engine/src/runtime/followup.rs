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
    fn followup_consume<'a>(
        &'a self,
        store: &'a S,
        sid: &'a str,
        choice: &'a str,
        run: &'a mut Value,
    ) -> impl Future<Output = Result<bool>> + Send + 'a;
}

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
        Ok(self.pending_steer(run).await?.is_some())
    }
    /// Whether the user chose "send now" for a waiting message: the current model
    /// call is replaced and a running tool call detaches without grace.
    pub async fn message_forced(&self, run: &Value) -> Result<bool> {
        Ok(self.pending_steer(run).await? == Some(true))
    }
    /// `Some(forced)` when an acceptable steer waits; forced if any entry is sent now.
    async fn pending_steer(&self, run: &Value) -> Result<Option<bool>> {
        if run["requester_user_id"]
            .as_str()
            .is_some_and(|actor| actor != self.store.current_user_id())
        {
            return Ok(None);
        }
        let q = self
            .native
            .followup_queue(
                self.store.records(),
                run["session_id"].as_str().unwrap_or(""),
            )
            .await?;
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
                    && e["platform_notice"].is_null()
            })
            .collect();
        Ok((!waiting.is_empty()).then(|| waiting.iter().any(|e| e["send_now"] == true)))
    }
    pub async fn apply_steering(&self, run: &mut Value) -> Result<bool> {
        if run["requester_user_id"]
            .as_str()
            .is_some_and(|actor| actor != self.store.current_user_id())
        {
            return Ok(false);
        }
        let s = run["session_id"].as_str().unwrap().to_owned();
        self.native
            .followup_consume(self.store.records(), &s, "steer", run)
            .await
    }
}
