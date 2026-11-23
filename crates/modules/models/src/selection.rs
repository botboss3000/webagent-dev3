//! Model selection records and immutable run plans. No secrets belong in these records.
//! The caller must persist the plan at admission and recheck live grants/revocation
//! before EVERY dispatch. Catalog edits cannot mutate an accepted plan.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_CANDIDATES: usize = 8;

/// One atomic catalog snapshot. Credentials are stored separately in encrypted docs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub revision: u64,
    #[serde(default)]
    pub platform_sources: BTreeMap<String, crate::account_catalog::PlatformPin>,
    pub connections: BTreeMap<String, Connection>,
    pub entries: BTreeMap<String, ModelEntry>,
    /// Retired backup groups and per-agent assignments; saved catalogs may still hold them.
    #[serde(default, rename = "groups", skip_serializing)]
    pub retired_groups: Retired,
    #[serde(default, rename = "assignments", skip_serializing)]
    pub retired_assignments: Retired,
    #[serde(default)]
    pub agent_models: BTreeMap<String, AgentModels>,
}

impl Catalog {
    pub fn validate(&self) -> Result<()> {
        if self.connections.len() > 256
            || self.entries.len() > 1024
            || self.agent_models.len() > 1024
        {
            bail!("Model catalog exceeds record limits");
        }
        for (id, pin) in &self.platform_sources {
            if !self.entries.contains_key(id)
                || id != &crate::account_catalog::platform_id(&pin.entry.id)
            {
                bail!("Invalid platform provenance");
            }
        }
        for (id, c) in &self.connections {
            if id != &c.id
                || !valid_id(id)
                || !valid_id(&c.owner_id)
                || !valid_id(&c.credential_ref)
                || c.revision == 0
                || c.allowed_agents.iter().any(|id| !valid_id(id))
            {
                bail!("Invalid connection record");
            }
        }
        for (id, e) in &self.entries {
            if id != &e.id
                || !valid_id(id)
                || !valid_id(&e.owner_id)
                || !crate::platform_catalog::valid_model(&e.upstream_model)
                || e.display_name.trim().is_empty()
                || e.display_name.len() > 200
                || e.revision == 0
                || e.allowed_agents.iter().any(|id| !valid_id(id))
                || e.bridge_clients.len() > 16
                || e.bridge_clients.iter().any(|id| !valid_id(id))
                || !self.connections.contains_key(&e.connection_id)
            {
                bail!("Invalid model entry record");
            }
        }
        for (id, roster) in &self.agent_models {
            if id != &roster.agent_id
                || !valid_id(id)
                || !valid_id(&roster.owner_id)
                || roster.revision == 0
                || roster.entry_ids.len() > 256
                || roster.entry_ids.iter().collect::<BTreeSet<_>>().len() != roster.entry_ids.len()
                || roster
                    .entry_ids
                    .iter()
                    .any(|id| !self.entries.contains_key(id))
            {
                bail!("Invalid agent model roster");
            }
            // Model-specific support is checked at use against the live provider file,
            // so editing that file never invalidates a saved roster.
            for (id, effort) in &roster.efforts {
                if !roster.entry_ids.contains(id)
                    || !crate::providers::EFFORTS.contains(&effort.as_str())
                {
                    bail!("Invalid default effort for the agent roster");
                }
            }
            for (role, appointment) in &roster.roles {
                let entry = self
                    .entries
                    .get(&appointment.entry_id)
                    .ok_or_else(|| anyhow::anyhow!("Role entry missing"))?;
                if !roster.entry_ids.contains(&appointment.entry_id) || !role.supported_by(entry) {
                    bail!("Role entry is outside roster or lacks required capability");
                }
            }
            roster.validate_aliases()?;
            for (mode, id) in &roster.mode_models {
                let text = self
                    .entries
                    .get(id)
                    .is_some_and(|entry| ModelRole::Standard.supported_by(entry));
                if !valid_id(mode) || !roster.entry_ids.contains(id) || !text {
                    bail!("Invalid execution mode model");
                }
            }
        }
        Ok(())
    }

    // Admission uses explicit appointments; missing specialized roles never inherit Standard.
    pub fn resolve_role(&self, agent_id: &str, role: ModelRole) -> Result<SelectionPlan> {
        let roster = self
            .agent_models
            .get(agent_id)
            .ok_or_else(|| anyhow::anyhow!("{} model required", role.label()))?;
        let appointment = roster
            .roles
            .get(&role)
            .ok_or_else(|| anyhow::anyhow!("{} model required", role.label()))?;
        self.resolve_roster(roster, role, &appointment.entry_id, None)
    }

    /// The critical tool guard's models in the order it tries them, each as a one-model plan:
    /// the agent's Decision model and that model's aliases (an alias may be a decision or a
    /// text model), or with no Decision model assigned its System model and that model's
    /// aliases. A model this agent cannot use is skipped; no usable model is an error.
    pub fn resolve_guard(&self, agent_id: &str) -> Result<Vec<SelectionPlan>> {
        let roster = self
            .agent_models
            .get(agent_id)
            .ok_or_else(|| anyhow::anyhow!("Guard model required"))?;
        let (role, appointment) = match roster.roles.get(&ModelRole::Decision) {
            Some(appointment) => (ModelRole::Decision, appointment),
            None => (
                ModelRole::System,
                roster
                    .roles
                    .get(&ModelRole::System)
                    .ok_or_else(|| anyhow::anyhow!("Guard model required"))?,
            ),
        };
        let aliases = roster
            .aliases
            .get(&appointment.entry_id)
            .map_or(&[][..], Vec::as_slice);
        let mut plans = Vec::new();
        for id in std::iter::once(&appointment.entry_id).chain(aliases) {
            let decision = self
                .entries
                .get(id)
                .is_some_and(|entry| ModelRole::Decision.supported_by(entry));
            let role = match role {
                ModelRole::Decision if !decision => ModelRole::Standard,
                role => role,
            };
            if let Ok(mut plan) = self.resolve_roster(roster, role, id, None) {
                plan.candidates.truncate(1);
                plans.push(plan);
            }
        }
        if plans.is_empty() {
            bail!("No usable guard model");
        }
        Ok(plans)
    }

