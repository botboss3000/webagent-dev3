//! Packaged gate descriptors define which module can run at each fixed Rust checkpoint.
//! Operators select modules in workflow JSON; a descriptor never grants tools or publication.
use crate::workflow::{Checkpoint, Policy};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const MESSAGE_VISIBILITY: &str = "message_visibility_v1";
pub const CLOSING_STATUS: &str = "closing_status";
/// Checkout folder of the integral gate configuration files.
pub const GATE_FOLDER: &str = "config/engine/gates";
pub const PUBLICATION_SIZE: &str = "publication_size_guard_v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    Rust,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub id: String,
    pub checkpoint: Checkpoint,
    pub runtime: Runtime,
    pub config: Value,
}

#[derive(Debug, Clone)]
pub struct Selection {
    pub id: String,
    pub optional: bool,
    pub manifest: Manifest,
}

fn source(id: &str) -> Result<String> {
    match id {
        MESSAGE_VISIBILITY | CLOSING_STATUS => {
            // Live files in config/engine/gates (`live-files.json`); an integral gate has no
            // packaged default, so a missing file fails closed.
            webagent_configuration::live::parsed(
                &format!("{GATE_FOLDER}/{id}.json"),
                64 * 1024,
                webagent_configuration::live::utf8,
            )
        }
        PUBLICATION_SIZE => {
            publication_source(webagent_configuration::prompts::default_source(None)?.as_deref())
        }
        _ => bail!("Unknown gate module"),
    }
}

/// A configured published descriptor is authoritative; an absent publication uses the immutable image copy.
fn publication_source(source: Option<&std::path::Path>) -> Result<String> {
    let relative = "plugins/gates/publication_size_guard_v1/gate.json";
    let path = match source {
        Some(root) => root.join(relative),
        None => {
            #[cfg(any(test, feature = "test-support"))]
            {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../..")
                    .join(relative)
            }
            #[cfg(not(any(test, feature = "test-support")))]
            {
                webagent_configuration::paths::app().join(relative)
            }
        }
    };
    read_publication_descriptor(&path)
}
fn read_publication_descriptor(path: &std::path::Path) -> Result<String> {
    use std::io::Read;
    const LIMIT: u64 = 64 * 1024;
    let info = std::fs::symlink_metadata(path)?;
    if !info.is_file() || info.file_type().is_symlink() || info.len() > LIMIT {
        bail!("Invalid publication gate descriptor file");
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        bail!("Publication gate descriptor exceeds limit");
    }
    Ok(String::from_utf8(bytes)?)
}

pub fn manifest(id: &str) -> Result<Manifest> {
    let item: Manifest = serde_json::from_str(&source(id)?)?;
    validate_manifest(id, &item)?;
    Ok(item)
}

