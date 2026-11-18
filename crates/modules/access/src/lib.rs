//! Phase 3 access decisions. Visibility and availability never grant permission.
pub mod configuration;

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use webagent_accounts::AccountStatus;

/// A refusal whose fixed reason is safe to show the agent: it names a setting, never a
/// destination, identity or authority detail. Other preflight errors stay masked.
#[derive(Debug)]
pub struct PublicRefusal(pub &'static str);

impl std::fmt::Display for PublicRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for PublicRefusal {}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Guest,
    User,
    Administrator,
}

/// The app tier that administers the installation and alone may use Administrator tools.
pub const ADMINISTRATOR_TIER: &str = "administrator";
/// The signed-in tier new accounts start on; paid tiers sit between it and administrator.
pub const FREE_TIER: &str = "free";
/// The tier of visitors who are not signed in; never assigned to a real account.
pub const ANONYMOUS_TIER: &str = "anonymous";

impl Role {
    /// An account's role follows from its tier; it is never set on its own.
    #[must_use]
    pub fn for_tier(tier_id: &str) -> Self {
        match tier_id {
            ADMINISTRATOR_TIER => Self::Administrator,
            ANONYMOUS_TIER => Self::Guest,
            _ => Self::User,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relationship {
    Revoked,
    Owner,
    Member,
    Caller,
}

/// Stored and shown as `allow`, `require_approval` and `deny`; the older `auto` and `ask`
/// are still read so records saved before the rename keep their meaning.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Permission {
    #[serde(rename = "deny")]
    Deny,
    #[serde(rename = "require_approval", alias = "ask")]
    Ask,
    #[serde(rename = "allow", alias = "auto")]
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
/// Whether an ability works on an agent owned on a tier for a caller on a tier
/// (id, owner tier, caller tier), from the ability manifests' `owner_tier`.
pub type AbilityTiers = fn(&str, &str, &str) -> bool;
/// Whether a tool's own `owner_tier` admits an owner tier (name, owner tier); a tool
/// without one inherits its abilities' lists.
pub type ToolTiers = fn(&str, &str) -> bool;
/// Whether an ability is in the Administrator group (id).
pub type AdministratorAbility = fn(&str) -> bool;
static ABILITY_TIERS: std::sync::OnceLock<AbilityTiers> = std::sync::OnceLock::new();
static TOOL_TIERS: std::sync::OnceLock<ToolTiers> = std::sync::OnceLock::new();
static ADMINISTRATOR_ABILITY: std::sync::OnceLock<AdministratorAbility> =
    std::sync::OnceLock::new();
/// The app installs these from the ability catalog, once per process.
pub fn install_tier_access(
    ability: AbilityTiers,
    tool: ToolTiers,
    administrator: AdministratorAbility,
) {
    let _ = ABILITY_TIERS.set(ability);
    let _ = TOOL_TIERS.set(tool);
    let _ = ADMINISTRATOR_ABILITY.set(administrator);
}
/// Whether an ability's `role_access` admits a capability level (id, level rungs).
pub type AbilityRoles = fn(&str, &[String]) -> bool;
/// A tool's own `role_access` for a level (name, rungs); `None` defers to its abilities.
pub type ToolRoles = fn(&str, &[String]) -> Option<bool>;
static ABILITY_ROLES: std::sync::OnceLock<AbilityRoles> = std::sync::OnceLock::new();
static TOOL_ROLES: std::sync::OnceLock<ToolRoles> = std::sync::OnceLock::new();
/// The app installs these from the ability catalog, once per process.
pub fn install_role_access(ability: AbilityRoles, tool: ToolRoles) {
    let _ = ABILITY_ROLES.set(ability);
    let _ = TOOL_ROLES.set(tool);
}
/// Without installed manifests no role limit applies; tiers still fail closed.
#[must_use]
pub fn ability_role_allowed(id: &str, rungs: &[String]) -> bool {
    ABILITY_ROLES.get().is_none_or(|allowed| allowed(id, rungs))
}
#[must_use]
pub fn tool_role_allowed(name: &str, rungs: &[String]) -> Option<bool> {
    TOOL_ROLES.get().and_then(|allowed| allowed(name, rungs))
}
/// Administrator abilities stay hidden from everyone else rather than shown as blocked.
/// Fails closed: without installed manifests every ability counts as one.
#[must_use]
pub fn administrator_ability(id: &str) -> bool {
    ADMINISTRATOR_ABILITY
        .get()
        .is_none_or(|administrator| administrator(id))
}
/// Fails closed: no installed manifests grant no ability.
#[must_use]
pub fn ability_tier_allowed(id: &str, owner_tier: &str, caller_tier: &str) -> bool {
    ABILITY_TIERS
        .get()
        .is_some_and(|allowed| allowed(id, owner_tier, caller_tier))
}
/// Narrowing only: without installed manifests a tool adds no limit of its own.
#[must_use]
pub fn tool_tier_allowed(name: &str, owner_tier: &str) -> bool {
    TOOL_TIERS
        .get()
        .is_none_or(|allowed| allowed(name, owner_tier))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tier {
    pub id: String,
    pub revision: u64,
    pub allowed_models: Vec<String>,
    #[serde(default)]
    pub model_policy: Option<ModelPolicy>,
    pub max_sessions: u32,
    pub max_active_runs: u32,
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
            || self.max_sessions > 10_000
            || self.max_active_runs > 200
        {
            bail!("Invalid entitlement tier");
        }
        if self
            .allowed_models
            .iter()
            .any(|model| model != "*" && !valid_model(model))
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
        let tier_id = match account["tier_id"].as_str() {
            Some(value) if valid_id(value) => value.to_owned(),
            None if legacy_admin => ADMINISTRATOR_TIER.into(),
            _ => bail!("Account has no valid tier"),
        };
        let role = Role::for_tier(&tier_id);
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
    pub tier: Permission,
    pub agent: Permission,
    pub ability: Permission,
    pub account_active: bool,
    pub relationship: bool,
    pub agent_enabled: bool,
    pub available: bool,
    pub visible: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Allowed,
    ApprovalRequired,
    TierDenied,
    AgentDenied,
    AbilityDenied,
    AccountInactive,
    CallerDenied,
    AgentDisabled,
    CapabilityDenied,
    Unavailable,
    Hidden,
    /// The tool's or ability's `role_access` excludes the caller's level: hidden.
    RoleDenied,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ToolDecision {
    pub permission: Permission,
    pub reason: Reason,
}
impl ToolFactors {
    pub fn resolve(self) -> ToolDecision {
        use Permission::*;
        let denial = if self.tier == Deny {
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
        } else {
            None
        };
        if let Some(reason) = denial {
            return ToolDecision {
                permission: Deny,
                reason,
            };
        }
        let permission = self.tier.meet(self.agent).meet(self.ability);
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
    /// Whether the agent is shown the tool. A tool blocked only by the owner's tier stays
    /// listed so a call explains the tier; dispatch still refuses it as a Deny.
    #[must_use]
    pub fn offered(&self) -> bool {
        self.permission != Permission::Deny || self.reason == Reason::TierDenied
    }
    /// Auto waives a human tool checkpoint after every independent denial has
    /// been resolved. Ask, Plan and custom modes keep it.
    pub fn for_execution_mode(self, mode: &Value) -> Self {
        if mode["execution_mode"] == "auto" && self.permission == Permission::Ask {
            Self {
                permission: Permission::Auto,
                reason: Reason::Allowed,
            }
        } else {
            self
        }
    }
}

impl ToolDecision {
    /// The Main Chat's scheduling-approval setting, injected into its session mode as
    /// `schedule_approval`, overrides the mode for its scheduling tools only: "each" always
    /// asks, "recurring" asks for cron schedules, "free" never asks. Denials still win.
    #[must_use]
    pub fn for_schedule_approval(self, mode: &Value, name: &str, args: &Value) -> Self {
        let policy = mode["schedule_approval"].as_str().unwrap_or("tool");
        if self.permission == Permission::Deny
            || !matches!(name, "schedule_task" | "remind_me" | "update_automation")
        {
            return self;
        }
        let ask = match policy {
            "each" => true,
            "recurring" => args["trigger"]["kind"] == "cron",
            "free" => false,
            _ => return self,
        };
        if ask {
            Self {
                permission: Permission::Ask,
                reason: Reason::ApprovalRequired,
            }
        } else {
            Self {
                permission: Permission::Auto,
                reason: Reason::Allowed,
            }
        }
    }
}

#[cfg(test)]
mod schedule_approval_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn main_chat_schedule_approval_overrides_the_mode_for_scheduling_only() {
        let auto = ToolDecision {
            permission: Permission::Auto,
            reason: Reason::Allowed,
        };
        let ask = ToolDecision {
            permission: Permission::Ask,
            reason: Reason::ApprovalRequired,
        };
        let cron = json!({"trigger":{"kind":"cron"}});
        let once = json!({"trigger":{"kind":"once"}});
        let mode = |p: &str| json!({"schedule_approval":p});
        assert_eq!(
            auto.for_schedule_approval(&mode("each"), "schedule_task", &once),
            ask
        );
        assert_eq!(
            auto.for_schedule_approval(&mode("recurring"), "schedule_task", &cron),
            ask
        );
        assert_eq!(
            ask.for_schedule_approval(&mode("recurring"), "remind_me", &once),
            auto
        );
        assert_eq!(
            ask.for_schedule_approval(&mode("free"), "update_automation", &cron),
            auto
        );
        assert_eq!(
            ask.for_schedule_approval(&mode("tool"), "schedule_task", &cron),
            ask
        );
        assert_eq!(
            ask.for_schedule_approval(&json!({}), "schedule_task", &cron),
            ask
        );
        assert_eq!(
            ask.for_schedule_approval(&mode("free"), "web_search", &cron),
            ask
        );
        let denied = ToolDecision {
            permission: Permission::Deny,
            reason: Reason::Allowed,
        };
        assert_eq!(
            denied.for_schedule_approval(&mode("free"), "schedule_task", &cron),
            denied
        );
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
        assert_eq!(admin.tier_id, "administrator");
        assert!(Principal::from_account(&json!({"user_id":"second"})).is_err());
        let suspended = Principal::from_account(&json!({"user_id":"second",
            "status":"suspended","role":"administrator","tier_id":"administrator",
            "access_revision":2}))
        .unwrap();
        assert!(!suspended.administrator());
    }
    #[test]
    fn strictest_permission_and_independent_denials_win() {
        let base = ToolFactors {
            tier: Permission::Auto,
            agent: Permission::Auto,
            ability: Permission::Auto,
            account_active: true,
            relationship: true,
            agent_enabled: true,
            available: true,
            visible: true,
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
        let auto = json!({"execution_mode":"auto"});
        assert_eq!(ask.for_execution_mode(&auto).permission, Permission::Auto);
        assert_eq!(ask.for_execution_mode(&auto).reason, Reason::Allowed);
        assert_eq!(denied.for_execution_mode(&auto), denied);
        assert_eq!(
            ask.for_execution_mode(&json!({"execution_mode":"ask"})),
            ask
        );
        assert_eq!(
            ask.for_execution_mode(&json!({"execution_mode":"plan"})),
            ask
        );
        assert_eq!(ask.for_execution_mode(&json!({})), ask);
    }
    #[test]
    fn byo_downgrade_blocks_use_and_growth_but_allows_reduction() {
        let mut tier = Tier {
            id: "free".into(),
            revision: 1,
            model_policy: None,
            allowed_models: vec!["*".into()],
            max_sessions: 10,
            max_active_runs: 2,
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
            id: "free".into(),
            revision: 1,
            model_policy: None,
            allowed_models: vec!["gpt-6-luna".into()],
            max_sessions: 10,
            max_active_runs: 2,
        };
        tier.validate().unwrap();
        assert!(!tier.allows_model("other"));
        tier.max_active_runs = 201;
        assert!(tier.validate().is_err());
    }
    #[test]
    fn tiers_hold_no_tool_or_ability_grants() {
        let tier = json!({"id":"free","revision":1,"allowed_models":["*"],
            "tools":{"*":"auto"},"max_sessions":10,"max_active_runs":2});
        assert!(serde_json::from_value::<Tier>(tier).is_err());
        // Without installed manifests no ability is granted and tools add no limit.
        assert!(!ability_tier_allowed("web_access", "free", "free"));
        assert!(tool_tier_allowed("file_write", "free"));
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
    tier_id: &str,
) -> anyhow::Result<()> {
    if actor == target && (status != AccountStatus::Active || tier_id != ADMINISTRATOR_TIER) {
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
        tier_id: FREE_TIER,
        profile_is_admin: false,
    }
}
pub const fn initial_guest_access() -> webagent_accounts::InitialAccess<'static> {
    webagent_accounts::InitialAccess {
        role: "guest",
        tier_id: ANONYMOUS_TIER,
        profile_is_admin: false,
    }
}
pub const fn initial_admin_access() -> webagent_accounts::InitialAccess<'static> {
    webagent_accounts::InitialAccess {
        role: "administrator",
        tier_id: ADMINISTRATOR_TIER,
        profile_is_admin: true,
    }
}