    /// One roster entry of `agent_id` as the critical tool guard would ask it, for an
    /// administrator's guard probe: a decision model as Decision, any other as a text model.
    pub fn resolve_guard_entry(&self, agent_id: &str, entry_id: &str) -> Result<SelectionPlan> {
        let roster = self
            .agent_models
            .get(agent_id)
            .ok_or_else(|| anyhow::anyhow!("Agent model roster required"))?;
        let decision = self
            .entries
            .get(entry_id)
            .is_some_and(|entry| ModelRole::Decision.supported_by(entry));
        let role = if decision {
            ModelRole::Decision
        } else {
            ModelRole::Standard
        };
        let mut plan = self.resolve_roster(roster, role, entry_id, None)?;
        plan.candidates.truncate(1);
        Ok(plan)
    }

    /// The text model for a chat in execution mode `mode`: the mode's own model when the
    /// roster names one, else the Standard (Text) appointment.
    pub fn resolve_mode(&self, agent_id: &str, mode: Option<&str>) -> Result<SelectionPlan> {
        let roster = self.agent_models.get(agent_id);
        match mode.and_then(|mode| roster?.mode_models.get(mode)) {
            Some(entry_id) => {
                self.resolve_roster(roster.unwrap(), ModelRole::Standard, entry_id, None)
            }
            None => self.resolve_role(agent_id, ModelRole::Standard),
        }
    }

    /// Custom chat uses an explicit roster entry, with that entry's aliases, and never changes Standard.
    pub fn resolve_custom(&self, agent_id: &str, entry_id: &str) -> Result<SelectionPlan> {
        let roster = self
            .agent_models
            .get(agent_id)
            .ok_or_else(|| anyhow::anyhow!("Agent model roster required"))?;
        self.resolve_roster(roster, ModelRole::Standard, entry_id, Some(entry_id.into()))
    }

    /// The selected entry first, then its aliases in the roster's order. The selected entry must
    /// be usable; an alias that lacks the role's capability or authorization is skipped.
    fn resolve_roster(
        &self,
        roster: &AgentModels,
        role: ModelRole,
        entry_id: &str,
        custom_entry_id: Option<String>,
    ) -> Result<SelectionPlan> {
        if !valid_id(&roster.agent_id) || !valid_id(&roster.owner_id) || roster.revision == 0 {
            bail!("Invalid agent model roster");
        }
        let aliases = roster.aliases.get(entry_id).map_or(&[][..], Vec::as_slice);
        let mut candidates = Vec::new();
        for (index, id) in std::iter::once(entry_id)
            .chain(aliases.iter().map(String::as_str))
            .enumerate()
        {
            let usable = self.usable(roster, role, id);
            let (entry, connection) = match (usable, index) {
                (Ok(found), _) => found,
                (Err(error), 0) => return Err(error),
                (Err(_), _) => continue,
            };
            candidates.push(Candidate {
                retired_limits: Retired,
                default_effort: roster.efforts.get(id).cloned(),
                platform: self.platform_sources.get(id).cloned(),
                entry_id: id.to_owned(),
                entry_revision: entry.revision,
                connection_id: connection.id.clone(),
                connection_revision: connection.revision,
                connection_owner_id: connection.owner_id.clone(),
                credential_ref: connection.credential_ref.clone(),
                transport: connection.transport.clone(),
                upstream_model: entry.upstream_model.clone(),
                vision: entry.capabilities.contains("vision"),
                bridge_clients: entry.bridge_clients.clone(),
            });
        }
        Ok(SelectionPlan {
            version: 2,
            retired_revisions: Retired,
            retired_group_revision: Retired,
            agent_id: roster.agent_id.clone(),
            owner_id: roster.owner_id.clone(),
            candidates,
            role,
            custom_entry_id,
            roster_revision: roster.revision,
            catalog_revision: self.revision,
            backup_policy: None,
        })
    }

    /// The roster entry `id` and its connection, when this agent may use them for `role`.
    fn usable(
        &self,
        roster: &AgentModels,
        role: ModelRole,
        id: &str,
    ) -> Result<(&ModelEntry, &Connection)> {
        if !roster.entry_ids.iter().any(|entry| entry == id) {
            bail!("Selected model is outside agent roster");
        }
        let entry = self
            .entries
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("Role entry missing"))?;
        let connection = self
            .connections
            .get(&entry.connection_id)
            .ok_or_else(|| anyhow::anyhow!("Model connection missing"))?;
        if !role.supported_by(entry)
            || entry.id != id
            || connection.id != entry.connection_id
            || entry.revision == 0
            || connection.revision == 0
            || connection.revoked
            || !entry.allowed_agents.contains(&roster.agent_id)
            || !connection.allowed_agents.contains(&roster.agent_id)
        {
            bail!("Role model is incompatible or not authorized");
        }
        Ok((entry, connection))
    }

    pub fn resolve(&self, agent_id: &str) -> Result<SelectionPlan> {
        self.resolve_role(agent_id, ModelRole::Standard)
    }
}

const CATALOG_KEY: &str = "model_selection:catalog:v1";