pub fn validate_manifest(id: &str, item: &Manifest) -> Result<()> {
    if item.schema_version != 1 || item.id != id {
        bail!("Invalid gate manifest identity");
    }
    // Each built-in descriptor is bounded and tied to its implementation. A future
    // plugin adapter can add IDs here only after its effects and data access are reviewed.
    match id {
        MESSAGE_VISIBILITY => {
            if item.checkpoint != Checkpoint::ModelDispatch || item.runtime != Runtime::Rust {
                bail!("Invalid message-visibility gate binding");
            }
            let config: VisibilityConfig = serde_json::from_value(item.config.clone())?;
            if config.tool_name != "publish_main_message" {
                bail!("Invalid message-visibility gate config");
            }
        }
        CLOSING_STATUS => {
            if item.checkpoint != Checkpoint::Publication || item.runtime != Runtime::Rust {
                bail!("Invalid closing-status gate binding");
            }
            serde_json::from_value::<ClosingConfig>(item.config.clone())?.validate()?;
        }
        PUBLICATION_SIZE => {
            if item.checkpoint != Checkpoint::Publication || item.runtime != Runtime::Rust {
                bail!("Invalid publication-size gate binding");
            }
            let config: SizeConfig = serde_json::from_value(item.config.clone())?;
            if !(1..=262144).contains(&config.max_text_bytes) {
                bail!("Invalid publication-size gate config");
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}

pub use self::controls::{ClosingConfig, VisibilityConfig};
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SizeConfig {
    pub max_text_bytes: usize,
}

pub fn visibility_config() -> Result<VisibilityConfig> {
    Ok(serde_json::from_value(
        manifest(MESSAGE_VISIBILITY)?.config,
    )?)
}
pub fn closing_config() -> Result<ClosingConfig> {
    Ok(serde_json::from_value(manifest(CLOSING_STATUS)?.config)?)
}
/// The live near-duplicate threshold for kept closing messages; an unreadable config keeps 0.8.
pub fn repeat_similarity() -> f64 {
    closing_config().map_or(0.8, |config| config.repeat_similarity)
}

pub fn parse_selection(checkpoint: Checkpoint, configured: &str) -> Result<Selection> {
    let (optional, id) = match configured.strip_prefix("optional:") {
        Some(id) => (true, id),
        None => (false, configured),
    };
    let item = manifest(id)?;
    // `optional:` stays accepted for the size gate, which ran in a separate process that
    // could be unavailable; it now runs in the app, so optional and required act alike.
    if item.checkpoint != checkpoint || (optional && id != PUBLICATION_SIZE) {
        bail!("Gate module cannot run at this checkpoint or in optional mode");
    }
    Ok(Selection {
        id: id.to_owned(),
        optional,
        manifest: item,
    })
}

/// Preserve operator order, but reject duplicate IDs even if one spelling is optional.
pub fn selected(policy: &Policy, checkpoint: Checkpoint) -> Result<Vec<Selection>> {
    let name = serde_json::to_value(checkpoint)?
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Checkpoint name invalid"))?
        .to_owned();
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    if let Some(configured) = policy.modules.get(&name) {
        if configured.len() > 8 {
            bail!("Too many gate modules at checkpoint");
        }
        for raw in configured {
            let selection = parse_selection(checkpoint, raw)?;
            if !seen.insert(selection.id.clone()) {
                bail!("Duplicate gate module");
            }
            result.push(selection);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn packaged_manifests_are_bounded_and_typed() {
        let visibility = visibility_config().unwrap();
        assert_eq!(visibility.tool_name, "publish_main_message");
        let closer = closing_config().unwrap();
        assert_eq!(closer.fallback_flag.as_deref(), Some("unflagged"));
        assert!(closer.notification.max_chars <= 1000);
        assert_eq!(manifest(PUBLICATION_SIZE).unwrap().runtime, Runtime::Rust);
        assert!(closer.notification.target_words <= 50);
    }
    #[test]
    fn malformed_integral_and_plugin_configs_fail_closed() {
        let mut close = manifest(CLOSING_STATUS).unwrap();
        close.config["notification"]["target_words"] = json!(0);
        assert!(validate_manifest(CLOSING_STATUS, &close).is_err());
        close.config["notification"]["target_words"] = json!(5);
        close.config["fallback_flag"] = json!("complete");
        assert!(validate_manifest(CLOSING_STATUS, &close).is_err());
        let mut visibility = manifest(MESSAGE_VISIBILITY).unwrap();
        // A gate file from before the default moved to the prompt files still loads.
        visibility.config["guidance"] = json!("older guidance");
        assert!(validate_manifest(MESSAGE_VISIBILITY, &visibility).is_ok());
        visibility.config["tool_name"] = json!("publish_something_else");
        assert!(validate_manifest(MESSAGE_VISIBILITY, &visibility).is_err());
        let mut size = manifest(PUBLICATION_SIZE).unwrap();
        size.config["max_text_bytes"] = json!(262145);
        assert!(validate_manifest(PUBLICATION_SIZE, &size).is_err());
        size.config["max_text_bytes"] = json!(8192);
        size.checkpoint = Checkpoint::ModelDispatch;
        assert!(validate_manifest(PUBLICATION_SIZE, &size).is_err());
    }
    #[test]
    fn selection_rejects_unknown_duplicates_and_wrong_checkpoint() {
        for modules in [
            json!({"publication":["unknown"]}),
            json!({"publication":[PUBLICATION_SIZE, format!("optional:{PUBLICATION_SIZE}")]}),
            json!({"model_dispatch":[CLOSING_STATUS]}),
            json!({"publication":[format!("optional:{CLOSING_STATUS}")]}),
        ] {
            assert!(Policy::resolve(Some(&json!({"modules":modules}))).is_err());
        }
        let policy = Policy::resolve(Some(&json!({"modules":{
            "model_dispatch":[MESSAGE_VISIBILITY],
            "publication":[CLOSING_STATUS,format!("optional:{PUBLICATION_SIZE}")]
        }})))
        .unwrap();
        assert_eq!(selected(&policy, Checkpoint::Publication).unwrap().len(), 2);
    }
}

pub mod controls {
    //! The loop calls gate behavior through these two integral modules.
    pub use super::closing::{
        agent_closing, closer_message, closer_prompt, closing_status, fallback_status,
        parse_closer, AgentClosing, CloserSettings, ClosingConfig, FlagSource, FlagSpec,
        SYSTEM_FLAGS,
    };
    pub use super::initial::{
        agent_visibility, working_schema, working_text, AgentVisibility, PromotionMode,
        VisibilityConfig,
    };
}
mod initial {
    //! The initial gate owns bounded working updates visible before the final reply.
    use crate::ports::AgentMetadata;
    use anyhow::{anyhow, bail, Result};
    use serde::{Deserialize, Serialize};
    use serde_json::{json, Value};
    use webagent_access::storage::RecordCodec;
    #[derive(Debug, Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct VisibilityConfig {
        pub tool_name: String,
        /// Gate files from before the default moved to the prompt files still carry it;
        /// it is read and ignored.
        #[serde(default, rename = "guidance")]
        _retired_guidance: Option<String>,
    }

    /// The agent selects one publication path; the server still owns the manual protocol.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum PromotionMode {
        None,
        Manual,
        Auto,
    }

    #[derive(Debug, Clone, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct AgentVisibility {
        pub mode: PromotionMode,
        /// Retired: publishing guidance is part of each execution mode's per-turn guidance.
        /// Kept so saved agents still load.
        #[serde(default)]
        pub guidance: String,
    }
    impl Default for AgentVisibility {
        fn default() -> Self {
            Self {
                mode: PromotionMode::Manual,
                guidance: String::new(),
            }
        }
    }
    impl AgentVisibility {
        pub fn validate(&self) -> Result<()> {
            if self.guidance.len() > 4096 {
                bail!("Publishing guidance must be at most 4096 bytes");
            }
            Ok(())
        }
        pub fn from_stored(value: &Value) -> Result<Self> {
            if value.is_null() {
                return Ok(Self::default());
            }
            if value.get("mode").is_some() {
                let mut stored: Self = serde_json::from_value(value.clone())?;
                // "No promotion" is retired: the publish tool is always offered.
                if stored.mode == PromotionMode::None {
                    stored.mode = PromotionMode::Manual;
                }
                return Ok(stored);
            }
            // Existing agents used a boolean. The retired format field is intentionally
            // ignored: Ask/Plan/Auto prompts now supply call-scoped formatting.
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Legacy {
                enabled: bool,
                guidance: String,
                #[serde(default)]
                format: String,
            }
            let legacy: Legacy = serde_json::from_value(value.clone())?;
            let _ = (legacy.format, legacy.enabled);
            Ok(Self {
                mode: PromotionMode::Manual,
                guidance: legacy.guidance,
            })
        }
    }
    pub async fn agent_visibility(
        store: &impl crate::ports::AgentMetadata,
        agent: &str,
    ) -> Result<AgentVisibility> {
        let view = store.for_agent(agent).await?;
        let blob: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
            .bind(agent)
            .fetch_one(view.agent_pool())
            .await?;
        let metadata = view.unseal(&blob)?;
        let settings = AgentVisibility::from_stored(&metadata["message_visibility"])?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn working_schema(config: &VisibilityConfig) -> Value {
        json!({"type":"function","name":config.tool_name,
        "description":"Publish a user-visible working update while this run continues. Without text it publishes the message you wrote just before this call in the same reply.",
        "parameters":{"type":"object","additionalProperties":false,
            "properties":{"text":{"type":"string","description":"Optional replacement text; omit it to publish your preceding message as written"}}}})
    }

    /// The update to publish: `text` when given, otherwise `preceding`, the message the
    /// model wrote before this call in the same reply, so it never retypes it.
    pub fn working_text(args: &Value, preceding: &str) -> Result<String> {
        let object = args
            .as_object()
            .ok_or_else(|| anyhow!("Working update must be an object"))?;
        if object.keys().any(|key| key != "text") {
            bail!("Working update has unexpected fields");
        }
        let text = match object.get("text") {
            Some(text) => text
                .as_str()
                .ok_or_else(|| anyhow!("Working update text must be a string"))?,
            None if preceding.trim().is_empty() => {
                bail!("No message precedes this call in the same reply; pass text")
            }
            None => preceding,
        };
        if text.trim().is_empty() || text.len() > 16384 {
            bail!("Working update text is empty or too large");
        }
        Ok(text.to_owned())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn agent_visibility_is_additive_strict_and_byte_bounded() {
            let defaults = AgentVisibility::default();
            assert_eq!(defaults.mode, PromotionMode::Manual);
            assert!(defaults.guidance.is_empty());
            assert!(AgentVisibility {
                mode: PromotionMode::None,
                guidance: "x".repeat(4096)
            }
            .validate()
            .is_ok());
            assert!(AgentVisibility {
                mode: PromotionMode::Auto,
                guidance: "é".repeat(2049)
            }
            .validate()
            .is_err());
            assert_eq!(
                AgentVisibility::from_stored(
                    &json!({"enabled":true,"guidance":"ok","format":"retired"})
                )
                .unwrap()
                .mode,
                PromotionMode::Manual
            );
            assert_eq!(
                AgentVisibility::from_stored(&json!({"enabled":false,"guidance":"ok"}))
                    .unwrap()
                    .mode,
                PromotionMode::Manual
            );
            assert_eq!(
                AgentVisibility::from_stored(&json!({"mode":"auto","guidance":"ok"}))
                    .unwrap()
                    .mode,
                PromotionMode::Auto
            );
            // "No promotion" is retired and loads as the publish tool, and guidance is optional.
            assert_eq!(
                AgentVisibility::from_stored(&json!({"mode":"none"}))
                    .unwrap()
                    .mode,
                PromotionMode::Manual
            );
            assert!(AgentVisibility::from_stored(
                &json!({"mode":"auto","guidance":"ok","format":"retired"})
            )
            .is_err());
            assert!(AgentVisibility::from_stored(
                &json!({"enabled":true,"guidance":"ok","fixed_protocol":"override"})
            )
            .is_err());
            assert!(AgentVisibility::from_stored(&json!({"mode":"both","guidance":"ok"})).is_err());
        }
        #[test]
        fn working_update_is_bounded_and_exact() {
            assert_eq!(
                working_text(&json!({"text":"Progress"}), "Earlier").unwrap(),
                "Progress"
            );
            assert_eq!(working_text(&json!({}), "Earlier").unwrap(), "Earlier");
            assert!(working_text(&json!({}), " ").is_err());
            assert!(working_text(&json!({"text":3}), "Earlier").is_err());
            assert!(working_text(&json!({"text":" ","extra":1}), "").is_err());
            assert!(working_text(&json!({"text":"x".repeat(16385)}), "").is_err());
            assert!(working_text(&json!({}), &"x".repeat(16385)).is_err());
        }
    }
}
mod closing {
    //! The closing status: a status flag and short notification for the session list. After a
    //! final reply the run makes one private closing turn on the run's own model: the same
    //! request plus the reply and `closing_status.md`, so the provider can reuse its cache.
    //! The answer is never published or kept in history. `closing_status.json` defines every
    //! session flag, agent and system, and the closing turn's attempts.
    use crate::ports::AgentMetadata;
    use anyhow::{anyhow, bail, Result};
    use serde::{Deserialize, Serialize};
    use serde_json::{json, Value};
    use webagent_access::storage::RecordCodec;

    /// Flags the runtime sets from run state; the config must define each of them.
    pub const SYSTEM_FLAGS: [&str; 5] =
        ["running", "queued", "awaiting_approval", "stopped", "error"];
    const TONES: [&str; 5] = ["success", "warning", "danger", "info", "neutral"];

    /// Who sets a flag: the closer from the agent's final reply, or the runtime.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum FlagSource {
        Agent,
        System,
    }

    /// One session flag: its key, display label and colour role, and for agent flags the
    /// meaning the agent is given.
    #[derive(Debug, Clone, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct FlagSpec {
        pub key: String,
        pub source: FlagSource,
        pub label: String,
        pub tone: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub meaning: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tooltip: Option<String>,
    }

    /// Notification length: the agent is asked for about `target_words`; the stored text
    /// is cut at `max_chars` only to stop a runaway message.
    #[derive(Debug, Clone, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct NotificationLimits {
        pub target_words: usize,
        pub max_chars: usize,
    }

    /// The closing turn: model calls allowed before the fallback status.
    #[derive(Debug, Clone, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct CloserSettings {
        pub attempts: u32,
    }
    impl Default for CloserSettings {
        fn default() -> Self {
            Self { attempts: 2 }
        }
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct ClosingConfig {
        /// Missing in a config written before the closing turn: two attempts.
        #[serde(default)]
        pub closer: CloserSettings,
        /// The retired closing tool's name. A release reads the live file before promotion
        /// replaces it, so the old key is accepted and ignored.
        #[serde(default, rename = "tool_name", skip_serializing)]
        pub retired_tool_name: Option<String>,
        pub notification: NotificationLimits,
        /// Flag shown when the closer gives no valid status within its attempts; `null`
        /// shows none.
        pub fallback_flag: Option<String>,
        /// Share of distinct words a final reply must have in common with the kept closing
        /// message before it replaces it; 0 turns the check off. Missing means 0.8.
        #[serde(default = "default_repeat_similarity")]
        pub repeat_similarity: f64,
        pub flags: Vec<FlagSpec>,
    }
    fn default_repeat_similarity() -> f64 {
        0.8
    }

    impl ClosingConfig {
        /// Keys the agent may choose, in configured order.
        pub fn agent_flags(&self) -> Vec<&str> {
            self.flags
                .iter()
                .filter(|flag| flag.source == FlagSource::Agent)
                .map(|flag| flag.key.as_str())
                .collect()
        }

        /// Reject a config the runtime or the session list could misread.
        pub fn validate(&self) -> Result<()> {
            if !(1..=3).contains(&self.closer.attempts) {
                bail!("closer attempts must be 1 to 3");
            }
            if !(1..=50).contains(&self.notification.target_words)
                || !(20..=1000).contains(&self.notification.max_chars)
                || !(0.0..=1.0).contains(&self.repeat_similarity)
            {
                bail!("closing notification limits are out of range");
            }
            if self.flags.is_empty() || self.flags.len() > 32 {
                bail!("closing config needs 1 to 32 flags");
            }
            let mut keys = std::collections::BTreeSet::new();
            for flag in &self.flags {
                let key_ok = !flag.key.is_empty()
                    && flag.key.len() <= 32
                    && flag.key.chars().all(|c| c.is_ascii_lowercase() || c == '_');
                let label_ok = !flag.label.trim().is_empty()
                    && flag.label.chars().count() <= 32
                    && !flag.label.chars().any(char::is_control);
                let meaning_ok = match (&flag.meaning, flag.source) {
                    (Some(text), FlagSource::Agent) => {
                        !text.trim().is_empty() && text.chars().count() <= 300
                    }
                    (None, FlagSource::System) => true,
                    _ => false,
                };
                let tooltip_ok = flag
                    .tooltip
                    .as_ref()
                    .is_none_or(|text| !text.trim().is_empty() && text.chars().count() <= 200);
                if !key_ok
                    || !label_ok
                    || !meaning_ok
                    || !tooltip_ok
                    || !TONES.contains(&flag.tone.as_str())
                    || !keys.insert(flag.key.as_str())
                {
                    bail!("closing flag '{}' is invalid", flag.key);
                }
            }
            if self.agent_flags().is_empty() {
                bail!("closing config needs at least one agent flag");
            }
            for key in SYSTEM_FLAGS {
                if self.system_flag(key).is_none() {
                    bail!("closing config must define system flag '{key}'");
                }
            }
            if let Some(key) = &self.fallback_flag {
                if self.system_flag(key).is_none() || SYSTEM_FLAGS.contains(&key.as_str()) {
                    bail!("fallback_flag must name a system flag other than the run states, or be null");
                }
            }
            Ok(())
        }

        fn system_flag(&self, key: &str) -> Option<&FlagSpec> {
            self.flags
                .iter()
                .find(|flag| flag.key == key && flag.source == FlagSource::System)
        }
    }

    /// Agent preferences: whether the closing status runs, which parts it requires, and
    /// guidance for the private notification.
    #[derive(Debug, Clone, Deserialize, Serialize)]
    #[serde(deny_unknown_fields, default)]
    pub struct AgentClosing {
        pub enabled: bool,
        pub require_flag: bool,
        pub require_notification: bool,
        pub notification_guidance: String,
    }
    impl Default for AgentClosing {
        fn default() -> Self {
            Self {
                enabled: true,
                require_flag: true,
                require_notification: true,
                notification_guidance: String::new(),
            }
        }
    }
    impl AgentClosing {
        /// The closer runs only when the agent requires at least one part.
        pub fn active(&self) -> bool {
            self.enabled && (self.require_flag || self.require_notification)
        }
        pub fn validate(&self) -> Result<()> {
            if self.notification_guidance.len() > 4096 {
                bail!("Notification guidance must be at most 4096 bytes");
            }
            Ok(())
        }
    }

    pub async fn agent_closing(
        store: &impl crate::ports::AgentMetadata,
        agent: &str,
    ) -> Result<AgentClosing> {
        let view = store.for_agent(agent).await?;
        let blob: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
            .bind(agent)
            .fetch_one(view.agent_pool())
            .await?;
        let metadata = view.unseal(&blob)?;
        let settings = if metadata["closing_message"].is_null() {
            AgentClosing::default()
        } else {
            serde_json::from_value(metadata["closing_message"].clone())?
        };
        settings.validate()?;
        Ok(settings)
    }

    /// The closer's live prompt file, beside `closing_status.json`.
    pub fn closer_prompt() -> Result<String> {
        let text = webagent_configuration::live::parsed(
            &format!("{}/closing_status.md", crate::gates::GATE_FOLDER),
            16 * 1024,
            webagent_configuration::live::utf8,
        )?;
        if text.trim().is_empty() {
            bail!("closer prompt is empty");
        }
        Ok(text)
    }

    /// The closing turn's private message: the prompt, the agent flags and note target this
    /// agent requires, its own note guidance, the exact answer format, and, on a retry, what
    /// was wrong with the previous answer.
    pub fn closer_message(
        prompt: &str,
        config: &ClosingConfig,
        settings: &AgentClosing,
        error: Option<&str>,
    ) -> String {
        let mut text = prompt.trim().to_owned();
        let mut format = Vec::new();
        if settings.require_flag {
            text.push_str("\n\nFlags:");
            for flag in config
                .flags
                .iter()
                .filter(|f| f.source == FlagSource::Agent)
            {
                let meaning = flag.meaning.as_deref().unwrap_or("");
                text.push_str(&format!("\n- {}: {meaning}", flag.key));
            }
            format.push("flag: <one flag key>");
        }
        if settings.require_notification {
            text.push_str(&format!(
                "\n\nThe note is about {} words: a glance-sized status or next step shown in \
                 the session list.",
                config.notification.target_words
            ));
            if !settings.notification_guidance.trim().is_empty() {
                text.push_str("\nAgent guidance for the note:\n");
                text.push_str(settings.notification_guidance.trim());
            }
            format.push("note: <the note>");
        }
        text.push_str("\n\nAnswer with exactly these lines and nothing else, and call no tool:\n");
        text.push_str(&format.join("\n"));
        if let Some(error) = error {
            text.push_str(&format!("\n\nYour previous answer was invalid: {error}."));
        }
        text
    }

    /// Read the closer's `flag:` and `note:` lines and validate them like a status.
    pub fn parse_closer(
        text: &str,
        config: &ClosingConfig,
        settings: &AgentClosing,
    ) -> Result<Value> {
        let mut args = serde_json::Map::new();
        for line in text.lines() {
            // Models sometimes add list markers, bold or code quotes around keys and values.
            let mark = |c: char| c.is_whitespace() || matches!(c, '*' | '`' | '"' | '-');
            let line = line.trim_matches(mark);
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let field = match key.trim_matches(mark).to_ascii_lowercase().as_str() {
                "flag" if settings.require_flag => "flag",
                "note" if settings.require_notification => "notification",
                _ => continue,
            };
            let value = value.trim_matches(mark);
            args.entry(field).or_insert_with(|| json!(value));
        }
        closing_status(&Value::Object(args), config, settings)
    }

    /// Validate exactly the parts this agent requires; parts it does not require are absent.
    /// A long notification is kept, cut at the configured character cap.
    pub fn closing_status(
        args: &Value,
        config: &ClosingConfig,
        settings: &AgentClosing,
    ) -> Result<Value> {
        let object = args
            .as_object()
            .ok_or_else(|| anyhow!("arguments must be a JSON object"))?;
        let allowed: Vec<&str> = [
            (settings.require_flag, "flag"),
            (settings.require_notification, "notification"),
        ]
        .into_iter()
        .filter_map(|(required, name)| required.then_some(name))
        .collect();
        if object.keys().any(|key| !allowed.contains(&key.as_str())) {
            bail!("only the fields {} are allowed", allowed.join(" and "));
        }
        let mut status = json!({"fallback":false});
        if settings.require_flag {
            let flag = object
                .get("flag")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("flag is missing"))?;
            let agent_flags = config.agent_flags();
            if !agent_flags.contains(&flag) {
                bail!(
                    "flag '{flag}' is unsupported; use one of {}",
                    agent_flags.join(", ")
                );
            }
            status["flag"] = json!(flag);
        }
        if !settings.require_notification {
            return Ok(status);
        }
        let notification = object
            .get("notification")
            .and_then(Value::as_str)
            .map(str::trim)
            .ok_or_else(|| anyhow!("notification is missing"))?;
        if notification.is_empty() {
            bail!("notification is empty");
        }
        let kept: String = notification
            .chars()
            .take(config.notification.max_chars)
            .collect();
        status["notification"] = json!(kept.trim_end());
        Ok(status)
    }

    /// The status recorded when the closer gives none: the configured fallback flag, only for
    /// agents that require a flag.
    pub fn fallback_status(config: &ClosingConfig, settings: &AgentClosing) -> Value {
        let mut status = json!({"fallback":true});
        if let Some(flag) = config
            .fallback_flag
            .as_ref()
            .filter(|_| settings.require_flag)
        {
            status["flag"] = json!(flag);
        }
        status
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn closer_accepts_agent_flags_and_keeps_long_notifications() {
            let config = crate::gates::closing_config().unwrap();
            let all = AgentClosing::default();
            assert_eq!(
                closing_status(
                    &json!({"flag":"complete","notification":"Done"}),
                    &config,
                    &all
                )
                .unwrap()["flag"],
                "complete"
            );
            for flag in ["unknown", "error", "unflagged", "running"] {
                assert!(
                    closing_status(&json!({"flag":flag,"notification":"Done"}), &config, &all)
                        .is_err()
                );
            }
            let long = "word ".repeat(30);
            let kept = closing_status(
                &json!({"flag":"waiting","notification":long}),
                &config,
                &all,
            )
            .unwrap();
            assert_eq!(
                kept["notification"],
                long.trim(),
                "a long notification is kept, not refused"
            );
            let runaway = "x".repeat(config.notification.max_chars + 50);
            let cut = closing_status(
                &json!({"flag":"waiting","notification":runaway}),
                &config,
                &all,
            )
            .unwrap();
            assert_eq!(
                cut["notification"].as_str().unwrap().chars().count(),
                config.notification.max_chars
            );
            let flag = closing_status(&json!({"flag":"done","notification":"Done"}), &config, &all);
            assert!(flag
                .unwrap_err()
                .to_string()
                .starts_with("flag 'done' is unsupported; use one of complete"));
            assert_eq!(fallback_status(&config, &all)["flag"], "unflagged");
            let note_only = AgentClosing {
                require_flag: false,
                ..AgentClosing::default()
            };
            assert!(fallback_status(&config, &note_only).get("flag").is_none());
        }
        #[test]
        fn config_rejects_missing_system_flags_and_bad_fallbacks() {
            let config = crate::gates::closing_config().unwrap();
            config.validate().unwrap();
            // The live file a release reads before promotion has the old shape.
            let mut old = crate::gates::manifest(crate::gates::CLOSING_STATUS)
                .unwrap()
                .config;
            old.as_object_mut().unwrap().remove("closer");
            old["tool_name"] = json!("set_closer_status");
            let old: ClosingConfig = serde_json::from_value(old).unwrap();
            old.validate().unwrap();
            assert_eq!(old.closer.attempts, 2);
            let mut missing = config.clone();
            missing.flags.retain(|flag| flag.key != "awaiting_approval");
            assert!(missing.validate().is_err());
            for fallback in ["", "complete", "running", "nope"] {
                let mut bad = config.clone();
                bad.fallback_flag = Some(fallback.into());
                assert!(bad.validate().is_err(), "{fallback}");
            }
            let mut off = config.clone();
            off.fallback_flag = None;
            off.validate().unwrap();
            let mut tone = config.clone();
            tone.flags[0].tone = "#00ff00".into();
            assert!(tone.validate().is_err());
            let mut no_meaning = config;
            no_meaning.flags[0].meaning = None;
            assert!(no_meaning.validate().is_err());
        }
        #[test]
        fn agent_toggles_select_the_required_closing_parts() {
            let config = crate::gates::closing_config().unwrap();
            let flag_only = AgentClosing {
                require_notification: false,
                ..AgentClosing::default()
            };
            assert_eq!(
                config.agent_flags(),
                ["complete", "needs_input", "waiting", "unverified", "failed"]
            );
            let status = closing_status(&json!({"flag":"complete"}), &config, &flag_only).unwrap();
            assert_eq!(status["flag"], "complete");
            assert!(status.get("notification").is_none());
            assert!(closing_status(
                &json!({"flag":"complete","notification":"Done"}),
                &config,
                &flag_only
            )
            .is_err());
            assert!(!closer_message("P", &config, &flag_only, None).contains("note:"));
            let note_only = AgentClosing {
                require_flag: false,
                ..AgentClosing::default()
            };
            let status =
                closing_status(&json!({"notification":"Done"}), &config, &note_only).unwrap();
            assert!(status.get("flag").is_none());
            assert!(!closer_message("P", &config, &note_only, None).contains("Flags:"));
            assert!(!AgentClosing {
                enabled: false,
                ..AgentClosing::default()
            }
            .active());
            assert!(!AgentClosing {
                require_flag: false,
                require_notification: false,
                ..AgentClosing::default()
            }
            .active());
            let saved: AgentClosing =
                serde_json::from_value(json!({"notification_guidance":"Keep it short"})).unwrap();
            assert!(saved.active() && saved.require_flag && saved.require_notification);
        }
        #[test]
        fn closer_message_lists_flag_meanings_and_appends_agent_text() {
            let config = crate::gates::closing_config().unwrap();
            let settings = AgentClosing {
                notification_guidance: "Name the next step".into(),
                ..AgentClosing::default()
            };
            settings.validate().unwrap();
            let prompt = closer_prompt().unwrap();
            let combined = closer_message(&prompt, &config, &settings, None);
            assert!(combined.starts_with(prompt.trim()));
            assert!(combined.contains("about 5 words"));
            assert!(combined.contains("- waiting: you are waiting on background work"));
            assert!(!combined.contains("unflagged"));
            assert!(combined.contains("Name the next step"));
            assert!(combined.ends_with("flag: <one flag key>\nnote: <the note>"));
            let retry = closer_message(&prompt, &config, &settings, Some("flag is missing"));
            assert!(retry.ends_with("Your previous answer was invalid: flag is missing."));
            assert!(AgentClosing {
                notification_guidance: "x".repeat(4097),
                ..AgentClosing::default()
            }
            .validate()
            .is_err());
        }
        #[test]
        fn closer_answers_parse_strictly() {
            let config = crate::gates::closing_config().unwrap();
            let all = AgentClosing::default();
            let status =
                parse_closer("flag: needs_input\nnote: Links shared", &config, &all).unwrap();
            assert_eq!(status["flag"], "needs_input");
            assert_eq!(status["notification"], "Links shared");
            assert_eq!(status["fallback"], false);
            let marked = parse_closer("**Flag:** `complete`\n- Note: \"Done\"", &config, &all);
            assert_eq!(marked.unwrap()["notification"], "Done");
            for bad in [
                "note: Done",
                "flag: complete",
                "flag: done\nnote: x",
                "",
                "flag: complete\nnote:",
            ] {
                assert!(parse_closer(bad, &config, &all).is_err(), "{bad}");
            }
            let flag_only = AgentClosing {
                require_notification: false,
                ..AgentClosing::default()
            };
            let status = parse_closer("flag: complete\nnote: ignored", &config, &flag_only);
            assert!(status.unwrap().get("notification").is_none());
        }
    }
}
pub mod protocol {
    //! The publication-size gate: checks a proposed final reply's byte count against the
    //! configured descriptor. It sees only the size, never the text, and cannot publish.
    use anyhow::Result;

