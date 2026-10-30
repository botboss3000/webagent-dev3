//! Phase 3 access decisions. Visibility and availability never grant permission.
pub mod configuration;

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub use webagent_accounts::AccountStatus;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Guest,
    User,
    Administrator,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relationship {
    Revoked,
    Owner,
    Member,
    Caller,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Deny,
    Ask,
    Auto,
}

/// The built-in Base ability: its tools are compiled in and need no capability-level
/// ability grant; their coded role defaults and any saved tool permission decide.
pub const BUILT_IN_ABILITY: &str = "base";

impl Permission {
    pub fn from_policy(value: &str) -> Result<Self> {
        match value {
            "deny" => Ok(Self::Deny),
            "require_approval" | "ask" => Ok(Self::Ask),
            "allow" | "auto" => Ok(Self::Auto),
            _ => bail!("Invalid permission"),
        }
    }
    pub fn meet(self, other: Self) -> Self {
        use Permission::*;
        match (self, other) {
            (Deny, _) | (_, Deny) => Deny,
            (Ask, _) | (_, Ask) => Ask,
            _ => Auto,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelPolicy {
    pub roster_id: Option<String>,
    pub allowed_entry_ids: std::collections::BTreeSet<String>,
    pub allow_byo: bool,
    pub max_byo_entries: u32,
}
/// The entitlement group of an ability, or of a tool's ability, from its group's
/// `_group.json`. The app installs these from the ability catalog.
pub type Entitlement = fn(&str) -> Option<&'static str>;
static ABILITY_ENTITLEMENT: std::sync::OnceLock<Entitlement> = std::sync::OnceLock::new();
static TOOL_ENTITLEMENT: std::sync::OnceLock<Entitlement> = std::sync::OnceLock::new();
pub fn install_entitlements(ability: Entitlement, tool: Entitlement) {
    let _ = ABILITY_ENTITLEMENT.set(ability);
    let _ = TOOL_ENTITLEMENT.set(tool);
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tier {
    pub id: String,
    pub revision: u64,
    pub allowed_models: Vec<String>,
    #[serde(default)]
    pub model_policy: Option<ModelPolicy>,
    pub tools: BTreeMap<String, Permission>,
    // Fail closed: a tier without ability_permissions grants no abilities.
    #[serde(default)]
    pub ability_permissions: BTreeMap<String, Permission>,
    /// Grants by entitlement group. An ability or tool the tier does not name inherits its
    /// group's grant, so a new ability in a granted group works at once.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub entitlements: BTreeMap<String, Permission>,
    pub max_sessions: u32,
    pub max_active_runs: u32,
    /// Set by `account_ladder::effective`: this tier's own grants, then each tier above it.
    #[serde(skip)]
    pub chain: Vec<TierGrants>,
}

/// One tier's own grants, resolved alone and then met across the account ladder.
#[derive(Clone, Debug, Default)]
pub struct TierGrants {
    abilities: BTreeMap<String, Permission>,
    tools: BTreeMap<String, Permission>,
    entitlements: BTreeMap<String, Permission>,
}

impl TierGrants {
    #[must_use]
    pub fn of(tier: &Tier) -> Self {
        Self {
            abilities: tier.ability_permissions.clone(),
            tools: tier.tools.clone(),
            entitlements: tier.entitlements.clone(),
        }
    }

    fn named(&self, tool: bool) -> &BTreeMap<String, Permission> {
        if tool {
            &self.tools
        } else {
            &self.abilities
        }
    }

    /// Every name any tier grants, resolved in each tier (named, wildcard, then group) and
    /// met across `chain`, plus the wildcard met alone, as an effective tier's named grants.
    pub(crate) fn merge(chain: &[Self], tool: bool) -> BTreeMap<String, Permission> {
        let hook = if tool {
            &TOOL_ENTITLEMENT
        } else {
            &ABILITY_ENTITLEMENT
        };
        let names: std::collections::BTreeSet<&String> = chain
            .iter()
            .flat_map(|grants| grants.named(tool).keys())
            .filter(|name| name.as_str() != "*")
            .collect();
        let mut merged: BTreeMap<String, Permission> = names
            .into_iter()
            .map(|name| {
                let permission = chain.iter().fold(Permission::Auto, |permission, grants| {
                    permission.meet(tier_grant(
                        grants.named(tool),
                        &grants.entitlements,
                        name,
                        hook,
                    ))
                });
                (name.clone(), permission)
            })
            .collect();
        let wildcard = chain.iter().fold(Permission::Auto, |permission, grants| {
            permission.meet(
                grants
                    .named(tool)
                    .get("*")
                    .copied()
                    .unwrap_or(Permission::Deny),
            )
        });
        merged.insert("*".into(), wildcard);
        merged
    }
}

/// A named grant, else the tier's wildcard, else the grant for the item's entitlement group.
fn tier_grant(
    named: &BTreeMap<String, Permission>,
    entitlements: &BTreeMap<String, Permission>,
    id: &str,
    group: &std::sync::OnceLock<Entitlement>,
) -> Permission {
    named
        .get(id)
        .or_else(|| named.get("*"))
        .copied()
        .or_else(|| {
            group
                .get()
                .and_then(|group| group(id))
                .and_then(|group| entitlements.get(group).copied())
        })
        .unwrap_or(Permission::Deny)
}
impl Tier {
    pub fn validate(&self) -> Result<()> {
        if let Some(policy) = &self.model_policy {
            if policy.roster_id.as_ref().is_some_and(|id| !valid_id(id))
                || policy.allowed_entry_ids.len() > 256
                || policy.allowed_entry_ids.iter().any(|id| !valid_id(id))
                || policy.max_byo_entries > 256
                || (policy.roster_id.is_none() && !policy.allowed_entry_ids.is_empty())
            {
                bail!("Invalid model policy");
            }
        }
        if !valid_id(&self.id)
            || self.revision == 0
            || self.allowed_models.len() > 64
            || self.tools.len() > 128
            || self.ability_permissions.len() > 128
            || self.entitlements.len() > 64
            || self.max_sessions > 10_000
            || self.max_active_runs > 200
        {
            bail!("Invalid entitlement tier");
        }
        if self
            .allowed_models
            .iter()
            .any(|model| model != "*" && !valid_model(model))
            || self.tools.keys().any(|tool| tool != "*" && !valid_id(tool))
            || self
                .ability_permissions
                .keys()
                .any(|id| id != "*" && !valid_id(id))
            || self.entitlements.keys().any(|group| !valid_id(group))
        {
            bail!("Invalid entitlement grant");
        }
        Ok(())
    }
    /// Legacy/BYO lane only. Platform access must use stable published identity.
    pub fn allows_byo_model(&self, model: &str) -> bool {
        self.model_policy
            .as_ref()
            .is_none_or(|policy| policy.allow_byo)
            && self.allows_model(model)
    }
    /// Downgrades must permit removals/revocations, never new over-quota entries.
    pub fn allows_byo_growth(&self, previous: usize, next: usize) -> bool {
        if next <= previous {
            return true;
        }
        self.model_policy
            .as_ref()
            .is_none_or(|policy| policy.allow_byo && next <= policy.max_byo_entries as usize)
    }
    pub fn allows_model(&self, model: &str) -> bool {
        self.allowed_models
            .iter()
            .any(|item| item == "*" || item == model)
    }
    pub fn ability_permission(&self, id: &str) -> Permission {
        self.grant(&self.ability_permissions, id, false)
    }
    pub fn tool_permission(&self, name: &str) -> Permission {
        self.grant(&self.tools, name, true)
    }
    /// Alone: named, wildcard, then entitlement group. In a ladder, named grants were met
    /// across it by `effective` (later edits to them still count); anything unnamed
    /// resolves in each tier and is met.
    fn grant(&self, named: &BTreeMap<String, Permission>, id: &str, tool: bool) -> Permission {
        let hook = if tool {
            &TOOL_ENTITLEMENT
        } else {
            &ABILITY_ENTITLEMENT
        };
        if self.chain.is_empty() {
            return tier_grant(named, &self.entitlements, id, hook);
        }
        if let Some(permission) = named.get(id) {
            return *permission;
        }
        self.chain
            .iter()
            .fold(Permission::Auto, |permission, grants| {
                permission.meet(tier_grant(
                    grants.named(tool),
                    &grants.entitlements,
                    id,
                    hook,
                ))
            })
    }
}
fn valid_model(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':' | b'/'))
}
pub fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
pub fn valid_agent_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

#[derive(Clone, Debug)]
pub struct Principal {
    pub user_id: String,
    pub status: AccountStatus,
    pub role: Role,
    pub tier_id: String,
    pub access_revision: u64,
}
impl Principal {
    /// Absent fields are allowed only for the original administrator record.
    pub fn from_account(account: &Value) -> Result<Self> {
        let user_id = account["user_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Account has no stable identity"))?;
        if !valid_id(user_id) {
            bail!("Invalid account identity");
        }
        let legacy_admin = user_id == "admin";
        let status = webagent_accounts::account_status(account)?;
        let role = match account["role"].as_str() {
            Some(value) => serde_json::from_value(Value::String(value.into()))?,
            None if legacy_admin => Role::Administrator,
            _ => bail!("Account has no role"),
        };
        let tier_id = match account["tier_id"].as_str() {
            Some(value) if valid_id(value) => value.to_owned(),
            None if legacy_admin => "operator".into(),
            _ => bail!("Account has no valid tier"),
        };
        let access_revision = match account["access_revision"].as_u64() {
            Some(value) if value > 0 => value,
            None if legacy_admin => 1,
            _ => bail!("Account has no access revision"),
        };
        Ok(Self {
            user_id: user_id.into(),
            status,
            role,
            tier_id,
            access_revision,
        })
    }
    pub fn active(&self) -> bool {
        self.status == AccountStatus::Active
    }
    pub fn administrator(&self) -> bool {
        self.active() && self.role == Role::Administrator
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ToolFactors {
    pub installation: Permission,
    pub tier: Permission,
    pub agent: Permission,
    pub ability: Permission,
    pub account_active: bool,
    pub relationship: bool,
    pub agent_enabled: bool,
    pub available: bool,
    pub visible: bool,
    pub read_only: bool,
    pub is_read: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Allowed,
    ApprovalRequired,
    PolicyDenied,
    TierDenied,
    AgentDenied,
    AbilityDenied,
    AccountInactive,
    CallerDenied,
    AgentDisabled,
    CapabilityDenied,
    Unavailable,
    Hidden,
    ReadOnly,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ToolDecision {
    pub permission: Permission,
    pub reason: Reason,
}
impl ToolFactors {
    pub fn resolve(self) -> ToolDecision {
        use Permission::*;
        let denial = if self.installation == Deny {
            Some(Reason::PolicyDenied)
        } else if self.tier == Deny {
            Some(Reason::TierDenied)
        } else if self.agent == Deny {
            Some(Reason::AgentDenied)
        } else if self.ability == Deny {
            Some(Reason::AbilityDenied)
        } else if !self.account_active {
            Some(Reason::AccountInactive)
        } else if !self.relationship {
            Some(Reason::CallerDenied)
        } else if !self.agent_enabled {
            Some(Reason::AgentDisabled)
        } else if !self.available {
            Some(Reason::Unavailable)
        } else if !self.visible {
            Some(Reason::Hidden)
        } else if self.read_only && !self.is_read {
            Some(Reason::ReadOnly)
        } else {
            None
        };
        if let Some(reason) = denial {
            return ToolDecision {
                permission: Deny,
                reason,
            };
        }
        let permission = self
            .installation
            .meet(self.tier)
            .meet(self.agent)
            .meet(self.ability);
        ToolDecision {
            permission,
            reason: if permission == Ask {
                Reason::ApprovalRequired
            } else {
                Reason::Allowed
            },
        }
    }
}
impl ToolDecision {
    /// Auto waives a human tool checkpoint after every independent denial has
    /// been resolved. Legacy write mode and read-only modes keep Ask semantics.
    pub fn for_execution_mode(self, mode: &Value) -> Self {
        if mode["mode"] == "write"
            && mode["execution_mode"] == "auto"
            && self.permission == Permission::Ask
        {
            Self {
                permission: Permission::Auto,
                reason: Reason::Allowed,
            }
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn legacy_admin_is_active_but_new_accounts_need_explicit_access_fields() {
        let admin = Principal::from_account(&json!({"user_id":"admin"})).unwrap();
        assert!(admin.administrator());
        assert_eq!(admin.tier_id, "operator");
        assert!(Principal::from_account(&json!({"user_id":"second"})).is_err());
        let suspended = Principal::from_account(&json!({"user_id":"second",
            "status":"suspended","role":"administrator","tier_id":"operator",
            "access_revision":2}))
        .unwrap();
        assert!(!suspended.administrator());
    }
    #[test]
    fn strictest_permission_and_independent_denials_win() {
        let base = ToolFactors {
            installation: Permission::Auto,
            tier: Permission::Auto,
            agent: Permission::Auto,
            ability: Permission::Auto,
            account_active: true,
            relationship: true,
            agent_enabled: true,
            available: true,
            visible: true,
            read_only: false,
            is_read: false,
        };
        assert_eq!(base.resolve().permission, Permission::Auto);
        assert_eq!(
            ToolFactors {
                tier: Permission::Ask,
                ..base
            }
            .resolve()
            .reason,
            Reason::ApprovalRequired
        );
        assert_eq!(
            ToolFactors {
                tier: Permission::Deny,
                ..base
            }
            .resolve()
            .reason,
            Reason::TierDenied
        );
        assert_eq!(
            ToolFactors {
                ability: Permission::Deny,
                ..base
            }
            .resolve()
            .reason,
            Reason::AbilityDenied
        );
        assert_eq!(
            ToolFactors {
                ability: Permission::Ask,
                ..base
            }
            .resolve()
            .reason,
            Reason::ApprovalRequired
        );
        assert_eq!(
            ToolFactors {
                account_active: false,
                ..base
            }
            .resolve()
            .reason,
            Reason::AccountInactive
        );
        assert_eq!(
            ToolFactors {
                relationship: false,
                ..base
            }
            .resolve()
            .reason,
            Reason::CallerDenied
        );
        assert_eq!(
            ToolFactors {
                available: false,
                ..base
            }
            .resolve()
            .reason,
            Reason::Unavailable
        );
        assert_eq!(
            ToolFactors {
                visible: false,
                ..base
            }
            .resolve()
            .reason,
            Reason::Hidden
        );
        assert_eq!(
            ToolFactors {
                read_only: true,
                ..base
            }
            .resolve()
            .reason,
            Reason::ReadOnly
        );
        assert_eq!(
            ToolFactors {
                read_only: true,
                is_read: true,
                ..base
            }
            .resolve()
            .permission,
            Permission::Auto
        );
    }
    #[test]
    fn auto_mode_skips_only_approval_and_keeps_denials() {
        let ask = ToolDecision {
            permission: Permission::Ask,
            reason: Reason::ApprovalRequired,
        };
        let denied = ToolDecision {
            permission: Permission::Deny,
            reason: Reason::TierDenied,
        };
        let auto = json!({"mode":"write","execution_mode":"auto"});
        assert_eq!(ask.for_execution_mode(&auto).permission, Permission::Auto);
        assert_eq!(ask.for_execution_mode(&auto).reason, Reason::Allowed);
        assert_eq!(denied.for_execution_mode(&auto), denied);
        assert_eq!(
            ask.for_execution_mode(&json!({"mode":"read_only","execution_mode":"ask"})),
            ask
        );
        assert_eq!(
            ask.for_execution_mode(&json!({"mode":"read_only","execution_mode":"auto"})),
            ask
        );
        assert_eq!(ask.for_execution_mode(&json!({"mode":"write"})), ask);
    }
    #[test]
    fn byo_downgrade_blocks_use_and_growth_but_allows_reduction() {
        let mut tier = Tier {
            id: "basic".into(),
            revision: 1,
            model_policy: None,
            allowed_models: vec!["*".into()],
            tools: BTreeMap::new(),
            ability_permissions: [("web_access".into(), Permission::Auto)].into(),
            entitlements: BTreeMap::new(),
            max_sessions: 10,
            max_active_runs: 2,
            chain: Vec::new(),
        };
        assert!(tier.allows_byo_model("deepseek-chat"));
        assert!(tier.allows_byo_growth(0, 1));
        tier.model_policy = Some(ModelPolicy {
            roster_id: None,
            allowed_entry_ids: Default::default(),
            allow_byo: false,
            max_byo_entries: 0,
        });
        assert!(!tier.allows_byo_model("deepseek-chat"));
        assert!(!tier.allows_byo_growth(0, 1));
        assert!(tier.allows_byo_growth(3, 3));
        assert!(tier.allows_byo_growth(3, 2));
        let policy = tier.model_policy.as_mut().unwrap();
        policy.allow_byo = true;
        policy.max_byo_entries = 2;
        assert!(tier.allows_byo_growth(1, 2));
        assert!(!tier.allows_byo_growth(2, 3));
        assert!(!tier.allows_byo_growth(3, 4));
        tier.allowed_models = vec!["deepseek-chat".into()];
        assert!(!tier.allows_byo_model("other"));
        assert!(tier.allows_byo_model("deepseek-chat"));
    }
    #[test]
    fn tier_is_explicit_and_bounded() {
        let mut tier = Tier {
            id: "basic".into(),
            revision: 1,
            model_policy: None,
            allowed_models: vec!["gpt-6-luna".into()],
            tools: BTreeMap::new(),
            ability_permissions: [("web_access".into(), Permission::Auto)].into(),
            entitlements: BTreeMap::new(),
            max_sessions: 10,
            max_active_runs: 2,
            chain: Vec::new(),
        };
        tier.validate().unwrap();
        assert!(!tier.allows_model("other"));
        assert_eq!(tier.tool_permission("file_write"), Permission::Deny);
        tier.tools.insert("file_write".into(), Permission::Ask);
        assert_eq!(tier.tool_permission("file_write"), Permission::Ask);
        tier.max_active_runs = 201;
        assert!(tier.validate().is_err());
    }
    #[test]
    fn tier_without_ability_permissions_grants_no_abilities() {
        let tier: Tier =
            serde_json::from_value(json!({"id":"basic","revision":1,"allowed_models":["*"],
            "tools":{"*":"auto"},"max_sessions":10,"max_active_runs":2}))
            .unwrap();
        assert!(tier.ability_permissions.is_empty());
        for id in ["codebase_admin", "web_access", "*"] {
            assert_eq!(tier.ability_permission(id), Permission::Deny);
        }
    }
}

pub mod authority;
pub mod capabilities;
pub mod credential_authorization;
pub mod policy_file;
pub mod repository;
pub mod storage;
pub fn require_administrator(account: &serde_json::Value) -> anyhow::Result<()> {
    if !Principal::from_account(account)?.administrator() {
        anyhow::bail!("Administrator access required");
    }
    Ok(())
}
pub fn validate_self_administration(
    actor: &str,
    target: &str,
    status: AccountStatus,
    role: Role,
) -> anyhow::Result<()> {
    if actor == target && (status != AccountStatus::Active || role != Role::Administrator) {
        anyhow::bail!("You cannot restrict or demote your own administrator account");
    }
    Ok(())
}

impl ModelPolicy {
    /// The base catalog is installation administration, never a tier roster grant.
    pub fn tier_roster_id(&self) -> Result<Option<&str>> {
        if self.roster_id.as_deref() == Some("base") {
            bail!("Base catalog is administrator-only");
        }
        Ok(self.roster_id.as_deref())
    }
    /// Models supplies its existing published snapshot; Access owns grant membership.
    pub fn validate_published_entries(&self, contains: impl Fn(&str) -> bool) -> Result<()> {
        if self.allowed_entry_ids.iter().any(|id| !contains(id)) {
            bail!("Catalog entry not published");
        }
        Ok(())
    }
}
#[cfg(test)]
mod catalog_authority_tests {
    use super::*;
    #[test]
    fn tier_grants_require_non_base_published_entry_identity() {
        let mut policy:ModelPolicy=serde_json::from_value(serde_json::json!({"roster_id":"base","allowed_entry_ids":[],"allow_byo":false,"max_byo_entries":0})).unwrap();
        assert_eq!(
            policy.tier_roster_id().unwrap_err().to_string(),
            "Base catalog is administrator-only"
        );
        policy.roster_id = Some("team".into());
        policy.allowed_entry_ids = ["published".into()].into_iter().collect();
        assert_eq!(policy.tier_roster_id().unwrap(), Some("team"));
        assert!(policy
            .validate_published_entries(|id| id == "published")
            .is_ok());
        assert_eq!(
            policy
                .validate_published_entries(|_| false)
                .unwrap_err()
                .to_string(),
            "Catalog entry not published"
        );
    }
}

#[cfg(test)]
mod ladder_tests;

pub mod membership;

pub mod account_ladder;

/// Current installation defaults are authorization assignments, not credential policy.
pub const fn initial_user_access() -> webagent_accounts::InitialAccess<'static> {
    webagent_accounts::InitialAccess {
        role: "user",
        tier_id: "basic",
        profile_is_admin: false,
    }
}
pub const fn initial_guest_access() -> webagent_accounts::InitialAccess<'static> {
    webagent_accounts::InitialAccess {
        role: "guest",
        tier_id: "anonymous",
        profile_is_admin: false,
    }
}
pub const fn initial_admin_access() -> webagent_accounts::InitialAccess<'static> {
    webagent_accounts::InitialAccess {
        role: "administrator",
        tier_id: "operator",
        profile_is_admin: true,
    }
}
