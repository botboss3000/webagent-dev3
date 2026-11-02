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
    /// A linked client (for example a local Claude Code) answers each call; no network endpoint.
    ClaudeBridge,
    /// A linked OpenCode harness answers each call; no network endpoint.
    OpencodeBridge,
    /// A linked Muse harness answers each call; no network endpoint.
    MuseBridge,
}

impl ProviderId {
    /// A linked client answers these providers' calls through the host's harness bridge queue.
    pub fn is_bridge(self) -> bool {
        matches!(
            self,
            Self::ClaudeBridge | Self::OpencodeBridge | Self::MuseBridge
        )
    }
    /// Installation-owned endpoints only. Client catalog payloads cannot override these.
    pub fn chat_endpoint(self) -> Option<&'static str> {
        match self {
            Self::OpenaiSubscription
            | Self::Typesafe
            | Self::ClaudeBridge
            | Self::OpencodeBridge
            | Self::MuseBridge => None,
            Self::Deepseek => Some("https://api.deepseek.com/chat/completions"),
            Self::OpenaiApi => Some("https://api.openai.com/v1/chat/completions"),
            Self::Openrouter => Some("https://openrouter.ai/api/v1/chat/completions"),
            Self::Abacus => Some("https://routellm.abacus.ai/v1/chat/completions"),
            Self::Gemini => {
                Some("https://generativelanguage.googleapis.com/v1beta/openai/chat/completions")
            }
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
/// Saved selections may still carry the retired `limits` key; it is ignored.
pub struct ProviderSelection {
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
        Ok(Self {
            provider,
            model: model.to_owned(),
        })
    }

    /// Compaction budget in the four-byte estimate: the provider catalog's context window
    /// less an 8,192-token output reserve. `None` when the catalog lists no context.
    /// This sizes compaction only; calls are not rejected by it (the provider decides).
    pub fn input_budget_bytes(&self) -> Result<Option<usize>> {
        crate::providers::context(self.provider, &self.model)
            .map(|context| {
                context
                    .saturating_sub(8_192)
                    .checked_mul(4)
                    .ok_or_else(|| anyhow!("Model input budget overflow"))
            })
            .transpose()
    }

    /// Serialize the pinned provider/model identity for durable selection plans.
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
        // Selections saved before per-agent limits were retired still load.
        let saved: ProviderSelection = serde_json::from_value(
            json!({"provider":"deepseek","model":"m","limits":{"input_tokens":1,"output_tokens":1}}),
        )
        .unwrap();
        assert_eq!(saved.value(), json!({"provider":"deepseek","model":"m"}));
        let parsed =
            ProviderSelection::parse(&json!({"model":"m","limits":null}), "default").unwrap();
        assert_eq!(parsed.model, "m");
        // The compaction budget follows the catalog context window, less the output reserve.
        let luna = ProviderSelection::parse(&json!({"model":"gpt-6-luna"}), "default").unwrap();
        assert_eq!(luna.input_budget_bytes().unwrap(), Some(263_808 * 4));
        assert_eq!(parsed.input_budget_bytes().unwrap(), None);
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
        assert!(ProviderSelection::parse(
            &json!({"provider":"deepseek","model":"organization/model-name"}),
            "default"
        )
        .is_ok());
        for model in ["model\n", "model name", "model?token=x"] {
            assert!(ProviderSelection::parse(
                &json!({"provider":"deepseek","model":model}),
                "default"
            )
            .is_err());
        }
    }
}

pub use crate::transport::{projection_id, typed_failure, CallDraft, Provider, ProviderFailure};

#[cfg(test)]
pub use crate::transport::DEFAULT_INSTRUCTIONS;