    pub const MODULE_ID: &str = crate::gates::PUBLICATION_SIZE;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum GateOutcome {
        Pass,
        Reject {
            module_id: String,
            reason_code: String,
        },
    }

    /// Rejects a reply larger than any selected size gate's `max_text_bytes`.
    pub fn evaluate_publication(
        policy: &crate::workflow::Policy,
        text_bytes: usize,
    ) -> Result<GateOutcome> {
        for selected in crate::gates::selected(policy, crate::workflow::Checkpoint::Publication)? {
            if selected.id != MODULE_ID {
                continue;
            }
            let config: crate::gates::SizeConfig =
                serde_json::from_value(selected.manifest.config)?;
            if text_bytes > config.max_text_bytes {
                return Ok(GateOutcome::Reject {
                    module_id: selected.id,
                    reason_code: "publication_size_exceeded".into(),
                });
            }
        }
        Ok(GateOutcome::Pass)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;

        #[test]
        fn size_gate_rejects_only_above_the_configured_limit() {
            let limit = crate::gates::manifest(MODULE_ID).unwrap().config["max_text_bytes"]
                .as_u64()
                .unwrap() as usize;
            for spelling in [MODULE_ID.to_owned(), format!("optional:{MODULE_ID}")] {
                let policy = crate::workflow::Policy::resolve(Some(
                    &json!({"modules":{"publication":[spelling]}}),
                ))
                .unwrap();
                assert_eq!(
                    evaluate_publication(&policy, limit).unwrap(),
                    GateOutcome::Pass
                );
                assert_eq!(
                    evaluate_publication(&policy, limit + 1).unwrap(),
                    GateOutcome::Reject {
                        module_id: MODULE_ID.into(),
                        reason_code: "publication_size_exceeded".into()
                    }
                );
            }
        }

        #[test]
        fn without_the_size_gate_every_size_passes() {
            let policy = crate::workflow::Policy::resolve(Some(
                &json!({"modules":{"publication":[crate::gates::CLOSING_STATUS]}}),
            ))
            .unwrap();
            assert_eq!(
                evaluate_publication(&policy, usize::MAX).unwrap(),
                GateOutcome::Pass
            );
        }
    }
}

#[cfg(test)]
mod publication_descriptor_file_tests {
    use super::*;
    #[test]
    fn authoritative_file_is_bounded_regular_and_never_falls_back() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture
            .path()
            .join("plugins/gates/publication_size_guard_v1/gate.json");
        assert!(publication_source(Some(fixture.path())).is_err());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = publication_source(None).unwrap();
        std::fs::write(&path, &original).unwrap();
        let item: Manifest =
            serde_json::from_str(&publication_source(Some(fixture.path())).unwrap()).unwrap();
        validate_manifest(PUBLICATION_SIZE, &item).unwrap();
        std::fs::write(&path, vec![b' '; 64 * 1024 + 1]).unwrap();
        assert!(publication_source(Some(fixture.path())).is_err());
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("missing-target", &path).unwrap();
        assert!(publication_source(Some(fixture.path())).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(publication_source(Some(fixture.path())).is_err());
    }
}
