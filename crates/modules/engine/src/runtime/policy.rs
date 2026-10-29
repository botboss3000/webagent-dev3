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
            // Removed custom modes fail closed to Ask; never inherit write authority.
            let entry = X::resolve_mode(&config, id).unwrap_or_else(|_| X::removed_mode());
            // A configuration edit cannot elevate an already read-only conversation.
            // Explicit mode selection must cross the existing approval boundary.
            if value["mode"] != "read_only" || entry["permission_policy"] == "read_only" {
                value["mode"] = entry["permission_policy"].clone();
            }
            value["execution_mode"] = entry["id"].clone();
            value["definition"] = entry;
        }
        if !matches!(value["mode"].as_str(), Some("write" | "read_only"))
            || value["revision"].as_u64().is_none()
        {
            return Err(anyhow!("Invalid session capability mode"));
        }
        Ok(value)
    }
    pub async fn effective_policy(&self, sid: &str) -> Result<(Value, u64)> {
        self.effective_policy_for_mode(&self.session_mode(sid).await?)
            .await
    }
    pub async fn effective_policy_for_mode(&self, mode: &Value) -> Result<(Value, u64)> {
        let mut policy = self
            .store
            .get("policy")
            .await?
            .unwrap_or_else(|| self.policy_default());
        // New Base discovery is available by default; an explicit saved denial wins.
        for name in ["search_tools", "list_tools", "load_tool"] {
            if policy["rules"].get(name).is_none() {
                policy["rules"][name] = json!("allow");
            }
        }
        for name in ["task_status", "task_wait", "task_cancel"] {
            if policy["rules"].get(name).is_none() {
                policy["rules"][name] = json!("allow");
            }
        }
        policy = H::messaging_policy_defaults(policy);
        if mode["mode"] == "read_only" {
            for schema in C::schemas()? {
                let name = schema["name"].as_str().unwrap_or("");
                if !A::is_read(name) && !H::messaging_owns(name) {
                    policy["rules"][name] = json!("deny");
                }
            }
        }
        let revision = mode["revision"]
            .as_u64()
            .expect("session_mode validates a numeric revision");
        Ok((policy, revision))
    }
    pub async fn ability_controls(&self, sid: &str) -> Result<(Value, u64, Value, Value)> {
        self.ability_controls_for(sid, self.store.current_user_id())
            .await
    }
    pub async fn ability_controls_for(
        &self,
        sid: &str,
        requester: &str,
    ) -> Result<(Value, u64, Value, Value)> {
        let (policy, mode_revision) = self.effective_policy(sid).await?;
        let mut visibility = self.session_agent_abilities(sid).await?;
        let agent = self.session_agent_id(sid).await?;
        let access = self
            .chat
            .live_access(self.store.records(), requester, sid, &agent)
            .await?;
        access.capability_level.decorate_visibility(&mut visibility);
        let state = self.session_ability_state(sid).await?;
        Ok((policy, mode_revision, visibility, state))
    }
    /// An ability whose manifest names `config.required_model_role` stays off for an agent whose
    /// roster cannot resolve that role. Discovery applies it, so the model cannot load a tool it
    /// cannot run; the per-call path stays free of the model catalog read.
    pub(crate) async fn hide_without_model_role(
        &self,
        agent: &str,
        policy: &Value,
        visibility: &mut Value,
        state: &Value,
    ) -> Result<()> {
        let required: Vec<(String, String)> = self
            .abilities
            .catalog_with(policy, visibility, state)
            .into_iter()
            .filter_map(|entry| {
                Some((
                    entry["id"].as_str()?.to_owned(),
                    entry["config"]["required_model_role"].as_str()?.to_owned(),
                ))
            })
            .collect();
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
        let pending = if run["pending"]["id"] == pending_id {
            run["pending"].clone()
        } else {
            run["batch"]
                .as_array()
                .and_then(|xs| xs.iter().find(|x| x["pending"]["id"] == pending_id))
                .map(|v| v["pending"].clone())
                .unwrap_or(Value::Null)
        };
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
        let (policy, _, visibility, state) = self.ability_controls(sid).await?;
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
            .live_tool_decision(
                &access,
                name,
                &policy,
                &visibility,
                &state,
                &run["loaded_abilities"],
                mode["mode"] == "read_only",
            )
            .for_execution_mode(&mode);
        Ok(access.admitted()
            && decision.permission != access::Permission::Deny
            && (decision.permission != access::Permission::Ask
                || pending["human_approved"] == true))
    }
    pub fn live_tool_decision(
        &self,
        access: &LiveAccess,
        name: &str,
        policy: &Value,
        visibility: &Value,
        state: &Value,
        loaded: &Value,
        read_only: bool,
    ) -> access::ToolDecision {
        let installation = access::Permission::from_policy(policy_decision(policy, name))
            .unwrap_or(access::Permission::Deny);
        let (available, visible) = self.abilities.tool_gate(name, visibility, state, loaded);
        let ability_ids = self
            .abilities
            .tool_abilities(name, visibility, state, loaded);
        let caution = if !A::is_read(name)
            && ability_ids.iter().any(|id| {
                let key = match id.as_str() {
                    "terminal_control" => "terminal_caution",
                    "codebase_admin" => "destructive_caution",
                    _ => return false,
                };
                visibility["settings"][id][key] == "cautious"
            }) {
            access::Permission::Ask
        } else {
            access::Permission::Auto
        };
        let decision = access.tool_decision_for_abilities(
            name,
            &ability_ids,
            installation.meet(caution),
            available,
            visible,
            read_only && !H::messaging_owns(name),
            A::is_read(name),
        );
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
    pub fn policy_default(&self) -> Value {
        default_policy::<A, X>()
    }
    pub fn filter_ability_entry(
        &self,
        access: &LiveAccess,
        entry: &mut Value,
        policy: &Value,
        visibility: &Value,
        state: &Value,
        loaded: &Value,
        read_only: bool,
    ) -> bool {
        let id = entry["id"].as_str().unwrap_or("");
        if access.ability_permission(id) == access::Permission::Deny {
            return false;
        }
        let Some(tools) = entry["tools"].as_array_mut() else {
            return false;
        };
        tools.retain(|tool| {
            let name = tool["name"].as_str().unwrap_or("");
            let installation = access::Permission::from_policy(policy_decision(policy, name))
                .unwrap_or(access::Permission::Deny);
            let (available, _) = self.abilities.tool_gate(name, visibility, state, loaded);
            let ability_ids = self
                .abilities
                .tool_abilities(name, visibility, state, loaded);
            access
                .tool_decision_for_abilities(
                    name,
                    &ability_ids,
                    installation,
                    available,
                    true,
                    read_only && !H::messaging_owns(name),
                    A::is_read(name),
                )
                .permission
                != access::Permission::Deny
        });
        !tools.is_empty()
    }

    pub async fn action_statuses(&self, run: &Value) -> Result<Vec<Value>> {
        let ledger = self.actions.ledger(self.store.records(), run).await?;
        let agent = run["agent_id"].as_str().unwrap_or("shared_default");
        let sid = run["session_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing action session"))?;
        let (policy, _, visibility, state) = self.ability_controls(sid).await?;
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
                        &policy,
                        &visibility,
                        &state,
                        &run["loaded_abilities"],
                        false,
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
    json!({"mode":"write","revision":0})
}
pub fn default_session_abilities() -> Value {
    json!({"revision":0,"active":[],"suppressed":[]})
}
pub fn policy_decision<'a>(policy: &'a Value, name: &str) -> &'a str {
    policy["rules"][name].as_str().unwrap_or("deny")
}

