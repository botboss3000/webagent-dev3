//! Runs the critical tool guard (rules in `tools::critical_guard`) for one call: gathers the
//! user's messages, then asks the agent's guard models in order, each within its own time
//! and all within the total, until one answers. A decision model is asked through the System
//! One request; a text model through a bounded lane answer. Nothing it returns enters chat.
use super::lane::LaneError;
use super::models::ModelRecords;
use super::ports::{
    AgentPolicy, ChatAuthority, MainGuidance, ModuleBindings, RunObservation, RunPresentation,
    ScopedStorage,
};
use super::{config, Runtime};
use crate::ports::{AdmissionEffects, ChatEffects, NotificationEffects, UsageEffects};
use crate::tools::critical_guard::{self as rules, Message, Mode, Policy, Verdict};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use webagent_models::model_selection::{FailoverState, ModelRole, SelectionPlan};
use webagent_models::provider::ProviderId;

/// Whether the guard runs for an agent that saved no mode. Runtime test fixtures switch it
/// off, since most of them count model calls exactly; a test that covers the guard saves a mode.
pub static GUARD_DEFAULT: AtomicBool = AtomicBool::new(true);
const INSTRUCTIONS: &str =
    "You check whether a user asked for an action. Answer in the exact one-line form requested.";

fn lane_error(error: LaneError) -> anyhow::Error {
    match error {
        LaneError::Rejected(_, message) => anyhow!(message),
        LaneError::Dispatch(error) | LaneError::Internal(error) => error,
    }
}

