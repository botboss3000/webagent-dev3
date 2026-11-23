//! Answer "may this person use this agent or tool right now?" Account status,
//! tier, agent relationship, and permission all matter. Visibility only decides
//! what the model sees; it never grants execution. A fresh access snapshot is
//! required at each checkpoint because an earlier approval can become stale.
use crate::storage::{RecordCodec, Repository};
use crate::{
    self as access, AccountStatus, Permission, Principal, Relationship, Tier, ToolDecision,
    ToolFactors,
};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;

pub fn permission_map(value: &Value) -> Result<()> {
    if value.is_null() {
        return Ok(());
    }
    let map = value
        .as_object()
        .ok_or_else(|| anyhow!("Invalid permission map"))?;
    if map.len() > 128 {
        return Err(anyhow!("Permission map exceeds limit"));
    }
    for (name, mode) in map {
        if name != "*" && !access::valid_id(name) {
            return Err(anyhow!("Invalid permission name"));
        }
        Permission::from_policy(
            mode.as_str()
                .ok_or_else(|| anyhow!("Invalid permission mode"))?,
        )?;
    }
    Ok(())
}

pub fn caller_map(value: &Value) -> Result<()> {
    if value.is_null() {
        return Ok(());
    }
    let map = value
        .as_object()
        .ok_or_else(|| anyhow!("Invalid tool caller map"))?;
    if map.len() > 128 {
        return Err(anyhow!("Tool caller map exceeds limit"));
    }
    for (name, roles) in map {
        if name != "*" && !access::valid_id(name) {
            return Err(anyhow!("Invalid tool caller name"));
        }
        let roles = roles
            .as_array()
            .ok_or_else(|| anyhow!("Invalid tool caller roles"))?;
        if roles.len() > 3 {
            return Err(anyhow!("Too many tool caller roles"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for role in roles {
            let role = role
                .as_str()
                .ok_or_else(|| anyhow!("Invalid tool caller role"))?;
            if !matches!(role, "owner" | "member" | "caller") || !seen.insert(role) {
                return Err(anyhow!("Invalid tool caller role"));
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessRevision {
    pub account: u64,
    pub tier: u64,
    pub membership: u64,
    pub agent: u64,
    #[serde(default)]
    pub policy: String,
    #[serde(default)]
    pub capability_level: u64,
}

#[derive(Clone, Debug)]
pub struct LiveAccess {
    pub principal: Principal,
    pub tier: Tier,
    pub agent_id: String,
    pub relationship: Relationship,
    pub agent_enabled: bool,
    pub agent_administrator: bool,
    /// The agent's owner (directory record); empty when unreadable.
    pub agent_owner_id: String,
    /// The agent owner's app tier: it decides which of the agent's abilities work.
    pub owner_tier_id: String,
    /// The owner's effective tier, from the same policy read: the owner pays, so it decides
    /// which models the agent may run. `None` (no owner, unknown tier) allows no model.
    pub owner_tier: Option<Tier>,
    /// The owner's tier and every tier ranked below it, from the same policy read: a tier
    /// gets each ability and tool any tier below it gets (`account_ladder::inherited`).
    pub owner_tiers: Vec<String>,
    pub revision: AccessRevision,
    pub capability_level: crate::capabilities::CapabilityLevel,
    /// The policy's run-sharing limits, read with the tier from the same policy document.
    pub run_limits: crate::run_limits::RunLimits,
    tool_permissions: Value,
    ability_permissions: Value,
    tool_callers: Value,
}

impl LiveAccess {
    /// CONTEXT-AUTH-FINGERPRINT: explicit shaping inputs, not Debug formatting.
    /// This private value is hashed by the run-local cache and never sent to the
    /// provider or browser. Include grants as well as revisions so malformed
    /// revision updates cannot retain stale guidance or tool exposure.
    pub fn context_fingerprint(&self) -> Value {
        serde_json::json!([
            self.principal.user_id,
            self.principal.status,
            self.principal.role,
            self.principal.tier_id,
            self.principal.access_revision,
            self.tier,
            self.agent_id,
            self.relationship,
            self.agent_enabled,
            self.revision,
            self.agent_administrator,
            self.agent_owner_id,
            self.owner_tier_id,
            self.owner_tier,
            self.owner_tiers,
            self.capability_level,
            self.tool_permissions,
            self.ability_permissions,
            self.tool_callers
        ])
    }

    pub fn admitted(&self) -> bool {
        self.principal.status == AccountStatus::Active
            && self.relationship != Relationship::Revoked
            && self.agent_enabled
    }

    /// Whether a run on this agent may use `model` through a connection `model_owner` owns.
    /// The agent's owner pays, so the owner's tier decides for every caller: the owner's own
    /// connected models need only the tier's own-model permission, any other (catalog) model
    /// must be in the tier's allowed models.
    pub fn model_allowed(&self, model: &str, model_owner: &str) -> bool {
        self.admitted() && self.owner_tier_allows_model(model, model_owner)
    }
    /// The owner's-tier half of [`Self::model_allowed`], for model administration, which
    /// also serves agents that are not admitting runs (disabled, or the editor's own access).
    pub fn owner_tier_allows_model(&self, model: &str, model_owner: &str) -> bool {
        self.owner_tier.as_ref().is_some_and(|tier| {
            if !self.agent_owner_id.is_empty() && model_owner == self.agent_owner_id {
                tier.allows_own_models()
            } else {
                tier.allows_model(model)
            }
        })
    }

    pub fn max_sessions(&self) -> u32 {
        self.tier.max_sessions
    }
    /// The tier's limit; a test account also stays within the live test-account cap
    /// (`config/accounts/test-accounts.json`), so a test never starves real users.
    pub fn max_active_runs(&self) -> u32 {
        if self.principal.test_account {
            self.tier
                .max_active_runs
                .min(webagent_accounts::TestAccountLimits::live().max_active_runs)
        } else {
            self.tier.max_active_runs
        }
    }

    fn agent_tool_permission(&self, name: &str) -> Permission {
        let value = self
            .tool_permissions
            .get(name)
            .or_else(|| self.tool_permissions.get("*"));
        match value {
            None => Permission::Auto,
            Some(Value::String(value)) => {
                Permission::from_policy(value).unwrap_or(Permission::Deny)
            }
            _ => Permission::Deny,
        }
    }

    pub fn ability_permission(&self, ability_id: &str) -> Permission {
        if !self.capability_level.ability_enabled(ability_id)
            || !self.ability_role_allowed(ability_id)
        {
            return Permission::Deny;
        }
        self.base_ability_permission(ability_id)
            .meet(self.tier_ability_permission(ability_id))
            .meet(self.level_ability_permission(ability_id))
    }
    /// Whether the agent is shown the ability: allowed, or blocked only by the owner's
    /// tier (its tools then fail with the tier reason). Administrator abilities are never
    /// shown to anyone they do not serve.
    pub fn ability_offered(&self, ability_id: &str) -> bool {
        if self.ability_permission(ability_id) != Permission::Deny {
            return true;
        }
        self.capability_level.ability_enabled(ability_id)
            && self.ability_role_allowed(ability_id)
            && self.tier_ability_permission(ability_id) == Permission::Deny
            && !access::administrator_ability(ability_id)
            && self
                .base_ability_permission(ability_id)
                .meet(self.level_ability_permission(ability_id))
                != Permission::Deny
    }
    /// The owner's tier, from the ability's `owner_tier`; Administrator abilities also need
    /// an administrator caller. Paused, not removed: the agent keeps its saved design.
    pub fn tier_ability_permission(&self, ability_id: &str) -> Permission {
        if access::ability_tier_allowed(ability_id, &self.owner_tiers, &self.principal.tier_id) {
            Permission::Auto
        } else {
            Permission::Deny
        }
    }
    /// A tool's own `owner_tier` for the owner's tier, narrowing its abilities' lists.
    pub fn tier_tool_permission(&self, name: &str) -> Permission {
        if access::tool_tier_allowed(name, &self.owner_tiers) {
            Permission::Auto
        } else {
            Permission::Deny
        }
    }
    fn level_ability_permission(&self, ability_id: &str) -> Permission {
        if ability_id == access::BUILT_IN_ABILITY {
            Permission::Auto
        } else {
            self.capability_level.ability_permission(ability_id)
        }
    }
    fn base_ability_permission(&self, ability_id: &str) -> Permission {
        match self
            .ability_permissions
            .get(ability_id)
            .or_else(|| self.ability_permissions.get("*"))
        {
            None => Permission::Auto,
            Some(Value::String(value)) => {
                Permission::from_policy(value).unwrap_or(Permission::Deny)
            }
            _ => Permission::Deny,
        }
    }

    fn caller_allowed(&self, name: &str) -> bool {
        let value = self
            .tool_callers
            .get(name)
            .or_else(|| self.tool_callers.get("*"));
        match value {
            None => true,
            Some(Value::Array(items)) => items.iter().any(|item| {
                item.as_str()
                    == Some(match self.relationship {
                        Relationship::Owner => "owner",
                        Relationship::Member => "member",
                        Relationship::Caller => "caller",
                        Relationship::Revoked => "revoked",
                    })
            }),
            _ => false,
        }
    }

    /// The rungs of the caller's level: it, then the levels below it.
    fn rungs(&self) -> Vec<String> {
        self.capability_level
            .chain
            .first()
            .map(|(rungs, _)| rungs.clone())
            .unwrap_or_else(|| vec![self.capability_level.id.clone()])
    }
    /// Whether the caller's level may ever have the ability, by its `role_access`.
    pub fn ability_role_allowed(&self, ability_id: &str) -> bool {
        access::ability_role_allowed(ability_id, &self.rungs())
    }
    /// A tool's own `role_access`, else any of its abilities' (every role without one).
    fn tool_role_allowed(&self, name: &str, ability_ids: &[String]) -> bool {
        let rungs = self.rungs();
        access::tool_role_allowed(name, &rungs).unwrap_or_else(|| {
            ability_ids.is_empty()
                || ability_ids
                    .iter()
                    .any(|id| access::ability_role_allowed(id, &rungs))
        })
    }

    /// `available` must come from the installed tool catalog; `visible`
    /// must come from the agent and session visibility state. Neither grants
    /// access over a Deny in the tier, role or agent permission plane.
    pub fn tool_decision(&self, name: &str, available: bool, visible: bool) -> ToolDecision {
        if !self.tool_role_allowed(name, &[]) {
            return ToolDecision {
                permission: Permission::Deny,
                reason: access::Reason::RoleDenied,
            };
        }
        self.resolve_tool(name, Permission::Auto, available, visible)
    }

    pub fn tool_decision_for_abilities(
        &self,
        name: &str,
        ability_ids: &[String],
        available: bool,
        visible: bool,
    ) -> ToolDecision {
        if matches!(
            name,
            "prompt_slot_write" | "edit_own_prompt" | "save_own_skill" | "remove_own_skill"
        ) && !self.agent_administrator
        {
            return ToolDecision {
                permission: Permission::Deny,
                reason: access::Reason::CallerDenied,
            };
        }
        if !self.capability_level.tool_enabled(name) {
            return ToolDecision {
                permission: Permission::Deny,
                reason: access::Reason::AgentDenied,
            };
        }
        // A hard limit from tool.json: hidden from levels its roles exclude.
        if !self.tool_role_allowed(name, ability_ids) {
            return ToolDecision {
                permission: Permission::Deny,
                reason: access::Reason::RoleDenied,
            };
        }
        // A tool several abilities provide works through any the owner's tier allows.
        let allowed: Vec<&String> = ability_ids
            .iter()
            .filter(|id| self.tier_ability_permission(id) != Permission::Deny)
            .collect();
        let ability = if ability_ids.is_empty() {
            if available {
                Permission::Deny
            } else {
                Permission::Auto
            }
        } else if allowed.is_empty() {
            Permission::Deny
        } else {
            allowed.iter().fold(Permission::Auto, |permission, id| {
                permission.meet(self.ability_permission(id))
            })
        };
        // One calculation drives discovery, guidance, schemas, previews and dispatch.
        // Report the narrowing layer without interpreting visibility as permission.
        if self.tier_tool_permission(name) != Permission::Deny {
            if !ability_ids.is_empty() && allowed.is_empty() {
                // Administrator tools are hidden, not offered with a tier error.
                let reason = if ability_ids
                    .iter()
                    .all(|id| access::administrator_ability(id))
                {
                    access::Reason::CallerDenied
                } else {
                    access::Reason::TierDenied
                };
                return ToolDecision {
                    permission: Permission::Deny,
                    reason,
                };
            }
            if allowed
                .iter()
                .any(|id| self.level_ability_permission(id) == Permission::Deny)
            {
                return ToolDecision {
                    permission: Permission::Deny,
                    reason: access::Reason::CapabilityDenied,
                };
            }
        }
        self.resolve_tool(name, ability, available, visible)
    }

    fn resolve_tool(
        &self,
        name: &str,
        ability: Permission,
        available: bool,
        visible: bool,
    ) -> ToolDecision {
        if name == "build_work" && !self.principal.administrator() {
            return ToolDecision {
                permission: Permission::Deny,
                reason: access::Reason::CallerDenied,
            };
        }
        let level = self.capability_level.tool_permission(name);
        if level == Permission::Deny
            && self.tier_tool_permission(name) != Permission::Deny
            && self.agent_tool_permission(name) != Permission::Deny
        {
            return ToolDecision {
                permission: Permission::Deny,
                reason: access::Reason::CapabilityDenied,
            };
        }
        ToolFactors {
            tier: self.tier_tool_permission(name),
            agent: self.agent_tool_permission(name).meet(level),
            ability,
            account_active: self.principal.active(),
            relationship: self.relationship != Relationship::Revoked && self.caller_allowed(name),
            agent_enabled: self.agent_enabled,
            available,
            visible,
        }
        .resolve()
    }
}

impl<C: RecordCodec> Repository<'_, C> {
    pub async fn live_access(&self, user_id: &str, agent_id: &str) -> Result<LiveAccess> {
        validate_live_access(user_id, agent_id)?;
        let agent_view = self.for_agent(agent_id).await?;
        let account = self
            .account_by_id(user_id)
            .await?
            .ok_or_else(|| anyhow!("Account not found"))?;
        let principal = Principal::from_account(&account)?;
        if principal.user_id != user_id {
            return Err(anyhow!("Account identity conflict"));
        }
        // Guest policy is rechecked by the loop as well as HTTP authentication.
        let (policy, policy_digest) = self.read_access_policy_digested()?;
        if principal.role == access::Role::Guest {
            if policy["registration"] != "open" {
                return Err(anyhow!("Guest access is not enabled"));
            }
        }
        let tier = crate::account_ladder::effective(&policy, &principal.tier_id)?;
        tier.validate()?;
        let run_limits = crate::run_limits::RunLimits::from_policy(&policy)?;
        if tier.id != principal.tier_id {
            return Err(anyhow!("Tier identity conflict"));
        }
        // Each record is read once here (the owner, the agent record, the membership and
        // the account); the checks are those of `resolved_membership`, `agent_owner`,
        // `agent_authority` and `capability_levels`, which used to read them again.
        let owner: String = sqlx::query_scalar("SELECT owner_id FROM agent_directory WHERE id=?")
            .bind(agent_id)
            .fetch_one(self.app_pool)
            .await?;
        let agent = sqlx::query("SELECT status,metadata,revision FROM agents WHERE id=?")
            .bind(agent_id)
            .fetch_one(agent_view.agent_pool)
            .await?;
        let metadata = self.unseal_agent(&agent.get::<Vec<u8>, _>("metadata"))?;
        if metadata["owner_id"].as_str() != Some(owner.as_str())
            && !(agent_id == "shared_default" && owner == "admin" && metadata["owner_id"].is_null())
        {
            return Err(anyhow!("Agent owner conflict"));
        }
        let raw = self.agent_membership(user_id, agent_id).await?;
        let membership =
            crate::membership::membership_from(user_id, agent_id, &owner, raw.clone(), || {
                Ok((
                    principal.clone(),
                    crate::membership::MemberAccess::from_metadata(&metadata)?,
                ))
            })?;
        if let Some(value) = &membership {
            if value["user_id"] != user_id
                || value["agent_id"] != agent_id
                || value["revision"]
                    .as_u64()
                    .is_none_or(|revision| revision == 0)
            {
                return Err(anyhow!("Membership identity or revision conflict"));
            }
        }
        let relationship = match membership
            .as_ref()
            .and_then(|value| value["relationship"].as_str())
        {
            Some(value) => serde_json::from_value(Value::String(value.into()))?,
            None => Relationship::Revoked,
        };
        let membership_revision = membership
            .as_ref()
            .and_then(|value| value["revision"].as_u64())
            .unwrap_or(0);
        let agent = Some(agent);
        let (
            agent_enabled,
            agent_revision,
            tool_permissions,
            ability_permissions,
            tool_callers,
            owner,
        ) = if let Some(agent) = agent {
            if !metadata.is_object() {
                return Err(anyhow!("Invalid agent metadata"));
            }
            let owner = owner.clone();
            if (agent_id == "shared_default" && owner != "admin")
                || metadata["owner_id"].as_str().is_some_and(|id| id != owner)
                || (agent_id != "shared_default"
                    && metadata["owner_id"].as_str() != Some(owner.as_str()))
            {
                return Err(anyhow!("Agent owner conflict"));
            }
            let permissions = metadata
                .get("tool_permissions")
                .cloned()
                .unwrap_or(Value::Null);
            permission_map(&permissions)?;
            let abilities = metadata
                .get("ability_permissions")
                .cloned()
                .unwrap_or(Value::Null);
            permission_map(&abilities)?;
            let callers = metadata.get("tool_callers").cloned().unwrap_or(Value::Null);
            caller_map(&callers)?;
            (
                agent.get::<String, _>("status") == "active",
                agent.get::<i64, _>("revision").max(0) as u64,
                permissions,
                abilities,
                callers,
                Some(owner),
            )
        } else {
            (false, 0, Value::Null, Value::Null, Value::Null, None)
        };
        let owner_id = owner.clone().unwrap_or_default();
        // A missing owner account leaves no tier, so no ability works.
        let owner_tier_id = match owner {
            Some(owner) if owner == principal.user_id => principal.tier_id.clone(),
            Some(owner) => match self.account_by_id(&owner).await? {
                Some(account) => Principal::from_account(&account)?.tier_id,
                None => String::new(),
            },
            None => String::new(),
        };
        let (owner_tiers, owner_tier) = if owner_tier_id.is_empty() {
            (Vec::new(), None)
        } else if owner_tier_id == principal.tier_id {
            (
                crate::account_ladder::inherited(&policy, &owner_tier_id),
                Some(tier.clone()),
            )
        } else {
            (
                crate::account_ladder::inherited(&policy, &owner_tier_id),
                crate::account_ladder::effective(&policy, &owner_tier_id)
                    .ok()
                    .filter(|owner| owner.validate().is_ok()),
            )
        };
        // `agent_authority`'s checks of the stored membership, then its `can_manage`.
        if raw.as_ref().is_some_and(|v| {
            v["user_id"] != user_id
                || v["agent_id"] != agent_id
                || v["revision"].as_u64().is_none_or(|r| r == 0)
        }) {
            return Err(anyhow!("Invalid membership record"));
        }
        raw.as_ref()
            .map(|v| serde_json::from_value::<Relationship>(v["relationship"].clone()))
            .transpose()?;
        let agent_operator = owner_id == user_id
            || raw
                .as_ref()
                .is_some_and(|v| v["agent_administrator"] == true && v["relationship"] == "member");
        let agent_administrator = principal.active() && agent_operator;
        let levels = crate::capabilities::CapabilityLevels::from_metadata(&metadata)?;
        let level_id = crate::capabilities::membership_level(membership.as_ref(), relationship)?;
        let capability_level = levels.effective(&level_id)?;
        let revision = AccessRevision {
            account: principal.access_revision,
            tier: tier.revision,
            membership: membership_revision,
            agent: agent_revision,
            policy: policy_digest,
            capability_level: levels.revision,
        };
        let agent_enabled = agent_enabled
            && crate::configuration::page_allowed(&policy, "main", "chat", &principal);
        Ok(LiveAccess {
            principal,
            tier,
            agent_id: agent_id.to_owned(),
            relationship,
            agent_enabled,
            agent_administrator,
            agent_owner_id: owner_id,
            owner_tier_id,
            owner_tier,
            owner_tiers,
            revision,
            capability_level,
            run_limits,
            tool_permissions,
            ability_permissions,
            tool_callers,
        })
    }
}

impl<C: RecordCodec> Repository<'_, C> {
    pub async fn set_agent_permissions(
        &self,
        agent: &str,
        expected: u64,
        abilities: Value,
        tools: Value,
    ) -> Result<Value> {
        validate_set_agent_permissions(agent, expected, &abilities, &tools)?;
        let view = self.for_agent(agent).await?;
        let mut tx = view.agent_pool.begin().await?;
        sqlx::query("UPDATE agents SET revision=revision WHERE id=?")
            .bind(agent)
            .execute(&mut *tx)
            .await?;
        let row = sqlx::query("SELECT metadata,revision FROM agents WHERE id=?")
            .bind(agent)
            .fetch_one(&mut *tx)
            .await?;
        if row.get::<i64, _>("revision") as u64 != expected {
            return Err(anyhow!("Agent revision changed"));
        }
        let mut metadata = self.unseal_agent(&row.get::<Vec<u8>, _>("metadata"))?;
        metadata["ability_permissions"] = abilities.clone();
        metadata["tool_permissions"] = tools.clone();
        sqlx::query("UPDATE agents SET metadata=?,revision=revision+1,updated_at=strftime('%s','now') WHERE id=?")
            .bind(self.seal_agent(&metadata)?).bind(agent).execute(&mut *tx).await?;
        let receipt = serde_json::json!({"agent_id":agent,"actor_id":self.current_user_id(),"revision":expected+1});
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(format!(
                "agent:{agent}:ability_policy_audit:{}",
                expected + 1
            ))
            .bind(self.seal_agent(&receipt)?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(
            serde_json::json!({"revision":expected+1,"ability_permissions":abilities,"tool_permissions":tools}),
        )
    }
    pub async fn agent_tool_permissions(&self, agent_id: &str) -> Result<Value> {
        if !access::valid_agent_id(agent_id) {
            return Err(anyhow!("Invalid agent identity"));
        }
        let view = self.for_agent(agent_id).await?;
        let row = sqlx::query("SELECT metadata,revision FROM agents WHERE id=?")
            .bind(agent_id)
            .fetch_optional(view.agent_pool)
            .await?
            .ok_or_else(|| anyhow!("Agent not found"))?;
        let metadata = self.unseal_agent(&row.get::<Vec<u8>, _>("metadata"))?;
        Ok(serde_json::json!({"agent_id":agent_id,
            "revision":row.get::<i64,_>("revision"),
            "tool_permissions":metadata.get("tool_permissions")
                .cloned().unwrap_or_else(||serde_json::json!({}))}))
    }

    pub async fn set_agent_tool_permissions(
        &self,
        agent_id: &str,
        expected_revision: u64,
        permissions: std::collections::BTreeMap<String, Permission>,
    ) -> Result<Value> {
        validate_set_agent_tool_permissions(agent_id, expected_revision, &permissions)?;
        let view = self.for_agent(agent_id).await?;
        // Read-then-write: take the write lock first, so a concurrent writer makes this wait
        // (busy_timeout) instead of failing with 'database is locked'.
        let mut tx = view.agent_pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query("SELECT metadata,revision FROM agents WHERE id=?")
            .bind(agent_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| anyhow!("Agent not found"))?;
        if row.get::<i64, _>("revision") as u64 != expected_revision {
            return Err(anyhow!("Agent revision changed"));
        }
        let mut metadata = self.unseal_agent(&row.get::<Vec<u8>, _>("metadata"))?;
        if !metadata.is_object() {
            return Err(anyhow!("Invalid agent metadata"));
        }
        metadata["tool_permissions"] = serde_json::to_value(&permissions)?;
        let updated = sqlx::query(
            "UPDATE agents SET metadata=?,revision=revision+1,
            updated_at=strftime('%s','now') WHERE id=? AND revision=?",
        )
        .bind(self.seal_agent(&metadata)?)
        .bind(agent_id)
        .bind(expected_revision as i64)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(anyhow!("Agent revision changed"));
        }
        tx.commit().await?;
        self.event(
            "",
            "agent_tool_permissions_changed",
            &serde_json::json!({
            "agent_id":agent_id,"revision":expected_revision+1}),
        )
        .await?;
        Ok(
            serde_json::json!({"agent_id":agent_id,"revision":expected_revision+1,
            "tool_permissions":permissions}),
        )
    }
    pub async fn agent_ability_permissions(&self, agent_id: &str) -> Result<Value> {
        let view = self.for_agent(agent_id).await?;
        let row = sqlx::query("SELECT metadata,revision FROM agents WHERE id=?")
            .bind(agent_id)
            .fetch_optional(view.agent_pool)
            .await?
            .ok_or_else(|| anyhow!("Agent not found"))?;
        let metadata = self.unseal_agent(&row.get::<Vec<u8>, _>("metadata"))?;
        Ok(serde_json::json!({"agent_id":agent_id,
            "revision":row.get::<i64,_>("revision"),
            "ability_permissions":metadata.get("ability_permissions")
                .cloned().unwrap_or_else(||serde_json::json!({}))}))
    }

    pub async fn set_agent_ability_permissions(
        &self,
        agent_id: &str,
        expected_revision: u64,
        permissions: std::collections::BTreeMap<String, Permission>,
    ) -> Result<Value> {
        validate_set_agent_ability_permissions(agent_id, expected_revision, &permissions)?;
        let view = self.for_agent(agent_id).await?;
        // Read-then-write: take the write lock first, so a concurrent writer makes this wait
        // (busy_timeout) instead of failing with 'database is locked'.
        let mut tx = view.agent_pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query("SELECT metadata,revision FROM agents WHERE id=?")
            .bind(agent_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| anyhow!("Agent not found"))?;
        if row.get::<i64, _>("revision") as u64 != expected_revision {
            return Err(anyhow!("Agent revision changed"));
        }
        let mut metadata = self.unseal_agent(&row.get::<Vec<u8>, _>("metadata"))?;
        if !metadata.is_object() {
            return Err(anyhow!("Invalid agent metadata"));
        }
        metadata["ability_permissions"] = serde_json::to_value(&permissions)?;
        let updated = sqlx::query(
            "UPDATE agents SET metadata=?,revision=revision+1,
            updated_at=strftime('%s','now') WHERE id=? AND revision=?",
        )
        .bind(self.seal_agent(&metadata)?)
        .bind(agent_id)
        .bind(expected_revision as i64)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(anyhow!("Agent revision changed"));
        }
        tx.commit().await?;
        self.event(
            "",
            "agent_ability_permissions_changed",
            &serde_json::json!({
            "agent_id":agent_id,"revision":expected_revision+1}),
        )
        .await?;
        Ok(
            serde_json::json!({"agent_id":agent_id,"revision":expected_revision+1,
            "ability_permissions":permissions}),
        )
    }

    pub async fn agent_tool_callers(&self, agent_id: &str) -> Result<Value> {
        let view = self.for_agent(agent_id).await?;
        let row = sqlx::query("SELECT metadata,revision FROM agents WHERE id=?")
            .bind(agent_id)
            .fetch_optional(view.agent_pool)
            .await?
            .ok_or_else(|| anyhow!("Agent not found"))?;
        let metadata = self.unseal_agent(&row.get::<Vec<u8>, _>("metadata"))?;
        Ok(serde_json::json!({"agent_id":agent_id,
            "revision":row.get::<i64,_>("revision"),
            "tool_callers":metadata.get("tool_callers")
                .cloned().unwrap_or_else(||serde_json::json!({}))}))
    }

    pub async fn set_agent_tool_callers(
        &self,
        agent_id: &str,
        expected_revision: u64,
        callers: std::collections::BTreeMap<String, Vec<Relationship>>,
    ) -> Result<Value> {
        validate_set_agent_tool_callers(agent_id, expected_revision, &callers)?;
        let view = self.for_agent(agent_id).await?;
        // Read-then-write: take the write lock first, so a concurrent writer makes this wait
        // (busy_timeout) instead of failing with 'database is locked'.
        let mut tx = view.agent_pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query("SELECT metadata,revision FROM agents WHERE id=?")
            .bind(agent_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| anyhow!("Agent not found"))?;
        if row.get::<i64, _>("revision") as u64 != expected_revision {
            return Err(anyhow!("Agent revision changed"));
        }
        let mut metadata = self.unseal_agent(&row.get::<Vec<u8>, _>("metadata"))?;
        if !metadata.is_object() {
            return Err(anyhow!("Invalid agent metadata"));
        }
        metadata["tool_callers"] = serde_json::to_value(&callers)?;
        let updated = sqlx::query(
            "UPDATE agents SET metadata=?,revision=revision+1,
            updated_at=strftime('%s','now') WHERE id=? AND revision=?",
        )
        .bind(self.seal_agent(&metadata)?)
        .bind(agent_id)
        .bind(expected_revision as i64)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(anyhow!("Agent revision changed"));
        }
        tx.commit().await?;
        self.event(
            "",
            "agent_tool_callers_changed",
            &serde_json::json!({
            "agent_id":agent_id,"revision":expected_revision+1}),
        )
        .await?;
        Ok(
            serde_json::json!({"agent_id":agent_id,"revision":expected_revision+1,
            "tool_callers":callers}),
        )
    }
}

pub fn validate_set_agent_tool_callers(
    agent_id: &str,
    expected_revision: u64,
    callers: &std::collections::BTreeMap<String, Vec<Relationship>>,
) -> Result<()> {
    if expected_revision >= i64::MAX as u64
        || callers.len() > 128
        || callers.iter().any(|(name, roles)| {
            (name != "*" && !access::valid_id(name))
                || roles.len() > 3
                || roles.iter().any(|role| *role == Relationship::Revoked)
                || roles
                    .iter()
                    .enumerate()
                    .any(|(i, role)| roles[..i].contains(role))
        })
    {
        return Err(anyhow!("Invalid agent tool caller rules"));
    }
    Ok(())
}

pub fn validate_set_agent_ability_permissions(
    agent_id: &str,
    expected_revision: u64,
    permissions: &std::collections::BTreeMap<String, Permission>,
) -> Result<()> {
    if expected_revision >= i64::MAX as u64
        || permissions.len() > 128
        || permissions
            .keys()
            .any(|id| id != "*" && !access::valid_id(id))
    {
        return Err(anyhow!("Invalid agent ability permissions"));
    }
    Ok(())
}

pub fn validate_set_agent_tool_permissions(
    agent_id: &str,
    expected_revision: u64,
    permissions: &std::collections::BTreeMap<String, Permission>,
) -> Result<()> {
    if !access::valid_agent_id(agent_id)
        || expected_revision >= i64::MAX as u64
        || permissions.len() > 128
        || permissions
            .keys()
            .any(|name| name != "*" && !access::valid_id(name))
    {
        return Err(anyhow!("Invalid agent permission update"));
    }
    Ok(())
}

pub fn validate_set_agent_permissions(
    agent: &str,
    expected: u64,
    abilities: &Value,
    tools: &Value,
) -> Result<()> {
    permission_map(&abilities)?;
    permission_map(&tools)?;
    if !abilities.is_object() || !tools.is_object() || expected >= i64::MAX as u64 {
        return Err(anyhow!("Invalid permission maps"));
    }
    Ok(())
}

pub fn validate_live_access(user_id: &str, agent_id: &str) -> Result<()> {
    if !access::valid_id(user_id) || !access::valid_agent_id(agent_id) {
        return Err(anyhow!("Invalid access identity"));
    }
    Ok(())
}