impl<C: crate::storage::RuntimeStorage> crate::storage::Repository<'_, C> {
    pub async fn model_catalog(&self) -> Result<Catalog> {
        use sqlx::Row;
        let mut connection = self.app_pool.acquire().await?;
        self.read_catalog_connection(&mut connection).await
    }

    /// Administrator API must authorize the caller before invoking this method.
    /// Acquire a database write lock BEFORE reading the revision, including first insert.
    pub async fn save_model_catalog(
        &self,
        catalog: &Catalog,
        expected_revision: u64,
    ) -> Result<Catalog> {
        use sqlx::Row;
        // Removing a model from a roster also removes it from that roster's aliases.
        let mut catalog = catalog.clone();
        for roster in catalog.agent_models.values_mut() {
            roster.prune_references();
        }
        let catalog = &catalog;
        catalog.validate()?;
        let mut tx = self.app_pool.begin().await?;
        sqlx::query("UPDATE docs SET value=value WHERE key=?")
            .bind(CATALOG_KEY)
            .execute(&mut *tx)
            .await?;
        let previous = self.read_catalog_connection(&mut tx).await?;
        if previous.revision != expected_revision || catalog.revision != expected_revision {
            bail!("Model catalog revision changed");
        }
        let mut saved = catalog.clone();
        // Clients submit the last observed record revision. The server advances it.
        macro_rules! revisions {
            ($field:ident) => {
                for (id, record) in &mut saved.$field {
                    if let Some(old) = previous.$field.get(id) {
                        if record.owner_id != old.owner_id || record.revision != old.revision {
                            bail!("Model record ownership or revision changed");
                        }
                        if record != old {
                            record.revision = old.revision.checked_add(1).ok_or_else(|| {
                                anyhow::anyhow!("Model record revision exhausted")
                            })?;
                        }
                    } else if record.revision != 1 {
                        bail!("New model record revision must be one");
                    }
                }
            };
        }
        revisions!(connections);
        revisions!(entries);
        revisions!(agent_models);
        for id in previous.agent_models.keys() {
            if !saved.agent_models.contains_key(id) {
                bail!(
                    "Clear roles and roster explicitly instead of removing the agent model record"
                );
            }
        }
        // Catalog writes authorize newly appointed uses while allowing grants to be
        // revoked from existing appointments. Dispatch checks that live revocation.
        for (id, roster) in &saved.agent_models {
            let previous_roster = previous.agent_models.get(id);
            for entry_id in &roster.entry_ids {
                if previous_roster.is_none_or(|old| !old.entry_ids.contains(entry_id)) {
                    let entry = &saved.entries[entry_id];
                    let connection = &saved.connections[&entry.connection_id];
                    if connection.revoked
                        || !entry.allowed_agents.contains(id)
                        || !connection.allowed_agents.contains(id)
                    {
                        bail!("New roster entry is not authorized");
                    }
                }
            }
            for (role, appointment) in &roster.roles {
                if previous_roster.and_then(|old| old.roles.get(role)) != Some(appointment) {
                    saved.resolve_role(id, *role)?;
                }
            }
        }
        saved.revision = expected_revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Model catalog revision exhausted"))?;
        self.stage_catalog_connection(&mut tx, &saved).await?;
        tx.commit().await?;
        Ok(saved)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    OpenaiSubscription,
    Typesafe,
    Deepseek,
    OpenaiApi,
    Openrouter,
    Abacus,
    Gemini,
    Groq,
    Together,
    Mistral,
    Fireworks,
    Xai,
    Perplexity,
    Deepinfra,
    ClaudeBridge,
    OpencodeBridge,
    MuseBridge,
    CodexBridge,
    AntigravityBridge,
    GeminiBridge,
    Loadtest,
    Local,
}