/// What the guard shows a model for call `name(args)`: the state (rule, the newest messages,
/// the shortened arguments) and the messages it offers to link.
fn guard_input(
    definition: &webagent_abilities::tool_definitions::ToolDefinition,
    name: &str,
    args: &Value,
    messages: &[Message],
    policy: &Policy,
) -> (Value, Vec<Message>) {
    let messages = rules::recent(messages, policy);
    let guard = definition.guard.as_ref();
    let shown = rules::shown_arguments(args, guard, policy.argument_max_chars);
    (rules::state(name, shown, guard, &messages), messages)
}

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
        X: AgentPolicy<S> + MainGuidance<S>,
    > Runtime<S, C, B, A, H, M, X>
{
    /// The guard record for call `name(args)` of `run`, or `None` when the tool is not
    /// critical or the agent's guard mode (`settings`, the agent's ability settings) is off.
    /// A guard that cannot reach any model returns a record that did not pass, never an error.
    pub async fn critical_guard(
        &self,
        run: &Value,
        name: &str,
        args: &Value,
        settings: &Value,
    ) -> Result<Option<Value>> {
        let Some(definition) = webagent_abilities::tool_definitions::get(name) else {
            return Ok(None);
        };
        let setting = &settings["task_progress"]["critical_guard"];
        let mode = if setting.is_null() && !GUARD_DEFAULT.load(Ordering::Relaxed) {
            Mode::Off
        } else {
            Mode::parse(setting)
        };
        if !definition.critical || mode == Mode::Off {
            return Ok(None);
        }
        let policy = Policy::live();
        let sid = run["session_id"].as_str().unwrap_or("");
        let rid = run["id"].as_str().unwrap_or("");
        let messages: Vec<Message> = self
            .native
            .guard_messages(self.store.records(), sid, rid, policy.messages)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(label, text)| Message { label, text })
            .collect();
        let (state, messages) = guard_input(&definition, name, args, &messages, &policy);
        let (verdict, error) = match self.guard_verdict(run, &state, &messages, &policy).await {
            Ok(verdict) => (Some(verdict), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let passed = verdict.as_ref().is_some_and(|v| v.passes(&policy));
        let mut record = rules::record(verdict.as_ref(), error.as_deref(), mode, passed);
        record["line"] = json!(rules::approval_line(&record));
        Ok(Some(record))
    }

    /// An administrator's guard probe: guard check for call `name(args)` with user `messages`,
    /// asked of one model (`entry_id`, a roster entry of session `sid`'s agent) exactly as the
    /// guard would ask it, with no time limit beyond the model's own. Returns the verdict, or
    /// the error, and the milliseconds the model took. Nothing is recorded.
    pub async fn guard_probe(
        &self,
        sid: &str,
        entry_id: &str,
        name: &str,
        args: &Value,
        messages: Vec<Message>,
    ) -> Result<Value> {
        let definition = webagent_abilities::tool_definitions::get(name)
            .ok_or_else(|| anyhow!("Unknown tool"))?;
        let agent = self.chat.agent_id(self.store.records(), sid).await?;
        let policy = Policy::live();
        let (state, messages) = guard_input(&definition, name, args, &messages, &policy);
        let mut plan = self
            .models()
            .model_catalog()
            .await?
            .resolve_guard_entry(&agent, entry_id)?;
        self.pin_model_plan(&mut plan).await?;
        let run = json!({"agent_id":agent,"session_id":sid,
            "requester_user_id":self.store.records().current_user_id()});
        let started = Instant::now();
        let answer = self.guard_ask(&run, plan, &state, &messages).await;
        let elapsed = started.elapsed().as_millis() as u64;
        Ok(match answer {
            Ok(verdict) => json!({"verdict":verdict.asked.as_str(),"confidence":verdict.confidence,
                "message":verdict.message,"model":verdict.model,
                "passes":verdict.passes(&policy),"elapsed_ms":elapsed}),
            Err(error) => json!({"error":error.to_string(),"elapsed_ms":elapsed}),
        })
    }

    /// Ask the guard models in order until one answers, within the policy's time limits.
    async fn guard_verdict(
        &self,
        run: &Value,
        state: &Value,
        messages: &[Message],
        policy: &Policy,
    ) -> Result<Verdict> {
        let agent = run["agent_id"].as_str().unwrap_or("");
        let plans = self.models().model_catalog().await?.resolve_guard(agent)?;
        let started = Instant::now();
        let total = Duration::from_secs(policy.total_seconds);
        let mut last = anyhow!("No guard model answered");
        for mut plan in plans {
            let left = total.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Err(anyhow!("Guard timed out"));
            }
            let limit = left.min(Duration::from_secs(policy.model_seconds));
            if let Err(error) = self.pin_model_plan(&mut plan).await {
                last = error;
                continue;
            }
            let answer = tokio::time::timeout(limit, self.guard_ask(run, plan, state, messages));
            match answer.await {
                Ok(Ok(verdict)) => return Ok(verdict),
                Ok(Err(error)) => last = error,
                Err(_) => last = anyhow!("Guard model timed out"),
            }
        }
        Err(last)
    }

    /// One guard model's verdict.
    async fn guard_ask(
        &self,
        run: &Value,
        plan: SelectionPlan,
        state: &Value,
        messages: &[Message],
    ) -> Result<Verdict> {
        let decision = plan.role == ModelRole::Decision;
        let lane = json!({
            "agent_id": run["agent_id"],
            "session_id": run["session_id"],
            "requester_user_id": run["requester_user_id"],
            "selection_plan": plan,
            "failover_state": FailoverState::default(),
        });
        if decision {
            use webagent_models::decision;
            let (selection, tokens) = self.lane_selection(&lane).await.map_err(lane_error)?;
            // The caller's timeout bounds the request; this only caps a stuck connection.
            let limit = Duration::from_secs(120);
            let choices = if selection.provider == ProviderId::Local {
                // A self-hosted model reads little: show it only the newest messages that fit.
                let reads = decision::local_input_limit(&selection.model).await?;
                let (state, messages) = rules::fit(state, messages, reads);
                let body = decision::request(&selection.model, state, &rules::questions(&messages));
                decision::ask_local(&selection.model, &body, limit).await?
            } else {
                let provider = webagent_models::providers::load(selection.provider)?
                    .ok_or_else(|| anyhow!("Decision provider file missing"))?;
                let body =
                    decision::request(&selection.model, state.clone(), &rules::questions(messages));
                decision::ask(&provider.base_url, tokens["api_key"].as_str(), &body, limit).await?
            };
            return rules::from_choices(&choices, &selection.model);
        }
        let model = plan
            .candidates
            .first()
            .map(|c| c.upstream_model.clone())
            .unwrap_or_default();
        let input = vec![json!({"role":"user","content":rules::text_prompt(state)})];
        let answer = self
            // The tool step running the guard holds the dispatch gate already.
            .lane_answer_gated(&lane, input, INSTRUCTIONS, true)
            .await
            .map_err(lane_error)?;
        rules::from_text(&answer, messages, &model)
            .ok_or_else(|| anyhow!("Guard model gave no verdict"))
    }
}
