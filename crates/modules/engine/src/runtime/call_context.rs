//! One read of a model call's access inputs (stream B4, `stream-b-design.md`).
//!
//! The top of each run step already reads the caller's `live_access`; the `CallContext` adds the
//! session's mode, the agent's ability visibility (decorated by the caller's capability level)
//! and the session's ability state, read once. Context preparation then takes these instead of
//! reading them again (`context_fingerprint`, `ability_controls_for`, `session_mode`,
//! `run_model_authorized`). The dispatch fence, the tool decision and the tool execution check
//! (`foreground_authorized`) still read everything fresh: a change made during preparation is
//! caught there exactly as before. Nothing here is kept past the step.
use super::ports::{
    AgentPolicy, ChatAuthority, NativePolicy, RunObservation, RunPresentation, ScopedStorage,
};
use super::{config, Runtime};
use crate::ports::{AdmissionEffects, ChatEffects, NotificationEffects, UsageEffects};
use anyhow::Result;
use serde_json::Value;
use webagent_access::authority::LiveAccess;

#[derive(Clone, Debug)]
pub struct CallContext {
    pub requester: String,
    pub session_id: String,
    pub agent_id: String,
    pub access: LiveAccess,
    pub mode: Value,
    pub mode_revision: u64,
    pub visibility: Value,
    pub ability_state: Value,
}

impl CallContext {
    /// True when this context was read for `sid` and `requester`; any other pair reads fresh.
    pub fn answers(&self, sid: &str, requester: &str) -> bool {
        self.session_id == sid && self.requester == requester
    }
    /// The values `ability_controls_for` returns.
    pub fn controls(&self) -> (u64, Value, Value) {
        (
            self.mode_revision,
            self.visibility.clone(),
            self.ability_state.clone(),
        )
    }
}