impl Transport {
    pub fn uses_api_key(&self) -> bool {
        matches!(self, Self::Typesafe)
            || self.provider_id().is_bridge()
            || self.provider_id().chat_endpoint().is_some()
    }
    pub fn provider_id(&self) -> crate::provider::ProviderId {
        match self {
            Self::OpenaiSubscription => crate::provider::ProviderId::OpenaiSubscription,
            Self::Typesafe => crate::provider::ProviderId::Typesafe,
            Self::Deepseek => crate::provider::ProviderId::Deepseek,
            Self::OpenaiApi => crate::provider::ProviderId::OpenaiApi,
            Self::Openrouter => crate::provider::ProviderId::Openrouter,
            Self::Abacus => crate::provider::ProviderId::Abacus,
            Self::Gemini => crate::provider::ProviderId::Gemini,
            Self::Groq => crate::provider::ProviderId::Groq,
            Self::Together => crate::provider::ProviderId::Together,
            Self::Mistral => crate::provider::ProviderId::Mistral,
            Self::Fireworks => crate::provider::ProviderId::Fireworks,
            Self::Xai => crate::provider::ProviderId::Xai,
            Self::Perplexity => crate::provider::ProviderId::Perplexity,
            Self::Deepinfra => crate::provider::ProviderId::Deepinfra,
            Self::ClaudeBridge => crate::provider::ProviderId::ClaudeBridge,
            Self::OpencodeBridge => crate::provider::ProviderId::OpencodeBridge,
            Self::MuseBridge => crate::provider::ProviderId::MuseBridge,
            Self::CodexBridge => crate::provider::ProviderId::CodexBridge,
            Self::AntigravityBridge => crate::provider::ProviderId::AntigravityBridge,
            Self::GeminiBridge => crate::provider::ProviderId::GeminiBridge,
            Self::Loadtest => crate::provider::ProviderId::Loadtest,
            Self::Local => crate::provider::ProviderId::Local,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub id: String,
    pub owner_id: String,
    pub transport: Transport,
    /// Reference to an encrypted credential document, never a token or password.
    pub credential_ref: String,
    pub allowed_agents: BTreeSet<String>,
    pub revision: u64,
    pub revoked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    pub id: String,
    pub owner_id: String,
    pub connection_id: String,
    pub upstream_model: String,
    pub display_name: String,
    pub capabilities: BTreeSet<String>,
    pub allowed_agents: BTreeSet<String>,
    /// Bridge models: the paired clients (their ids) that may answer this entry's calls. Empty
    /// means any client paired with the connection's link, including ones paired later.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bridge_clients: Vec<String>,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
// Capabilities describe support; explicit semantic appointments select workflows.
pub enum ModelRole {
    /// The agent's default text model, shown as "Text"; an execution mode may name its own.
    #[default]
    Standard,
    Vision,
    Image,
    System,
    Voice,
    Decision,
    /// Transcribes voice dictation audio: a speech-to-text provider model. With none assigned
    /// the local recognizer transcribes.
    Dictation,
}
impl ModelRole {
    pub fn label(self) -> &'static str {
        match self {
            Self::Standard => "Text",
            Self::Vision => "Vision",
            Self::Image => "Image",
            Self::System => "System",
            Self::Voice => "Voice",
            Self::Decision => "Decision",
            Self::Dictation => "Dictation",
        }
    }
    pub fn supported_by(self, entry: &ModelEntry) -> bool {
        entry.capabilities.contains(match self {
            Self::Standard | Self::System => "text",
            Self::Vision => "vision",
            Self::Image => "image",
            Self::Voice => "voice",
            Self::Decision => "decision",
            Self::Dictation => "transcription",
        })
    }
}

/// Accepts and drops a retired key in saved records (per-agent token limits, which calls now
/// take from the provider catalog, and the legacy backup groups and assignments). Never serialized.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Retired;
impl<'de> Deserialize<'de> for Retired {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(deserializer).map(|_| Self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentModels {
    #[serde(default, rename = "limits", skip_serializing)]
    pub retired_limits: Retired,
    /// Per-entry default reasoning effort for this agent; absent means the provider default.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub efforts: BTreeMap<String, String>,
    pub agent_id: String,
    pub owner_id: String,
    pub entry_ids: Vec<String>,
    #[serde(deserialize_with = "roles_without_retired")]
    pub roles: BTreeMap<ModelRole, RoleAssignment>,
    /// Text model per execution mode id (ask, plan, auto, or a custom mode); a mode without
    /// one uses the Standard (Text) appointment.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mode_models: BTreeMap<String, String>,
    /// Ordered backups per roster entry: a call on that entry moves to the next alias when the
    /// backup policy allows. Every alias is another entry of this roster.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub aliases: BTreeMap<String, Vec<String>>,
    pub revision: u64,
}
/// Saved rosters may still appoint the retired Premium role; it is dropped on load.
fn roles_without_retired<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<BTreeMap<ModelRole, RoleAssignment>, D::Error> {
    let raw = BTreeMap::<String, RoleAssignment>::deserialize(deserializer)?;
    raw.into_iter()
        .filter(|(role, _)| role != "premium")
        .map(|(role, appointment)| {
            serde_json::from_value(serde_json::Value::String(role))
                .map(|role| (role, appointment))
                .map_err(serde::de::Error::custom)
        })
        .collect()
}
impl AgentModels {
    fn validate_aliases(&self) -> Result<()> {
        for (id, aliases) in &self.aliases {
            let unique: BTreeSet<_> = aliases.iter().collect();
            if !self.entry_ids.contains(id)
                || aliases.is_empty()
                || aliases.len() >= MAX_CANDIDATES
                || unique.len() != aliases.len()
                || aliases
                    .iter()
                    .any(|a| a == id || !self.entry_ids.contains(a))
            {
                bail!("Invalid model aliases");
            }
        }
        Ok(())
    }
    /// Drop aliases, alias lists and mode models naming entries no longer in the roster.
    pub fn prune_references(&mut self) {
        let roster = &self.entry_ids;
        self.aliases.retain(|id, aliases| {
            aliases.retain(|alias| alias != id && roster.contains(alias));
            roster.contains(id) && !aliases.is_empty()
        });
        self.mode_models.retain(|_, id| roster.contains(id));
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleAssignment {
    pub entry_id: String,
    /// Retired role-level backups; aliases now belong to each roster entry.
    #[serde(default, rename = "backup_entry_ids", skip_serializing)]
    pub retired_backups: Retired,
}
impl RoleAssignment {
    pub fn new(entry_id: impl Into<String>) -> Self {
        Self {
            entry_id: entry_id.into(),
            retired_backups: Retired,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    #[serde(default, rename = "limits", skip_serializing)]
    pub retired_limits: Retired,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
    #[serde(default)]
    pub platform: Option<crate::account_catalog::PlatformPin>,
    pub entry_id: String,
    pub entry_revision: u64,
    pub connection_id: String,
    pub connection_revision: u64,
    /// Who added the connection, when the plan was made: the agent owner's own models and
    /// catalog models follow different tier rules (`LiveAccess::model_allowed`). Checks of a
    /// stored plan read the live catalog instead.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub connection_owner_id: String,
    pub credential_ref: String,
    pub transport: Transport,
    pub upstream_model: String,
    /// The entry accepts image input, so sent images may go to it directly.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub vision: bool,
    /// The entry's `bridge_clients`: who may answer a bridge call for this candidate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bridge_clients: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionPlan {
    pub version: u32,
    /// Retired legacy-assignment revisions; plans saved before aliases may still hold them.
    #[serde(default, rename = "assignment_revision", skip_serializing)]
    pub retired_revisions: Retired,
    #[serde(default, rename = "group_revision", skip_serializing)]
    pub retired_group_revision: Retired,
    pub agent_id: String,
    pub owner_id: String,
    pub candidates: Vec<Candidate>,
    #[serde(default)]
    pub role: ModelRole,
    #[serde(default)]
    pub custom_entry_id: Option<String>,
    #[serde(default)]
    pub roster_revision: u64,
    #[serde(default)]
    pub catalog_revision: u64,
    /// The agent's effective backup policy, pinned when the plan is admitted; plans without one
    /// follow the live app policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_policy: Option<crate::backup_policy::BackupPolicy>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-._:".contains(&c))
}

impl SelectionPlan {
    /// The backup policy pinned at admission, or the live app policy for an older plan.
    pub fn policy(&self) -> crate::backup_policy::BackupPolicy {
        self.backup_policy
            .clone()
            .unwrap_or_else(crate::backup_policy::BackupPolicy::live)
    }

    /// Live authorization is separate from snapshot revision: ordinary edits do not
    /// reroute a run, but deletion, credential replacement, or revocation blocks it.
    pub fn authorize_candidate(
        &self,
        index: usize,
        entries: &BTreeMap<String, ModelEntry>,
        connections: &BTreeMap<String, Connection>,
    ) -> Result<&Candidate> {
        let candidate = self
            .candidates
            .get(index)
            .ok_or_else(|| anyhow::anyhow!("Selection candidates exhausted"))?;
        let entry = entries
            .get(&candidate.entry_id)
            .ok_or_else(|| anyhow::anyhow!("Model entry removed"))?;
        let connection = connections
            .get(&candidate.connection_id)
            .ok_or_else(|| anyhow::anyhow!("Model connection removed"))?;
        // Ownership permits administration, never implicit runtime sharing between agents.
        if entry.id != candidate.entry_id
            || connection.id != candidate.connection_id
            || connection.revoked
            || connection.credential_ref != candidate.credential_ref
            || connection.transport != candidate.transport
            || !entry.allowed_agents.contains(&self.agent_id)
            || !connection.allowed_agents.contains(&self.agent_id)
        {
            bail!("Pinned model candidate is no longer authorized");
        }
        Ok(candidate)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    Transient,
    QuotaExhausted,
    InvalidCredentials,
    ModelUnavailable,
    MalformedRequest,
    Cancelled,
    PolicyRejected,
    Protocol,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailoverState {
    pub candidate_index: usize,
    pub retries: u8,
    pub response_started: bool,
    pub uncertain_tool_effect: bool,
    pub history: Vec<FailoverEvent>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailoverEvent {
    pub from_entry_id: String,
    pub to_entry_id: String,
    pub reason: FailureKind,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NextAction {
    Retry,
    Backup,
    Terminal,
}
impl Default for FailoverState {
    fn default() -> Self {
        Self {
            candidate_index: 0,
            retries: 0,
            response_started: false,
            uncertain_tool_effect: false,
            history: Vec::new(),
        }
    }
}
impl FailoverState {
    /// Classify one failure under the plan's backup policy: temporary errors retry on the same
    /// model first, then any switchable failure moves to the next alias. Never replays partial
    /// output or uncertain effects. Caller must durably save this state before redispatch.
    pub fn failed(
        &mut self,
        plan: &SelectionPlan,
        kind: FailureKind,
        policy: &crate::backup_policy::BackupPolicy,
    ) -> NextAction {
        if self.response_started
            || self.uncertain_tool_effect
            || self.candidate_index >= plan.candidates.len()
            || matches!(
                kind,
                FailureKind::MalformedRequest
                    | FailureKind::Cancelled
                    | FailureKind::PolicyRejected
                    | FailureKind::Protocol
            )
        {
            return NextAction::Terminal;
        }
        if kind == FailureKind::Transient && self.retries < policy.same_model_retries {
            self.retries += 1;
            return NextAction::Retry;
        }
        let next = self.candidate_index + 1;
        if next >= plan.candidates.len() || !policy.switches(kind) {
            return NextAction::Terminal;
        }
        self.history.push(FailoverEvent {
            from_entry_id: plan.candidates[self.candidate_index].entry_id.clone(),
            to_entry_id: plan.candidates[next].entry_id.clone(),
            reason: kind,
        });
        self.candidate_index = next;
        self.retries = 0;
        NextAction::Backup
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup_policy::BackupPolicy;
    use serde_json::json;

    fn role(entry_id: &str) -> RoleAssignment {
        RoleAssignment::new(entry_id)
    }
    /// Roster a, b, c on distinct connections; Standard is a with aliases c then b.
    fn catalog_fixture() -> Catalog {
        let mut entries = BTreeMap::new();
        let mut connections = BTreeMap::new();
        for id in ["a", "b", "c"] {
            connections.insert(
                id.into(),
                Connection {
                    id: id.into(),
                    owner_id: "owner".into(),
                    transport: Transport::OpenaiSubscription,
                    credential_ref: format!("credential:{id}"),
                    allowed_agents: BTreeSet::from(["agent-a".into()]),
                    revision: 1,
                    revoked: false,
                },
            );
            entries.insert(
                id.into(),
                ModelEntry {
                    id: id.into(),
                    owner_id: "owner".into(),
                    connection_id: id.into(),
                    upstream_model: "same-upstream".into(),
                    display_name: id.into(),
                    capabilities: BTreeSet::from(["text".into()]),
                    allowed_agents: BTreeSet::from(["agent-a".into()]),
                    bridge_clients: Vec::new(),
                    revision: 1,
                },
            );
        }
        let roster = AgentModels {
            retired_limits: Retired,
            efforts: BTreeMap::new(),
            agent_id: "agent-a".into(),
            owner_id: "owner".into(),
            entry_ids: vec!["a".into(), "b".into(), "c".into()],
            roles: BTreeMap::from([(ModelRole::Standard, role("a"))]),
            mode_models: Default::default(),
            aliases: BTreeMap::from([("a".into(), vec!["c".into(), "b".into()])]),
            revision: 1,
        };
        Catalog {
            revision: 0,
            platform_sources: BTreeMap::new(),
            entries,
            connections,
            retired_groups: Retired,
            retired_assignments: Retired,
            agent_models: BTreeMap::from([("agent-a".into(), roster)]),
        }
    }
    fn ids(plan: &SelectionPlan) -> Vec<&str> {
        plan.candidates
            .iter()
            .map(|c| c.entry_id.as_str())
            .collect()
    }
    #[test]
    fn retired_keys_in_saved_catalogs_rosters_and_plans_are_ignored() {
        let c = catalog_fixture();
        let mut saved = serde_json::to_value(&c).unwrap();
        assert!(saved.get("groups").is_none() && saved.get("assignments").is_none());
        saved["groups"] = json!({});
        saved["assignments"] = json!({});
        let roster = &mut saved["agent_models"]["agent-a"];
        roster["limits"] = json!({"a":{"input_tokens":150_000,"output_tokens":2_000}});
        roster["roles"]["standard"]["backup_entry_ids"] = json!([]);
        assert_eq!(serde_json::from_value::<Catalog>(saved).unwrap(), c);
        let plan = c.resolve("agent-a").unwrap();
        let mut saved = serde_json::to_value(&plan).unwrap();
        assert!(saved["candidates"][0].get("limits").is_none());
        saved["candidates"][0]["limits"] = json!({"input_tokens":1,"output_tokens":1});
        saved["assignment_revision"] = json!(1);
        saved["group_revision"] = json!(null);
        assert_eq!(
            serde_json::from_value::<SelectionPlan>(saved).unwrap(),
            plan
        );
    }
    #[test]
    fn aliases_follow_the_selected_entry_in_order() {
        let mut c = catalog_fixture();
        c.validate().unwrap();
        assert_eq!(ids(&c.resolve("agent-a").unwrap()), vec!["a", "c", "b"]);
        // A custom choice brings its own aliases; one without any runs alone.
        assert_eq!(ids(&c.resolve_custom("agent-a", "b").unwrap()), vec!["b"]);
        let roster = c.agent_models.get_mut("agent-a").unwrap();
        roster.aliases.insert("b".into(), vec!["a".into()]);
        assert_eq!(
            ids(&c.resolve_custom("agent-a", "b").unwrap()),
            vec!["b", "a"]
        );
        let plan = c.resolve("agent-a").unwrap();
        assert_ne!(
            plan.candidates[0].credential_ref,
            plan.candidates[1].credential_ref
        );
        assert_eq!(plan.version, 2);
        assert!(plan.backup_policy.is_none());
    }
    #[test]
    fn invalid_aliases_are_rejected_and_pruning_follows_the_roster() {
        for aliases in [
            vec!["a".to_owned()],
            vec!["b".into(), "b".into()],
            vec!["outside".into()],
            vec![],
        ] {
            let mut c = catalog_fixture();
            c.agent_models
                .get_mut("agent-a")
                .unwrap()
                .aliases
                .insert("a".into(), aliases);
            assert!(c.validate().is_err());
        }
        let mut c = catalog_fixture();
        let roster = c.agent_models.get_mut("agent-a").unwrap();
        roster.entry_ids.retain(|id| id != "c");
        roster.prune_references();
        assert_eq!(roster.aliases["a"], vec!["b"]);
        roster.entry_ids.retain(|id| id != "b");
        roster.prune_references();
        assert!(roster.aliases.is_empty());
    }
    #[test]
    fn unusable_aliases_are_skipped_but_the_selected_entry_must_be_usable() {
        let mut c = catalog_fixture();
        c.connections.get_mut("c").unwrap().revoked = true;
        assert_eq!(ids(&c.resolve("agent-a").unwrap()), vec!["a", "b"]);
        c.entries.get_mut("b").unwrap().allowed_agents.clear();
        assert_eq!(ids(&c.resolve("agent-a").unwrap()), vec!["a"]);
        c.entries.get_mut("a").unwrap().allowed_agents.clear();
        assert!(c.resolve("agent-a").is_err());
    }
    #[test]
    fn default_efforts_pin_per_candidate_and_stay_inside_the_roster() {
        let mut c = catalog_fixture();
        let roster = c.agent_models.get_mut("agent-a").unwrap();
        roster.efforts.insert("a".into(), "high".into());
        c.validate().unwrap();
        let plan = c.resolve("agent-a").unwrap();
        assert_eq!(plan.candidates[0].default_effort.as_deref(), Some("high"));
        assert!(plan.candidates[1..]
            .iter()
            .all(|c| c.default_effort.is_none()));
        let roster = c.agent_models.get_mut("agent-a").unwrap();
        roster.efforts.insert("a".into(), "turbo".into());
        assert!(c.validate().is_err());
        let roster = c.agent_models.get_mut("agent-a").unwrap();
        roster.efforts.clear();
        roster.efforts.insert("outside".into(), "low".into());
        assert!(c.validate().is_err());
    }
    #[test]
    fn decision_requires_an_explicit_compatible_role() {
        let mut c = catalog_fixture();
        c.entries
            .get_mut("b")
            .unwrap()
            .capabilities
            .insert("decision".into());
        assert!(c.resolve_role("agent-a", ModelRole::Decision).is_err());
        c.agent_models
            .get_mut("agent-a")
            .unwrap()
            .roles
            .insert(ModelRole::Decision, role("b"));
        assert_eq!(
            c.resolve_role("agent-a", ModelRole::Decision)
                .unwrap()
                .candidates[0]
                .entry_id,
            "b"
        );
        assert_eq!(
            serde_json::to_value(ModelRole::Decision).unwrap(),
            "decision"
        );
        assert_eq!(c.resolve("agent-a").unwrap().candidates[0].entry_id, "a");
        c.entries
            .get_mut("b")
            .unwrap()
            .capabilities
            .remove("decision");
        assert!(c.resolve_role("agent-a", ModelRole::Decision).is_err());
    }
    #[test]
    fn the_guard_tries_decision_then_its_aliases_or_falls_back_to_system() {
        let mut c = catalog_fixture();
        // No Decision or System model: nothing to ask.
        assert!(c.resolve_guard("agent-a").is_err());
        let roster = c.agent_models.get_mut("agent-a").unwrap();
        roster.roles.insert(ModelRole::System, role("a"));
        let plans = c.resolve_guard("agent-a").unwrap();
        let order: Vec<(&str, ModelRole)> = plans
            .iter()
            .map(|p| (p.candidates[0].entry_id.as_str(), p.role))
            .collect();
        assert_eq!(
            order,
            [
                ("a", ModelRole::System),
                ("c", ModelRole::System),
                ("b", ModelRole::System)
            ]
        );
        assert!(plans.iter().all(|p| p.candidates.len() == 1));
        c.entries
            .get_mut("b")
            .unwrap()
            .capabilities
            .insert("decision".into());
        let roster = c.agent_models.get_mut("agent-a").unwrap();
        roster.roles.insert(ModelRole::Decision, role("b"));
        roster.aliases.insert("b".into(), vec!["a".into()]);
        let order: Vec<(String, ModelRole)> = c
            .resolve_guard("agent-a")
            .unwrap()
            .into_iter()
            .map(|p| (p.candidates[0].entry_id.clone(), p.role))
            .collect();
        assert_eq!(
            order,
            [
                ("b".to_owned(), ModelRole::Decision),
                ("a".to_owned(), ModelRole::Standard)
            ]
        );
        let probe = c.resolve_guard_entry("agent-a", "b").unwrap();
        assert_eq!(
            (probe.role, probe.candidates.len()),
            (ModelRole::Decision, 1)
        );
        assert_eq!(
            c.resolve_guard_entry("agent-a", "a").unwrap().role,
            ModelRole::Standard
        );
        assert!(c.resolve_guard_entry("agent-a", "outside").is_err());
    }
    #[test]
    fn modes_use_their_own_text_model_or_fall_back_to_text() {
        let mut c = catalog_fixture();
        let roster = c.agent_models.get_mut("agent-a").unwrap();
        roster.mode_models.insert("plan".into(), "b".into());
        c.validate().unwrap();
        assert_eq!(
            ids(&c.resolve_mode("agent-a", Some("plan")).unwrap()),
            vec!["b"]
        );
        assert_eq!(
            ids(&c.resolve_mode("agent-a", Some("ask")).unwrap()),
            vec!["a", "c", "b"]
        );
        assert_eq!(
            ids(&c.resolve_mode("agent-a", None).unwrap()),
            vec!["a", "c", "b"]
        );
        let roster = c.agent_models.get_mut("agent-a").unwrap();
        roster.mode_models.insert("auto".into(), "outside".into());
        assert!(c.validate().is_err());
        let roster = c.agent_models.get_mut("agent-a").unwrap();
        roster.entry_ids.retain(|id| id != "b");
        roster.prune_references();
        assert!(!roster.mode_models.contains_key("plan"));
    }
    #[test]
    fn a_saved_premium_role_is_dropped_on_load() {
        let c = catalog_fixture();
        let mut saved = serde_json::to_value(&c).unwrap();
        saved["agent_models"]["agent-a"]["roles"]["premium"] = json!({"entry_id":"b"});
        let loaded: Catalog = serde_json::from_value(saved).unwrap();
        assert_eq!(loaded, c);
        assert_eq!(ModelRole::Standard.label(), "Text");
    }
    #[test]
    fn roles_and_custom_selections_stay_explicit_and_pinned() {
        let mut catalog = catalog_fixture();
        let standard = catalog.resolve("agent-a").unwrap();
        assert!(catalog.resolve_role("agent-a", ModelRole::System).is_err());
        let roster = catalog.agent_models.get_mut("agent-a").unwrap();
        roster.roles.insert(ModelRole::System, role("b"));
        roster.aliases.insert("b".into(), vec!["a".into()]);
        let system = catalog.resolve_role("agent-a", ModelRole::System).unwrap();
        assert_eq!(system.role, ModelRole::System);
        assert_eq!(ids(&system), vec!["b", "a"]);
        let custom = catalog.resolve_custom("agent-a", "c").unwrap();
        assert_eq!(custom.custom_entry_id.as_deref(), Some("c"));
        assert_eq!(custom.candidates.len(), 1);
        assert_eq!(catalog.resolve("agent-a").unwrap(), standard);
        let roster = catalog.agent_models.get_mut("agent-a").unwrap();
        roster.roles.get_mut(&ModelRole::System).unwrap().entry_id = "c".into();
        assert_eq!(system.candidates[0].entry_id, "b");
        assert_eq!(system.roster_revision, 1);
        let mut failed = FailoverState::default();
        assert_eq!(
            failed.failed(
                &system,
                FailureKind::ModelUnavailable,
                &BackupPolicy::default()
            ),
            NextAction::Backup
        );
        assert_eq!(failed.history[0].to_entry_id, "a");
    }
    #[test]
    fn every_role_checks_capability_roster_and_both_explicit_grants() {
        let mut catalog = catalog_fixture();
        for model_role in [ModelRole::Vision, ModelRole::Image, ModelRole::Voice] {
            catalog
                .agent_models
                .get_mut("agent-a")
                .unwrap()
                .roles
                .insert(model_role, role("a"));
            assert!(catalog.resolve_role("agent-a", model_role).is_err());
        }
        assert!(!catalog.resolve("agent-a").unwrap().candidates[0].vision);
        assert!(
            serde_json::to_value(&catalog.resolve("agent-a").unwrap().candidates[0])
                .unwrap()
                .get("vision")
                .is_none()
        );
        catalog.entries.get_mut("a").unwrap().capabilities.extend([
            "vision".into(),
            "image".into(),
            "voice".into(),
        ]);
        for model_role in [ModelRole::Vision, ModelRole::Image, ModelRole::Voice] {
            // Aliases without the role's capability are left out of the plan.
            assert_eq!(
                ids(&catalog.resolve_role("agent-a", model_role).unwrap()),
                vec!["a"]
            );
        }
        // The plan pins whether each candidate reads images, so the loop can send them directly.
        assert!(catalog.resolve("agent-a").unwrap().candidates[0].vision);
        catalog.entries.get_mut("a").unwrap().allowed_agents.clear();
        assert!(catalog.resolve_role("agent-a", ModelRole::Voice).is_err());
        catalog
            .entries
            .get_mut("a")
            .unwrap()
            .allowed_agents
            .insert("agent-a".into());
        catalog
            .connections
            .get_mut("a")
            .unwrap()
            .allowed_agents
            .clear();
        assert!(catalog.resolve_role("agent-a", ModelRole::Voice).is_err());
        catalog
            .connections
            .get_mut("a")
            .unwrap()
            .allowed_agents
            .insert("agent-a".into());
        catalog
            .agent_models
            .get_mut("agent-a")
            .unwrap()
            .entry_ids
            .retain(|id| id != "a");
        assert!(catalog.resolve_custom("agent-a", "a").is_err());
        assert!(catalog.validate().is_err());
    }
    #[test]
    fn editing_the_roster_does_not_reroute_a_pinned_plan() {
        let mut c = catalog_fixture();
        let p = c.resolve("agent-a").unwrap();
        c.agent_models
            .get_mut("agent-a")
            .unwrap()
            .roles
            .insert(ModelRole::Standard, role("b"));
        c.entries.get_mut("a").unwrap().upstream_model = "changed".into();
        assert_eq!(
            p.authorize_candidate(0, &c.entries, &c.connections)
                .unwrap()
                .upstream_model,
            "same-upstream"
        );
        assert_eq!(c.resolve("agent-a").unwrap().candidates[0].entry_id, "b");
    }
    #[test]
    fn revoked_or_replaced_connections_are_not_silently_used() {
        let mut c = catalog_fixture();
        let p = c.resolve("agent-a").unwrap();
        c.connections.get_mut("c").unwrap().revoked = true;
        assert!(p
            .authorize_candidate(1, &c.entries, &c.connections)
            .is_err());
        c.connections.get_mut("a").unwrap().credential_ref = "new-credential".into();
        assert!(p
            .authorize_candidate(0, &c.entries, &c.connections)
            .is_err());
        c.connections.get_mut("b").unwrap().allowed_agents.clear();
        assert!(p
            .authorize_candidate(2, &c.entries, &c.connections)
            .is_err());
    }
    #[test]
    fn bounded_retry_then_ordered_failover_then_terminal() {
        let p = catalog_fixture().resolve("agent-a").unwrap();
        let policy = BackupPolicy::default();
        let mut s = FailoverState::default();
        assert_eq!(
            s.failed(&p, FailureKind::Transient, &policy),
            NextAction::Retry
        );
        assert_eq!(
            s.failed(&p, FailureKind::Transient, &policy),
            NextAction::Retry
        );
        assert_eq!(
            s.failed(&p, FailureKind::Transient, &policy),
            NextAction::Backup
        );
        assert_eq!(s.history[0].to_entry_id, "c");
        assert_eq!(
            s.failed(&p, FailureKind::QuotaExhausted, &policy),
            NextAction::Backup
        );
        assert_eq!(s.history[1].to_entry_id, "b");
        assert_eq!(
            s.failed(&p, FailureKind::InvalidCredentials, &policy),
            NextAction::Terminal
        );
    }
    #[test]
    fn the_policy_sets_retries_and_which_failures_switch() {
        let p = catalog_fixture().resolve("agent-a").unwrap();
        let mut policy = BackupPolicy::default();
        policy.same_model_retries = 0;
        policy.switch_on.quota_exhausted = false;
        let mut s = FailoverState::default();
        assert_eq!(
            s.failed(&p, FailureKind::QuotaExhausted, &policy),
            NextAction::Terminal
        );
        assert_eq!(
            s.failed(&p, FailureKind::Transient, &policy),
            NextAction::Backup
        );
        policy.switch_on.transient = false;
        assert_eq!(
            s.failed(&p, FailureKind::Transient, &policy),
            NextAction::Terminal
        );
    }
    #[test]
    fn unsafe_failures_never_failover() {
        let p = catalog_fixture().resolve("agent-a").unwrap();
        let policy = BackupPolicy::default();
        for kind in [
            FailureKind::MalformedRequest,
            FailureKind::Cancelled,
            FailureKind::PolicyRejected,
            FailureKind::Protocol,
        ] {
            assert_eq!(
                FailoverState::default().failed(&p, kind, &policy),
                NextAction::Terminal
            );
        }
        let mut s = FailoverState {
            response_started: true,
            ..Default::default()
        };
        assert_eq!(
            s.failed(&p, FailureKind::Transient, &policy),
            NextAction::Terminal
        );
        let mut s = FailoverState {
            uncertain_tool_effect: true,
            ..Default::default()
        };
        assert_eq!(
            s.failed(&p, FailureKind::ModelUnavailable, &policy),
            NextAction::Terminal
        );
    }
    #[test]
    fn snapshot_and_history_round_trip() {
        let mut p = catalog_fixture().resolve("agent-a").unwrap();
        p.backup_policy = Some(BackupPolicy::default());
        assert_eq!(
            p,
            serde_json::from_str::<SelectionPlan>(&serde_json::to_string(&p).unwrap()).unwrap()
        );
        let mut s = FailoverState::default();
        s.failed(&p, FailureKind::ModelUnavailable, &p.policy());
        assert_eq!(
            s,
            serde_json::from_str::<FailoverState>(&serde_json::to_string(&s).unwrap()).unwrap()
        );
    }
}

#[cfg(test)]
mod upstream_id_tests {
    #[test]
    fn upstream_ids_are_not_record_ids() {
        assert!(crate::platform_catalog::valid_model(
            "organization/model-name"
        ));
        assert!(!super::valid_id("organization/model-name"));
        for invalid in ["", "model\n", "model name", "model?token=x"] {
            assert!(!crate::platform_catalog::valid_model(invalid));
        }
    }
    #[test]
    fn record_ids_obey_ascii_and_length_limits() {
        let max = "a".repeat(128);
        assert!(super::valid_id(&max));
        assert!(!super::valid_id(&format!("{max}a")));
        assert!(!super::valid_id("model/unsafe"));
        assert!(!super::valid_id("modél"));
    }
}
