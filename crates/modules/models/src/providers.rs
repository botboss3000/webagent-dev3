//! Provider and model catalogs: one `providers/<provider>.json` file per provider,
//! beside this crate. They are live files (listed in `live-files.json`), never
//! compiled in: every lookup reads the checkout, so the next request or model call
//! sees an edit.
use crate::provider::ProviderId;
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};

/// The full reasoning-effort scale, lowest first. Files may use any subset.
pub const EFFORTS: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];
const FILE_LIMIT: u64 = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output: Option<u64>,
    /// Absent inherits the provider scale; an empty list means no effort control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub efforts: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub provider: ProviderId,
    pub name: String,
    pub base_url: String,
    /// Unvalidated providers are listed but cannot be selected.
    pub validated: bool,
    /// Scale for models this file does not list (for example a newly released model).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub efforts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
    #[serde(default)]
    pub models: Vec<Model>,
}

/// Supported effort levels for one model, lowest first, and the provider's own default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Efforts {
    pub levels: Vec<String>,
    pub default: Option<String>,
}

impl Provider {
    pub fn efforts(&self, model: &str) -> Efforts {
        let listed = self.models.iter().find(|row| row.id == model);
        match listed.and_then(|row| row.efforts.clone()) {
            Some(levels) => Efforts { levels, default: listed.and_then(|row| row.default_effort.clone()) },
            None => Efforts { levels: self.efforts.clone(), default: self.default_effort.clone() },
        }
    }
    fn validate(&self, id: &str) -> Result<()> {
        if self.provider_id() != id { bail!("Provider file name does not match its provider"); }
        if self.name.trim().is_empty() || self.name.len() > 80 || self.base_url.len() > 200 {
            bail!("Invalid provider name or base URL");
        }
        scale(&self.efforts, self.default_effort.as_deref())?;
        let mut seen = std::collections::BTreeSet::new();
        for model in &self.models {
            if !crate::valid_model(&model.id) || !seen.insert(&model.id) {
                bail!("Invalid or duplicate model {}", model.id);
            }
            if model.name.as_ref().is_some_and(|name| name.trim().is_empty() || name.len() > 120) {
                bail!("Invalid name for model {}", model.id);
            }
            match &model.efforts {
                Some(levels) => scale(levels, model.default_effort.as_deref())?,
                None if model.default_effort.is_some() => bail!("Model {} sets a default without efforts", model.id),
                None => {},
            }
        }
        Ok(())
    }
    fn provider_id(&self) -> String {
        serde_json::to_value(self.provider).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
    }
}

/// Levels must be known, unique and ordered; a default must be one of them.
fn scale(levels: &[String], default: Option<&str>) -> Result<()> {
    let ranks: Vec<usize> = levels.iter()
        .map(|level| EFFORTS.iter().position(|known| known == level).ok_or_else(|| anyhow!("Unknown effort level {level}")))
        .collect::<Result<_>>()?;
    if ranks.windows(2).any(|pair| pair[0] >= pair[1]) { bail!("Effort levels must be unique and ordered lowest first"); }
    if default.is_some_and(|default| !levels.iter().any(|level| level == default)) {
        bail!("Default effort is not one of the listed levels");
    }
    Ok(())
}

const FOLDER: &str = "crates/modules/models/providers";