pub fn default_policy<A: crate::tools::execution::ActionCatalog, X: NativePolicy>() -> Value {
    let mut policy = json!({"version":1,"rules":{"list_abilities":"allow","load_ability":"allow","list_skills":"allow","load_skill":"allow","context_assets":"allow","get_time":"allow","get_date":"allow","calculate":"allow","read_attachment":"allow","search_this_session":"allow","read_own_prompt":"allow","web_search":"allow","web_read":"allow","get_weather":"allow","maps_geocode":"allow","file_list":"allow","file_read":"allow","file_search":"allow","state_inspect":"allow","prompt_slot_read":"allow","prompt_slot_write":"require_approval","file_write":"require_approval","checkout_edit":"require_approval","file_edit":"require_approval","workspace_seed":"require_approval","source_review":"allow","source_patch":"require_approval","promotion_propose":"allow","shell":"require_approval","python":"require_approval"}});
    for name in ["task_status", "task_wait", "task_cancel"] {
        policy["rules"][name] = json!("allow");
    }
    for schema in X::default_policy_schemas() {
        let Some(name) = schema["name"].as_str() else {
            continue;
        };
        policy["rules"][name] = json!(if A::is_read(name) {
            "allow"
        } else {
            "require_approval"
        });
    }
    for name in ["message_other_session", "start_session"] {
        policy["rules"][name] = json!("allow");
    }
    // Visitor front-desk actions only store a lead, notify staff or show reply buttons;
    // staff-side writes keep the require_approval default above.
    for name in ["lead_capture", "request_human", "offer_choices"] {
        if policy["rules"].get(name).is_some() {
            policy["rules"][name] = json!("allow");
        }
    }
    #[cfg(any(test, feature = "test-support"))]
    {
        policy["rules"]["test_owner_purchase"] = json!("require_approval");
    }
    policy
}
