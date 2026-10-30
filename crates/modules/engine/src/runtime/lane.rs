//! Bounded ephemeral specialized-model calls (voice, image and prompt proposals). No answer
//! enters durable chat: deltas are discarded and only the bounded final text is returned.
use super::models::ModelRecords;
use super::ports::{
    AgentPolicy, ChatAuthority, ModuleBindings, RunObservation, RunPresentation, ScopedStorage,
};
use super::{config, Runtime};
use crate::ports::{AdmissionEffects, ChatEffects, NotificationEffects, UsageEffects};
use anyhow::anyhow;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::mpsc;
use webagent_models::model_selection::{FailoverState, ModelRole};
use webagent_models::provider::ProviderSelection;

/// HTTP-facing class of a lane rejection; the API layer maps it to a status code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaneStatus {
    BadRequest,
    NotFound,
    Conflict,
    GatewayTimeout,
    BadGateway,
}

#[derive(Debug)]
pub enum LaneError {
    /// A rejection with a public message.
    Rejected(LaneStatus, &'static str),
    /// The model call did not complete. The error may mark a archived session.
    Dispatch(anyhow::Error),
    /// Storage or authority failure outside the model call.
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for LaneError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

const SESSION_NOT_FOUND: &str = "Session not found";
const DEADLINE: Duration = Duration::from_secs(90);
const MAX_ANSWER_BYTES: usize = 32768;

impl<
        S: ScopedStorage
            + ChatEffects
            + AdmissionEffects
            + NotificationEffects
            + UsageEffects
            + RunObservation
            + RunPresentation
            + ModelRecords,
        C: config::SchemaCatalog,
        B: Send + Sync,
        A: Send + Sync,
        H: ChatAuthority<S>,
        M: ModuleBindings<S>,
        X: AgentPolicy<S>,
    > Runtime<S, C, B, A, H, M, X>
{
    /// Pins the requested media role to an active session's execution epoch and agent after
    /// checking shutdown and model availability.
    pub async fn lane_run(&self, sid: &str, role: ModelRole) -> Result<Value, LaneError> {
        if self.shutoff().await? {
            return Err(LaneError::Rejected(
                LaneStatus::Conflict,
                "Global shutoff is active",
            ));
        }
        let session = self
            .store
            .get(&format!("session:{sid}"))
            .await?
            .ok_or(LaneError::Rejected(LaneStatus::BadRequest, SESSION_NOT_FOUND))?;
        if session["status"] != "active" {
            return Err(LaneError::Rejected(LaneStatus::NotFound, SESSION_NOT_FOUND));
        }
        let agent = self.chat.agent_id(self.store.records(), sid).await?;
        let mut plan = self
            .models()
            .model_catalog()
            .await?
            .resolve_role(&agent, role)
            .map_err(|_| {
                LaneError::Rejected(
                    LaneStatus::Conflict,
                    "Configure and connect the required model role for this agent",
                )
            })?;
        self.pin_model_plan(&mut plan).await.map_err(|_| {
            LaneError::Rejected(LaneStatus::Conflict, "Required model is unavailable")
        })?;
        Ok(json!({
            "agent_id": agent,
            "session_id": sid,
            "session_execution_epoch": session["execution_epoch"].as_u64().unwrap_or(0),
            "selection_plan": plan,
            "failover_state": FailoverState::default(),
        }))
    }

    /// Rechecks session/agent identity and the pinned epoch, then resolves the current
    /// candidate and its authorized credentials.
    pub async fn lane_selection(
        &self,
        run: &Value,
    ) -> Result<(ProviderSelection, Value), LaneError> {
        let sid = run["session_id"].as_str().unwrap_or("");
        self.chat.agent_id(self.store.records(), sid).await?;
        if let Some(epoch) = run["session_execution_epoch"].as_u64() {
            let current = self.store.get(&format!("session:{sid}")).await?;
            if !current.is_some_and(|s| s["execution_epoch"].as_u64().unwrap_or(0) == epoch) {
                return Err(LaneError::Rejected(LaneStatus::NotFound, SESSION_NOT_FOUND));
            }
        }
        let candidate = self
            .run_candidate(run)
            .await
            .map_err(|_| LaneError::Rejected(LaneStatus::Conflict, "Model access changed"))?
            .ok_or(LaneError::Rejected(LaneStatus::Conflict, "Model unavailable"))?;
        let selection = ProviderSelection {
            limits: candidate.limits.clone(),
            provider: candidate.transport.provider_id(),
            model: candidate.upstream_model,
        };
        let tokens = self.tokens_for(&selection, run).await.map_err(|_| {
            LaneError::Rejected(LaneStatus::Conflict, "Connect the required model provider")
        })?;
        Ok((selection, tokens))
    }

    /// Returns bounded private media text under session-epoch supervision and a 90-second
    /// deadline. Prepares the provider projection before admission, then fences current
    /// model/component authority across the first network poll.
    pub fn lane_answer<'a>(
        &'a self,
        run: &'a Value,
        input: Vec<Value>,
        instructions: &'a str,
    ) -> impl std::future::Future<Output = Result<String, LaneError>> + Send + 'a {
        // Keep the complete media state machine on the heap: callers otherwise embed both
        // credential-selection futures and their dispatch fence. Borrowing preserves
        // session_operation cancellation without a detached task.
        Box::pin(async move {
            let (selection, tokens) = self.lane_selection(run).await?;
            let sid = run["session_id"].as_str().unwrap_or("");
            let root = crate::ports::AgentMetadata::root(self.store.records());
            let (tx, mut rx) = mpsc::channel(16);
            let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
            let cancel = self.stop.child_token();
            let call = async {
                // Component IPC completes before the final session/model authority check.
                let engine_revision = crate::components::catalog()?.revision.clone();
                let engine_controls = crate::components::controls(root)?.revision;
                let prepared = crate::model_client::CallDraft::new(
                    &selection,
                    input,
                    vec![],
                    sid,
                    instructions,
                )
                .prepare(root, &cancel)
                .await?;
                let dispatch_guard = self.gate.lock().await;
                let (current, _) = self
                    .lane_selection(run)
                    .await
                    .map_err(|_| anyhow!("Model access changed"))?;
                if current != selection || self.shutoff().await? {
                    return Err(anyhow!("Model access changed"));
                }
                if cancel.is_cancelled()
                    || crate::components::catalog()?.revision != engine_revision
                    || crate::components::controls(root)?.revision != engine_controls
                    || !crate::components::component_available(
                        root,
                        crate::components::PROJECTION,
                    )
                {
                    return Err(anyhow!("Provider component or authority changed"));
                }
                // Poll the actual transport once while authority writers remain fenced.
                let mut completion = Box::pin(async {
                    let ready = prepared.bind_credentials(&tokens, &cancel)?;
                    self.provider.dispatch(ready, tx, cancel.clone()).await
                });
                let mut immediate = None;
                futures_util::future::poll_fn(|cx| {
                    if let std::task::Poll::Ready(result) =
                        std::future::Future::poll(completion.as_mut(), cx)
                    {
                        immediate = Some(result);
                    }
                    std::task::Poll::Ready(())
                })
                .await;
                drop(dispatch_guard);
                match immediate {
                    Some(result) => result,
                    None => completion.await,
                }
            };
            let result = tokio::time::timeout(
                DEADLINE,
                self.session_operation(sid, cancel.clone(), call),
            )
            .await;
            cancel.cancel();
            drain.abort();
            let result = result
                .map_err(|_| {
                    LaneError::Rejected(
                        LaneStatus::GatewayTimeout,
                        "Voice or image model timed out",
                    )
                })?
                .map_err(LaneError::Dispatch)?;
            self.lane_selection(run).await?;
            let answer = result["output"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|item| item["type"] == "message")
                .flat_map(|item| item["content"].as_array().into_iter().flatten())
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if answer.trim().is_empty() || answer.len() > MAX_ANSWER_BYTES {
                return Err(LaneError::Rejected(
                    LaneStatus::BadGateway,
                    "Model returned no bounded answer",
                ));
            }
            Ok(answer)
        })
    }
}