impl<
        S: ScopedStorage
            + ChatEffects
            + AdmissionEffects
            + NotificationEffects
            + UsageEffects
            + RunObservation
            + RunPresentation,
        C: config::SchemaCatalog,
        B: crate::context::abilities::Catalog,
        A: super::actions::Actions<S>,
        H: ChatAuthority<S>,
        M,
        X: AgentPolicy<S> + NativePolicy,
    > Runtime<S, C, B, A, H, M, X>
{
    /// The step's context from the `access` it already read for (`requester`, `sid`, `agent_id`).
    /// `None` when the session's agent is not the run's (the separate reads then stay) or a
    /// read fails (the same read fails again later at its old place, with its old handling).
    pub async fn call_context(
        &self,
        sid: &str,
        agent_id: &str,
        requester: &str,
        access: &LiveAccess,
    ) -> Option<CallContext> {
        let session_agent = self.session_agent_id(sid).await.ok()?;
        if session_agent != agent_id || access.agent_id != agent_id {
            return None;
        }
        // The order of `ability_controls_for`: mode, visibility, state.
        let mode = self.session_mode(sid).await.ok()?;
        let mode_revision = mode["revision"].as_u64()?;
        let mut visibility = self.session_agent_abilities(sid).await.ok()?;
        access.capability_level.decorate_visibility(&mut visibility);
        let ability_state = self.session_ability_state(sid).await.ok()?;
        Some(CallContext {
            requester: requester.to_owned(),
            session_id: sid.to_owned(),
            agent_id: agent_id.to_owned(),
            access: access.clone(),
            mode,
            mode_revision,
            visibility,
            ability_state,
        })
    }
    /// `ability_controls_for`, answered by `context` when it was read for this pair.
    pub async fn ability_controls_in(
        &self,
        sid: &str,
        requester: &str,
        context: Option<&CallContext>,
    ) -> Result<(u64, Value, Value)> {
        match context {
            Some(context) if context.answers(sid, requester) => Ok(context.controls()),
            _ => self.ability_controls_for(sid, requester).await,
        }
    }
    /// `session_mode`, answered by `context` when it was read for this session.
    pub async fn session_mode_in(&self, sid: &str, context: Option<&CallContext>) -> Result<Value> {
        match context {
            Some(context) if context.session_id == sid => Ok(context.mode.clone()),
            _ => self.session_mode(sid).await,
        }
    }
    /// The plain revisions behind an access decision for (`requester`, `sid`, `agent_id`);
    /// unreadable parts are `None` and count as changed (`revision_shadow`).
    pub async fn revision_vector(
        &self,
        requester: &str,
        sid: &str,
        agent_id: &str,
    ) -> super::revision_shadow::RevisionVector {
        use crate::ports::AgentMetadata;
        let records = self.store.records();
        let agent = AgentMetadata::for_agent(records, agent_id).await.ok();
        let mut vector = super::revision_shadow::RevisionVector::read(
            records.application_pool(),
            records.user_pool(),
            agent
                .as_ref()
                .map_or(records.agent_pool(), |a| a.agent_pool()),
            AgentMetadata::root(records),
            requester,
            agent_id,
            sid,
            |raw| webagent_access::storage::RecordCodec::unseal(records, raw),
        )
        .await;
        if agent.is_none() {
            vector.agent = None;
        }
        vector
    }
    /// Reads the vector after a check's own reads and counts the shadow answer; never decides.
    pub async fn shadow_check(
        &self,
        kind: super::revision_shadow::Kind,
        baseline: Option<&super::revision_shadow::Baseline>,
        observed: &super::revision_shadow::Observed,
    ) {
        self.shadow_check_at(kind, baseline, observed, None).await
    }
    /// `shadow_check` for a check that also knows the run's current model `candidate`
    /// (`revision_shadow::candidate_of`); a moved candidate counts as a changed subject.
    pub async fn shadow_check_at(
        &self,
        kind: super::revision_shadow::Kind,
        baseline: Option<&super::revision_shadow::Baseline>,
        observed: &super::revision_shadow::Observed,
        candidate: Option<u64>,
    ) {
        let Some(baseline) = baseline else {
            return;
        };
        let now = self
            .revision_vector(
                &baseline.requester,
                &baseline.session_id,
                &baseline.agent_id,
            )
            .await;
        super::revision_shadow::record_at(kind, baseline, &now, observed, candidate);
        super::revision_shadow::flush(self.store.records().application_pool()).await;
    }
    /// The step's shadow baseline: `vector` was read before `context`'s data.
    pub fn shadow_baseline(
        context: &CallContext,
        vector: super::revision_shadow::RevisionVector,
        run: &Value,
    ) -> super::revision_shadow::Baseline {
        let caller = super::revision_shadow::caller_class(&context.access, vector.participant);
        let baseline = super::revision_shadow::Baseline {
            vector,
            access: super::revision_shadow::access_part(
                &context.access,
                context.mode_revision,
                &context.visibility,
                &context.ability_state,
                &context.mode,
            ),
            mode_revision: context.mode_revision,
            run_id: run["id"].as_str().unwrap_or_default().to_owned(),
            generation: run["generation"].as_str().unwrap_or_default().to_owned(),
            session_id: context.session_id.clone(),
            agent_id: context.agent_id.clone(),
            requester: context.requester.clone(),
            caller,
            candidate: super::revision_shadow::candidate_of(run),
        };
        super::revision_shadow::remember(baseline.clone());
        baseline
    }
    /// The live access for (`requester`, `sid`, `agent_id`), answered by `context` when it
    /// was read for exactly that identity.
    pub async fn live_access_in(
        &self,
        requester: &str,
        sid: &str,
        agent_id: &str,
        context: Option<&CallContext>,
    ) -> Result<LiveAccess> {
        match context {
            Some(context) if context.answers(sid, requester) && context.agent_id == agent_id => {
                Ok(context.access.clone())
            }
            _ => {
                self.chat
                    .live_access(self.store.records(), requester, sid, agent_id)
                    .await
            }
        }
    }
}
