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
    pub revision: AccessRevision,
    pub capability_level: crate::capabilities::CapabilityLevel,
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

    pub fn model_allowed(&self, model: &str) -> bool {
        self.admitted() && self.tier.allows_byo_model(model)
    }

    pub fn max_sessions(&self) -> u32 {
        self.tier.max_sessions
    }
    pub fn max_active_runs(&self) -> u32 {
        self.tier.max_active_runs
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
        if !self.capability_level.ability_enabled(ability_id) {
            return Permission::Deny;
        }
        self.base_ability_permission(ability_id)
            .meet(self.tier.ability_permission(ability_id))
            .meet(self.level_ability_permission(ability_id))
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

    /// `available` must come from the installed tool catalog; `visible`
    /// must come from the agent and session visibility state. Neither grants
    /// access over a Deny in the operator, tier, or agent permission plane.
    pub fn tool_decision(
        &self,
        name: &str,
        installation: Permission,
        available: bool,
        visible: bool,
        read_only: bool,
        is_read: bool,
    ) -> ToolDecision {
        self.resolve_tool(
            name,
            installation,
            Permission::Auto,
            available,
            visible,
            read_only,
            is_read,
        )
    }

    pub fn tool_decision_for_abilities(
        &self,
        name: &str,
        ability_ids: &[String],
        installation: Permission,
        available: bool,
        visible: bool,
        read_only: bool,
        is_read: bool,
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
        let ability = if ability_ids.is_empty() {
            if available {
                Permission::Deny
            } else {
                Permission::Auto
            }
        } else {
            ability_ids.iter().fold(Permission::Auto, |permission, id| {
                permission.meet(self.ability_permission(id))
            })
        };
        // One calculation drives discovery, guidance, schemas, previews and dispatch.
        // Report the narrowing layer without interpreting visibility as permission.
        if installation != Permission::Deny && self.tier.tool_permission(name) != Permission::Deny {
            if ability_ids
                .iter()
                .any(|id| self.tier.ability_permission(id) == Permission::Deny)
            {
                return ToolDecision {
                    permission: Permission::Deny,
                    reason: access::Reason::TierDenied,
                };
            }
            if ability_ids
                .iter()
                .any(|id| self.level_ability_permission(id) == Permission::Deny)
            {
                return ToolDecision {
                    permission: Permission::Deny,
                    reason: access::Reason::CapabilityDenied,
                };
            }
        }
        self.resolve_tool(
            name,
            installation,
            ability,
            available,
            visible,
            read_only,
            is_read,
        )
    }

    fn resolve_tool(
        &self,
        name: &str,
        installation: Permission,
        ability: Permission,
        available: bool,
        visible: bool,
        read_only: bool,
        is_read: bool,
    ) -> ToolDecision {
        if matches!(
            name,
            "build_work"
                | "v3_build_status"
                | "v3_build_worker_restart"
                | "v3_build_image"
                | "v3_worker_submit"
                | "v3_worker_job"
                | "v3_build_work"
                | "v3_source_list"
                | "v3_source_read"
                | "v3_source_prepare"
                | "v3_source_apply"
                | "v3_source_apply_prepared"
                | "v3_source_receipt"
        ) && !self.principal.administrator()
        {
            return ToolDecision {
                permission: Permission::Deny,
                reason: access::Reason::CallerDenied,
            };
        }
        let level = self.capability_level.tool_permission(name);
        if level == Permission::Deny
            && installation != Permission::Deny
            && self.tier.tool_permission(name) != Permission::Deny
            && self.agent_tool_permission(name) != Permission::Deny
        {
            return ToolDecision {
                permission: Permission::Deny,
                reason: access::Reason::CapabilityDenied,
            };
        }
        ToolFactors {
            installation,
            tier: self.tier.tool_permission(name),
            agent: self.agent_tool_permission(name).meet(level),
            ability,
            account_active: self.principal.active(),
            relationship: self.relationship != Relationship::Revoked && self.caller_allowed(name),
            agent_enabled: self.agent_enabled,
            available,
            visible,
            read_only,
            is_read,
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
        let policy = self.read_access_policy()?;
        if principal.role == access::Role::Guest {
            if policy["registration"] != "open" {
                return Err(anyhow!("Guest access is not enabled"));
            }
        }
        let tier = crate::account_ladder::effective(&policy, &principal.tier_id)?;
        tier.validate()?;
        if tier.id != principal.tier_id {
            return Err(anyhow!("Tier identity conflict"));
        }
        let membership = self.resolved_membership(user_id, agent_id).await?;
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
        let agent = sqlx::query("SELECT status,metadata,revision FROM agents WHERE id=?")
            .bind(agent_id)
            .fetch_optional(agent_view.agent_pool)
            .await?;
        let (agent_enabled, agent_revision, tool_permissions, ability_permissions, tool_callers) =
            if let Some(agent) = agent {
                let metadata = self.unseal_agent(&agent.get::<Vec<u8>, _>("metadata"))?;
                if !metadata.is_object() {
                    return Err(anyhow!("Invalid agent metadata"));
                }
                let owner: Option<String> =
                    sqlx::query_scalar("SELECT owner_id FROM agent_directory WHERE id=?")
                        .bind(agent_id)
                        .fetch_optional(self.app_pool)
                        .await?;
                let owner = owner.ok_or_else(|| anyhow!("Agent directory entry missing"))?;
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
                )
            } else {
                (false, 0, Value::Null, Value::Null, Value::Null)
            };
        let agent_administrator =
            self.agent_authority(user_id, agent_id).await?["can_manage"] == true;
        let levels = self.capability_levels(agent_id).await?;
        let level_id = crate::capabilities::membership_level(membership.as_ref(), relationship)?;
        let capability_level = levels.effective(&level_id)?;
        let revision = AccessRevision {
            account: principal.access_revision,
            tier: tier.revision,
            membership: membership_revision,
            agent: agent_revision,
            policy: webagent_configuration::digest(&policy),
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
            revision,
            capability_level,
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
        let mut tx = view.agent_pool.begin().await?;
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
        let mut tx = view.agent_pool.begin().await?;
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
        let mut tx = view.agent_pool.begin().await?;
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
