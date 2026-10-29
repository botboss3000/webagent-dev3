//! Non-secret runtime authority: the live file `crates/modules/engine/loop-runtime.json`
//! (listed in `live-files.json`), read through the live loader on every use.
use crate::workflow;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};
/// Static composition of the actual native schema catalog; no cached or copied authority.
pub trait SchemaCatalog: Clone + Send + Sync + 'static {
    fn schemas() -> Result<Vec<serde_json::Value>>;
}
/// Defaults can construct workflow ceilings without choosing a native catalog.
/// Live validation and IO require a real composition and fail closed otherwise.
#[derive(Clone)]
pub struct UnboundSchemas;
impl SchemaCatalog for UnboundSchemas {
    fn schemas() -> Result<Vec<serde_json::Value>> {
        bail!("Runtime schema catalog unavailable")
    }
}
/// Live runtime file, relative to the checkout.
pub const LIVE: &str = "crates/modules/engine/loop-runtime.json";
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scheduler {
    #[serde(default)]
    pub max_active_runs: usize,
    pub max_unresolved_runs: usize,
    pub poll_interval_ms: u64,
    pub recovery_interval_ms: u64,
    /// Background fallback only; task hints and earlier lease deadlines still wake immediately.
    #[serde(default = "default_background_rescan_interval_ms")]
    pub background_rescan_interval_ms: u64,
}
/// Existing runtime JSON keeps the deployed five-second default without a rewrite.
fn default_background_rescan_interval_ms() -> u64 {
    webagent_background_contracts::DEFAULT_RESCAN_INTERVAL_MS
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub max_context_bytes: usize,
    /// MODEL-BUDGET-PROFILES: optional operator estimates, not provider tokenizers
    /// or compaction settings. Keys are provider/model; unknown models use bytes.
    #[serde(default)]
    pub model_profiles: BTreeMap<String, ModelBudget>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelBudget {
    pub context_tokens: usize,
    pub output_reserve_tokens: usize,
    pub bytes_per_token: usize,
}
impl Context {
    /// Conservative logical-byte proxy; never expands the existing safety ceiling.
    /// Output reserve is budgeting only, not a claim about upstream output controls.
    pub fn ceiling(
        &self,
        selection: &webagent_models::provider::ProviderSelection,
    ) -> Result<usize> {
        let provider = serde_json::to_value(selection.provider)?
            .as_str()
            .unwrap_or("")
            .to_owned();
        let ceiling = self.max_context_bytes.min(
            selection
                .input_budget_bytes()?
                .unwrap_or(self.max_context_bytes),
        );
        match self
            .model_profiles
            .get(&format!("{provider}/{}", selection.model))
        {
            None => Ok(ceiling),
            Some(p) => {
                if p.context_tokens == 0
                    || p.output_reserve_tokens >= p.context_tokens
                    || !(1..=8).contains(&p.bytes_per_token)
                {
                    bail!("Invalid model budget profile");
                }
                Ok(ceiling.min(
                    (p.context_tokens - p.output_reserve_tokens)
                        .checked_mul(p.bytes_per_token)
                        .ok_or_else(|| anyhow::anyhow!("Model budget overflow"))?,
                ))
            }
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cache {
    pub document_budget_bytes: usize,
    pub context_budget_bytes: usize,
}
// Explicit parallel modes are per tool; omitted tools reject multi-call batches.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tools {
    pub max_parallel_calls: usize,
    pub parallel_modes: BTreeMap<String, String>,
}
impl Default for Tools {
    fn default() -> Self {
        let parallel_modes = [
            "file_list",
            "file_read",
            "file_search",
            "source_review",
            "state_inspect",
            "prompt_slot_read",
        ]
        .into_iter()
        .map(|name| (name.to_owned(), "concurrent".to_owned()))
        .collect();
        Self {
            max_parallel_calls: 4,
            parallel_modes,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    pub max_turns: u64,
    pub max_attempts: u64,
    pub max_publication_bytes: usize,
    pub modules: BTreeMap<String, Vec<String>>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(bound = "")]
pub struct Config<C: SchemaCatalog = UnboundSchemas> {
    #[serde(skip)]
    catalog: std::marker::PhantomData<C>,
    pub schema_version: u32,
    pub scheduler: Scheduler,
    pub context: Context,
    pub cache: Cache,
    #[serde(default)]
    pub tools: Tools,
    pub workflow: Workflow,
}
// Bootstrap only. Once present, the validated data file is the sole authority.
impl<C: SchemaCatalog> Default for Config<C> {
    fn default() -> Self {
        Self {
            catalog: std::marker::PhantomData,
            schema_version: 2,
            scheduler: Scheduler {
                max_active_runs: 0,
                max_unresolved_runs: 200,
                poll_interval_ms: 300,
                recovery_interval_ms: 10000,
                background_rescan_interval_ms: default_background_rescan_interval_ms(),
            },
            context: Context {
                max_context_bytes: 262144,
                model_profiles: BTreeMap::new(),
            },
            cache: Cache {
                document_budget_bytes: 67108864,
                context_budget_bytes: 16777216,
            },
            tools: Tools::default(),
            workflow: Workflow {
                max_turns: 50,
                max_attempts: 3,
                max_publication_bytes: 262144,
                modules: BTreeMap::new(),
            },
        }
    }
}
impl<C: SchemaCatalog> Config<C> {
    pub fn policy(&self) -> Result<workflow::Policy> {
        workflow::Policy::resolve(Some(
            &serde_json::json!({"schema_version":1,"max_turns":self.workflow.max_turns,"max_attempts":self.workflow.max_attempts,"max_publication_bytes":self.workflow.max_publication_bytes,"modules":self.workflow.modules,"max_context_bytes":self.context.max_context_bytes}),
        ))
    }
    pub fn apply_policy(&mut self, p: &workflow::Policy) {
        self.context.max_context_bytes = p.max_context_bytes;
        self.workflow = Workflow {
            max_turns: p.max_turns,
            max_attempts: p.max_attempts,
            max_publication_bytes: p.max_publication_bytes,
            modules: p.modules.clone(),
        };
    }
    pub fn validate(&self) -> Result<()> {
        self.policy()?;
        if self.scheduler.background_rescan_interval_ms == 0 {
            bail!("Background rescan interval must be positive");
        }
        // MODEL-PROFILE-VALIDATION: reject invalid estimates before any provider call.
        for (key, p) in &self.context.model_profiles {
            if self.context.model_profiles.len() > 128
                || key.len() > 256
                || !key.contains('/')
                || p.context_tokens == 0
                || p.context_tokens > 10_000_000
                || p.output_reserve_tokens >= p.context_tokens
                || !(1..=8).contains(&p.bytes_per_token)
            {
                bail!("Invalid model budget profile");
            }
        }
        // BATCH-CONFIG-VALIDATION: unknown names and modes fail closed at startup.
        if !(1..=4).contains(&self.tools.max_parallel_calls) || self.tools.parallel_modes.len() > 64
        {
            bail!("Invalid parallel tool configuration");
        }
        if !self.tools.parallel_modes.is_empty() {
            let schemas = C::schemas()?;
            for (name, mode) in &self.tools.parallel_modes {
                if !schemas.iter().any(|schema| schema["name"] == name.as_str())
                    || !matches!(mode.as_str(), "concurrent" | "reject")
                {
                    bail!("Invalid parallel tool mode");
                }
            }
        }
        if self.schema_version != 2
            || self.scheduler.max_active_runs > 16
            || self.scheduler.max_unresolved_runs == 0
            || self.scheduler.max_unresolved_runs < self.scheduler.max_active_runs
            || self.scheduler.max_unresolved_runs > 200
            || !(10..=60000).contains(&self.scheduler.poll_interval_ms)
            || !(1000..=3600000).contains(&self.scheduler.recovery_interval_ms)
            || self.cache.document_budget_bytes > 268435456
            || self.cache.context_budget_bytes > 67108864
        {
            bail!("Invalid loop runtime configuration");
        }
        Ok(())
    }
    /// Read and validate a runtime file: the live file for a relative path (last good kept on
    /// a broken edit), or exactly that file for an absolute path (test fixtures).
    pub fn read(path: &Path) -> Result<Self> {
        let parse = |bytes: &[u8]| -> Result<Self> {
            let mut value: serde_json::Value = serde_json::from_slice(bytes)?;
            // Schema 1's reserve was redundant. Preserve all other settings and reject unknown fields.
            if value["schema_version"] == 1 {
                value["schema_version"] = serde_json::json!(2);
                if let Some(c) = value["context"].as_object_mut() {
                    c.remove("summary_reserve_bytes");
                }
            }
            let c: Self = serde_json::from_value(value)?;
            c.validate()?;
            Ok(c)
        };
        webagent_configuration::live::parsed_path(path, 1024 * 1024, parse)
    }
    /// Write a fixture file; the live file is saved through the broker.
    pub fn persist(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if path.is_relative() {
            bail!("The live runtime file is saved through the broker");
        }
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Config parent missing"))?;
        std::fs::create_dir_all(parent)?;
        let temp = parent.join(format!(".loop-runtime.{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            f.write_all(&serde_json::to_vec_pretty(self)?)?;
            f.sync_all()?;
            std::fs::rename(&temp, path)?;
            std::fs::File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temp);
        }
        result
    }
    /// The live runtime configuration and its checkout-relative path.
    pub fn live() -> Result<(Self, PathBuf)> {
        let path = PathBuf::from(LIVE);
        Ok((Self::read(&path)?, path))
    }
}
