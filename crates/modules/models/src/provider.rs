//! Validated provider identity and selection. Credentials and network I/O remain host-owned.
use anyhow::{anyhow, bail, Result};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    OpenaiSubscription,
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
    Typesafe,
}


impl ProviderId {
    /// Installation-owned endpoints only. Client catalog payloads cannot override these.
    pub fn chat_endpoint(self) -> Option<&'static str> {
        match self {
            Self::OpenaiSubscription | Self::Typesafe => None,
            Self::Deepseek => Some("https://api.deepseek.com/chat/completions"),
            Self::OpenaiApi => Some("https://api.openai.com/v1/chat/completions"),
            Self::Openrouter => Some("https://openrouter.ai/api/v1/chat/completions"),
            Self::Abacus => Some("https://routellm.abacus.ai/v1/chat/completions"),
            Self::Gemini => Some("https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"),
            Self::Groq => Some("https://api.groq.com/openai/v1/chat/completions"),
            Self::Together => Some("https://api.together.xyz/v1/chat/completions"),
            Self::Mistral => Some("https://api.mistral.ai/v1/chat/completions"),
            Self::Fireworks => Some("https://api.fireworks.ai/inference/v1/chat/completions"),
            Self::Xai => Some("https://api.x.ai/v1/chat/completions"),
            Self::Perplexity => Some("https://api.perplexity.ai/chat/completions"),
            Self::Deepinfra => Some("https://api.deepinfra.com/v1/openai/chat/completions"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProviderSelection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<crate::limits::ModelLimits>,
    pub provider: ProviderId,
    pub model: String,
}

impl ProviderSelection {
    /// Model-only legacy settings select the original subscription adapter.
    /// Explicit invalid provider values must never silently select a transport.
    pub fn parse(value: &Value, default_model: &str) -> Result<Self> {
        if !value.is_object() {
            bail!("Invalid provider selection");
        }
        let provider = match value.get("provider") {
            None => ProviderId::OpenaiSubscription,
            Some(value) => serde_json::from_value(value.clone())
                .map_err(|_| anyhow!("Unsupported provider identifier"))?,
        };
        let model = match value.get("model") {
            None => default_model,
            Some(value) => value.as_str().ok_or_else(|| anyhow!("Invalid model"))?,
        };
        if !crate::valid_model(model) {
            bail!("Invalid model");
        }
        let limits: Option<crate::limits::ModelLimits> = value.get("limits")
            .filter(|v| !v.is_null()).map(|v| serde_json::from_value(v.clone())).transpose()?;
        if let Some(limits) = &limits { limits.validate_for(provider, model)?; }
        Ok(Self {
            limits,
            provider,
            model: model.to_owned(),
        })
    }

    /// Convert validated model input tokens to the existing four-byte estimate, rejecting overflow; this is not provider tokenization.
    pub fn input_budget_bytes(&self) -> Result<Option<usize>> {
        let input = if let Some(limits) = &self.limits {
            limits.validate_for(self.provider, &self.model)?;
            Some(limits.input_tokens)
        } else if matches!(self.provider, ProviderId::OpenaiSubscription | ProviderId::OpenaiApi) {
            crate::limits::effective_limits(self.provider, &self.model).map(|(context, _)| context-8_192)
        } else { None };
        // Match the existing compaction estimate. This is not a provider tokenizer.
        input.map(|n| n.checked_mul(4).ok_or_else(|| anyhow!("Model input budget overflow"))).transpose()
    }

    /// Serialize the pinned provider/model/limits identity for durable selection plans.
    pub fn value(&self) -> Value {
        serde_json::to_value(self).expect("Provider selection serialization")
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use serde_json::json;
    /// Verify only an omitted provider selects subscription compatibility; explicit unsupported values fail.
    #[test]
    fn legacy_selection_defaults_only_missing_provider() {
        let selection =
            ProviderSelection::parse(&json!({"model":"legacy-model"}), "default").unwrap();
        assert_eq!(selection.provider, ProviderId::OpenaiSubscription);
        assert_eq!(
            selection.value(),
            json!({"provider":"openai_subscription","model":"legacy-model"})
        );
        assert_eq!(
            ProviderSelection::parse(&json!({}), "default")
                .unwrap()
                .model,
            "default"
        );
        for provider in [json!("other"), json!(null), json!(7), json!("")] {
            assert!(ProviderSelection::parse(
                &json!({"provider":provider,"model":"valid"}),
                "default"
            )
            .is_err());
        }
    }
}

#[cfg(test)]
mod upstream_id_tests {
    /// Accept vendor/model identifiers while rejecting whitespace, control characters and URL query syntax.
    #[test]
    fn selection_accepts_namespaced_models_without_accepting_controls() {
        use super::ProviderSelection;
        use serde_json::json;
        assert!(ProviderSelection::parse(&json!({"provider":"deepseek","model":"organization/model-name"}), "default").is_ok());
        for model in ["model\n", "model name", "model?token=x"] {
            assert!(ProviderSelection::parse(&json!({"provider":"deepseek","model":model}), "default").is_err());
        }
    }
}