fn read(name: &str, id: &str) -> Result<Provider> {
    let bytes = webagent_configuration::live::read(&format!("{FOLDER}/{name}"), FILE_LIMIT, |bytes| {
        let provider: Provider = serde_json::from_slice(bytes)?;
        provider.validate(id)
    }).map_err(|error| anyhow!("Invalid provider file {id}: {error:#}"))?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// One provider's file, or `None` when the provider has none.
pub fn load(provider: ProviderId) -> Result<Option<Provider>> {
    let id = serde_json::to_value(provider)?.as_str().unwrap_or_default().to_owned();
    let name = format!("{id}.json");
    if !webagent_configuration::live::list(FOLDER)?.contains(&name) { return Ok(None); }
    read(&name, &id).map(Some)
}

/// Every provider file, validated providers first, then by name.
pub fn load_all() -> Result<Vec<Provider>> {
    let mut providers = Vec::new();
    for name in webagent_configuration::live::list(FOLDER)? {
        let Some(id) = name.strip_suffix(".json") else { continue };
        providers.push(read(&name, id)?);
    }
    providers.sort_by(|a, b| b.validated.cmp(&a.validated).then_with(|| a.name.cmp(&b.name)));
    Ok(providers)
}

/// Supported levels for a model; empty when the provider has no file.
pub fn efforts(provider: ProviderId, model: &str) -> Result<Efforts> {
    Ok(load(provider)?.map(|file| file.efforts(model)).unwrap_or_default())
}

/// Keep a requested level only if the model currently supports it, so an edited
/// file degrades a saved preference to the provider default instead of failing a call.
pub fn supported(provider: ProviderId, model: &str, effort: Option<&str>) -> Option<String> {
    let effort = effort?;
    efforts(provider, model).ok()?.levels.into_iter().find(|level| level == effort)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shipped_files_load_and_resolve_model_and_provider_scales() {
        let all = load_all().unwrap();
        assert!(all.len() >= 14);
        let validated: Vec<_> = all.iter().filter(|p| p.validated).map(|p| p.provider).collect();
        assert!(validated.contains(&ProviderId::Deepseek) && validated.contains(&ProviderId::OpenaiSubscription));
        // The Claude Bridge file is a live file added after its code; check it once present.
        if validated.contains(&ProviderId::ClaudeBridge) {
            let bridge = efforts(ProviderId::ClaudeBridge, "claude-opus-5-5").unwrap();
            assert_eq!(bridge.levels, ["low", "medium", "high", "xhigh", "max"]);
            assert_eq!(bridge.default.as_deref(), Some("medium"));
        }
        let flash = efforts(ProviderId::Deepseek, "deepseek-flash").unwrap();
        assert_eq!(flash.levels, ["none", "low", "high", "max"]);
        assert_eq!(flash.default.as_deref(), Some("high"));
        // A subscription model missing from the file uses the provider scale.
        let unlisted = efforts(ProviderId::OpenaiSubscription, "future-model").unwrap();
        assert_eq!(unlisted.levels, ["low", "medium", "high", "xhigh"]);
        assert_eq!(unlisted.default.as_deref(), Some("medium"));
        assert!(efforts(ProviderId::Openrouter, "any").unwrap().levels.is_empty());
        assert_eq!(supported(ProviderId::Deepseek, "deepseek-flash", Some("max")).as_deref(), Some("max"));
        assert_eq!(supported(ProviderId::Deepseek, "deepseek-flash", Some("medium")), None);
        assert_eq!(supported(ProviderId::Deepseek, "deepseek-flash", None), None);
    }
    #[test]
    fn invalid_files_are_rejected() {
        let parse = |text: &str| serde_json::from_str::<Provider>(text).map_err(anyhow::Error::from).and_then(|p| p.validate("deepseek").map(|_| p));
        let base = |extra: &str| format!(r#"{{"provider":"deepseek","name":"DeepSeek","base_url":"x","validated":true,"models":[{extra}]}}"#);
        assert!(parse(&base(r#"{"id":"m","efforts":["low","high"],"default_effort":"high"}"#)).is_ok());
        assert!(parse(&base(r#"{"id":"m","efforts":["high","low"]}"#)).is_err());
        assert!(parse(&base(r#"{"id":"m","efforts":["turbo"]}"#)).is_err());
        assert!(parse(&base(r#"{"id":"m","efforts":["low"],"default_effort":"high"}"#)).is_err());
        assert!(parse(&base(r#"{"id":"m","default_effort":"high"}"#)).is_err());
        assert!(parse(&base(r#"{"id":"m"},{"id":"m"}"#)).is_err());
        assert!(parse(&base(r#"{"id":"m","unknown":1}"#)).is_err());
        assert!(parse(&base("").replace("deepseek\",\"name", "openrouter\",\"name")).is_err());
    }
}
