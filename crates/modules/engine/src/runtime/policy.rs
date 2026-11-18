//! Engine policy composition. Foreign owners supply current metadata, never cached decisions.
use super::ports::{
    AgentPolicy, ChatAuthority, NativePolicy, RunObservation, RunPresentation, ScopedStorage,
};
use super::{config, Runtime};
use crate::ports::{AdmissionEffects, ChatEffects, NotificationEffects, UsageEffects};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use webagent_access::{self as access, authority::LiveAccess};
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
    pub async fn session_agent_id(&self, sid: &str) -> Result<String> {
        self.chat.agent_id(self.store.records(), sid).await
    }
    pub async fn session_agent_abilities(&self, sid: &str) -> Result<Value> {
        let agent_id = self.session_agent_id(sid).await?;
        let scoped = self.store.for_agent(&agent_id).await?;
        self.native.abilities(scoped.records(), &agent_id).await
    }
    pub async fn session_ability_state(&self, sid: &str) -> Result<Value> {
        let value = self
            .store
            .get(&format!("session_ability_state:{sid}"))
            .await?
            .unwrap_or_else(default_session_abilities);
        if value["revision"].as_u64().is_none()
            || !value["active"]
                .as_array()
                .is_some_and(|a| a.iter().all(Value::is_string))
            || !value["suppressed"]
                .as_array()
                .is_some_and(|a| a.iter().all(Value::is_string))
        {
            return Err(anyhow!("Invalid session ability state"));
        }
        Ok(value)
    }
    pub async fn session_mode(&self, sid: &str) -> Result<Value> {
        let mut value = self
            .store
            .get(&format!("session_mode:{sid}"))
            .await?
            .unwrap_or_else(default_session_mode);
        let agent = self.session_agent_id(sid).await?;
        let config = self.native.modes(self.store.records(), &agent).await?;
        if let Some(id) = value["execution_mode"].as_str() {
            // Removed custom modes fail closed to Ask.
            let entry = X::resolve_mode(&config, id).unwrap_or_else(|_| X::removed_mode());
            value["execution_mode"] = entry["id"].clone();
            value["definition"] = entry;
        }
        // Older records may still carry a "mode" field; it no longer has any meaning.
        if let Some(fields) = value.as_object_mut() {
            fields.remove("mode");
        }
        if value["revision"].as_u64().is_none() {
            return Err(anyhow!("Invalid session capability mode"));
        }
        if let Some(policy) = self
            .native
            .main_chat_schedule_approval(self.store.records(), &agent, sid)
            .await?
        {
            value["schedule_approval"] = json!(policy);
        }
        Ok(value)
    }
    pub async fn ability_controls(&self, sid: &str) -> Result<(u64, Value, Value)> {
        self.ability_controls_for(sid, self.store.current_user_id())
            .await
    }
    pub async fn ability_controls_for(
        &self,
        sid: &str,
        requester: &str,
    ) -> Result<(u64, Value, Value)> {
        let mode_revision = self.session_mode(sid).await?["revision"]
            .as_u64()
            .expect("session_mode validates a numeric revision");
        let mut visibility = self.session_agent_abilities(sid).await?;
        let agent = self.session_agent_id(sid).await?;
        let access = self
            .chat
            .live_access(self.store.records(), requester, sid, &agent)
            .await?;
        access.capability_level.decorate_visibility(&mut visibility);
        let state = self.session_ability_state(sid).await?;
        Ok((mode_revision, visibility, state))
    }
    /// An ability whose manifest names `config.required_model_role` stays off for an agent whose
    /// roster cannot resolve that role. Discovery applies it, so the model cannot load a tool it
    /// cannot run; the per-call path stays free of the model catalog read.
    pub async fn hide_without_model_role(
        &self,
        agent: &str,
        visibility: &mut Value,
        state: &Value,
    ) -> Result<()> {
        let required = self.abilities.required_model_roles(visibility, state);
        if required.is_empty() {
            return Ok(());
        }
        let roles: Vec<String> = required.iter().map(|(_, role)| role.clone()).collect();
        let scoped = self.store.for_agent(agent).await?;
        let unresolved = self
            .native
            .unresolved_model_roles(scoped.records(), agent, &roles)
            .await?;
        for (id, role) in required {
            if unresolved.contains(&role) {
                visibility["visibility"][id] = json!("off");
            }
        }
        Ok(())
    }
    /// Rechecks a native tool invocation against its persisted run and the caller's live policy
    /// immediately before it executes, then fences the run's database lease. Returns the run
    /// with `pending` set to the invocation being executed, which may come from a batch.
    pub async fn authorize_native(
        &self,
        sid: &str,
        rid: &str,
        generation: &str,
        pending_id: &str,
        name: &str,
        args: &Value,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Value> {
        let mut run = self.run(rid).await?;
        let pending = crate::runtime::batch::pending_call(&run, pending_id).unwrap_or(Value::Null);
        if cancel.is_cancelled()
            || self.shutoff().await?
            || run["session_id"] != sid
            || run["status"] != "running"
            || run["generation"] != generation
            || pending["name"] != name
            || pending["state"] != "executing"
            || pending["arguments"] != *args
            || !self
                .chat
                .participation_allows_run(self.store.records(), &run)
                .await?
        {
            return Err(anyhow!("Core invocation is no longer authorized"));
        }
        let agent = self.session_agent_id(sid).await?;
        if run["agent_id"].as_str().unwrap_or("shared_default") != agent {
            return Err(anyhow!("Run agent changed"));
        }
        if !self
            .live_tool_allowed(sid, &agent, name, &run, &pending)
            .await?
        {
            return Err(anyhow!(
                "Core tool access changed; obtain fresh authorization"
            ));
        }
        self.store
            .records()
            .native_fence(sid, rid, pending_id, generation)
            .await?;
        run["pending"] = pending;
        Ok(run)
    }
    /// A wrapper cannot use another ability's implementation to bypass its live tool denial.
    pub async fn authorize_dependency(&self, sid: &str, run: &Value, name: &str) -> Result<()> {
        let agent = self.session_agent_id(sid).await?;
        if !self
            .live_tool_allowed(sid, &agent, name, run, &run["pending"])
            .await?
        {
            return Err(anyhow!(
                "Required dependency tool is unavailable or needs fresh approval"
            ));
        }
        Ok(())
    }
    /// The live decision for `name` admits the caller now: denied tools fail, and tools that
    /// ask need the pending call's recorded human approval.
    async fn live_tool_allowed(
        &self,
        sid: &str,
        agent: &str,
        name: &str,
        run: &Value,
        pending: &Value,
    ) -> Result<bool> {
        let (_, visibility, state) = self.ability_controls(sid).await?;
        let mode = self.session_mode(sid).await?;
        let actor = self
            .chat_actor
            .as_deref()
            .unwrap_or(self.store.current_user_id());
        let access = self
            .chat
            .live_access(self.store.records(), actor, sid, agent)
            .await?;
        let decision = self
            .live_tool_decision(&access, name, &visibility, &state, &run["loaded_abilities"])
            .for_execution_mode(&mode)
            .for_schedule_approval(&mode, name, &pending["arguments"]);
        Ok(access.admitted()
            && decision.permission != access::Permission::Deny
            && (decision.permission != access::Permission::Ask
                || pending["human_approved"] == true))
    }
    pub fn live_tool_decision(
        &self,
        access: &LiveAccess,
        name: &str,
        visibility: &Value,
        state: &Value,
        loaded: &Value,
    ) -> access::ToolDecision {
        let (available, visible) = self.abilities.tool_gate(name, visibility, state, loaded);
        let abilities = self
            .abilities
            .tool_abilities(name, visibility, state, loaded);
        let gate = webagent_abilities::ToolGate {
            installed: available,
            available,
            visible,
            abilities,
        };
        self.live_tool_decision_gated(access, name, visibility, &gate)
    }
    /// `live_tool_decision` with the tool's gate already worked out by `Catalog::tool_gates`
    /// for the whole call.
    pub fn live_tool_decision_gated(
        &self,
        access: &LiveAccess,
        name: &str,
        visibility: &Value,
        gate: &webagent_abilities::ToolGate,
    ) -> access::ToolDecision {
        // Administrator tools serve only Administrator-role accounts, whatever the agent,
        // tier or level grants.
        if self.abilities.administrator_tool(name) && !access.principal.administrator() {
            return access::ToolDecision {
                permission: access::Permission::Deny,
                reason: access::Reason::CallerDenied,
            };
        }
        // Core tools (`core: true` in `<tool>.tool.json`) skip ability and tier gating.
        if webagent_abilities::tool_definitions::is_core(name) {
            return access.tool_decision(name, true, true);
        }
        let (available, visible, ability_ids) = (gate.available, gate.visible, &gate.abilities);
        let decision = access.tool_decision_for_abilities(name, ability_ids, available, visible);
        // A tool switched off for this agent is denied as if it did not exist, so no
        // denial reason confirms a hidden capability. Only the reason changes.
        if decision.permission == access::Permission::Deny
            && self.abilities.tool_off(name, visibility)
        {
            return access::ToolDecision {
                permission: access::Permission::Deny,
                reason: access::Reason::Unavailable,
            };
        }
        decision
    }
    /// The live decision for a person calling `name` directly (not the model): ability
    /// visibility and loading do not apply; the caller, tier, role and level layers do.
    pub fn direct_tool_decision(&self, access: &LiveAccess, name: &str) -> access::ToolDecision {
        if self.abilities.administrator_tool(name) && !access.principal.administrator() {
            return access::ToolDecision {
                permission: access::Permission::Deny,
                reason: access::Reason::CallerDenied,
            };
        }
        access.tool_decision(name, true, true)
    }
    pub fn filter_ability_entry(
        &self,
        access: &LiveAccess,
        entry: &mut Value,
        visibility: &Value,
        state: &Value,
        loaded: &Value,
    ) -> bool {
        let id = entry["id"].as_str().unwrap_or("");
        if !access.ability_offered(id) {
            return false;
        }
        if access.tier_ability_permission(id) == access::Permission::Deny {
            // Tell the agent up front, so it can explain before a call fails.
            entry["tier_note"] = json!("Unavailable on this agent owner's tier; tool calls fail");
        }
        let Some(tools) = entry["tools"].as_array_mut() else {
            return false;
        };
        tools.retain(|tool| {
            let name = tool["name"].as_str().unwrap_or("");
            if self.abilities.administrator_tool(name) && !access.principal.administrator() {
                return false;
            }
            let (available, _) = self.abilities.tool_gate(name, visibility, state, loaded);
            let ability_ids = self
                .abilities
                .tool_abilities(name, visibility, state, loaded);
            access
                .tool_decision_for_abilities(name, &ability_ids, available, true)
                .offered()
        });
        !tools.is_empty()
    }

    /// `filter_ability_entry` with every tool's gate already worked out by
    /// `Catalog::tool_gates` for the whole call.
    pub fn filter_ability_entry_gated(
        &self,
        access: &LiveAccess,
        entry: &mut Value,
        gates: &std::collections::BTreeMap<String, webagent_abilities::ToolGate>,
    ) -> bool {
        let id = entry["id"].as_str().unwrap_or("");
        if !access.ability_offered(id) {
            return false;
        }
        if access.tier_ability_permission(id) == access::Permission::Deny {
            // Tell the agent up front, so it can explain before a call fails.
            entry["tier_note"] = json!("Unavailable on this agent owner's tier; tool calls fail");
        }
        let Some(tools) = entry["tools"].as_array_mut() else {
            return false;
        };
        tools.retain(|tool| {
            let name = tool["name"].as_str().unwrap_or("");
            if self.abilities.administrator_tool(name) && !access.principal.administrator() {
                return false;
            }
            let gate = gates.get(name).cloned().unwrap_or_default();
            access
                .tool_decision_for_abilities(name, &gate.abilities, gate.available, true)
                .offered()
        });
        !tools.is_empty()
    }

    pub async fn action_statuses(&self, run: &Value) -> Result<Vec<Value>> {
        let ledger = self.actions.ledger(self.store.records(), run).await?;
        let agent = run["agent_id"].as_str().unwrap_or("shared_default");
        let sid = run["session_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing action session"))?;
        let (_, visibility, state) = self.ability_controls(sid).await?;
        let access = self
            .chat
            .live_access(self.store.records(), ledger.current_user_id(), sid, agent)
            .await?;
        let mut records = Vec::new();
        for action in self.actions.list_for_agent(&ledger, agent).await? {
            let name = action["tool"].as_str().unwrap_or("");
            if access.admitted()
                && self
                    .live_tool_decision(
                        &access,
                        name,
                        &visibility,
                        &state,
                        &run["loaded_abilities"],
                    )
                    .permission
                    != access::Permission::Deny
            {
                records.push(A::summary(&action));
            }
        }
        Ok(records)
    }
}
pub fn default_session_mode() -> Value {
    json!({"revision":0})
}
pub fn default_session_abilities() -> Value {
    json!({"revision":0,"active":[],"suppressed":[]})
}
