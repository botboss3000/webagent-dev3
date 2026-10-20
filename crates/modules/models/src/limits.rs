//! Per-model planning budgets with transport-specific context ceilings.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// Agent-owned planning budgets, pinned into every candidate at admission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelLimits {
    pub input_tokens: usize,
    pub output_tokens: usize,
}
impl ModelLimits {
    pub fn validate_for(&self, provider:crate::provider::ProviderId, model:&str)->Result<()> {
        self.validate(model)?;
        if let Some((context,_))=effective_limits(provider,model) {
            if self.input_tokens.checked_add(self.output_tokens).is_none_or(|n|n>context) {
                bail!("Model budgets exceed this transport's context window");
            }
        }
        Ok(())
    }
    pub fn validate(&self, model: &str) -> Result<()> {
        if self.input_tokens == 0 || self.input_tokens > 10_000_000
            || self.output_tokens == 0 || self.output_tokens > 1_000_000 {
            bail!("Invalid model input/output limits");
        }
        if let Some((context, output)) = published_limits(model) {
            if self.output_tokens > output
                || self.input_tokens.checked_add(self.output_tokens).is_none_or(|n| n > context) {
                bail!("Model input and output exceed the published context limits");
            }
        }
        Ok(())
    }
}
/// OpenAI model documentation, verified 2026-10-10. Context includes output.
pub fn published_limits(model: &str) -> Option<(usize, usize)> {
    match model {
        "gpt-6-luna" | "gpt-6.1-sol" | "gpt-6-astra" => Some((1_050_000, 128_000)),
        _ => None,
    }
}

#[cfg(test)]
mod model_limit_tests {
 use super::*;
 #[test]
 fn published_context_includes_output_and_invalid_limits_fail() {
  assert!(ModelLimits{input_tokens:922_000,output_tokens:128_000}.validate("gpt-6-luna").is_ok());
  assert!(ModelLimits{input_tokens:1_050_000,output_tokens:128_000}.validate("gpt-6-luna").is_err());
  assert!(ModelLimits{input_tokens:150_000,output_tokens:128_001}.validate("gpt-6-luna").is_err());
  assert!(ModelLimits{input_tokens:0,output_tokens:100}.validate("fixture").is_err());
 }
}

/// The current authenticated Codex subscription catalog advertises 272k for
/// these models (verified 2026-10-10). Public API and subscription limits differ.
pub fn effective_limits(provider:crate::provider::ProviderId,model:&str)->Option<(usize,usize)> {
    let published=published_limits(model)?;
    if provider==crate::provider::ProviderId::OpenaiSubscription {
        Some((272_000,published.1))
    } else {Some(published)}
}

#[cfg(test)]
mod transport_limit_tests {
 use super::*;
 #[test]
 fn subscription_and_public_api_windows_are_separate() {
  let limits=ModelLimits{input_tokens:263_808,output_tokens:8_192};
  limits.validate_for(crate::provider::ProviderId::OpenaiSubscription,"gpt-6-luna").unwrap();
  let public=ModelLimits{input_tokens:922_000,output_tokens:128_000};
  assert!(public.validate_for(crate::provider::ProviderId::OpenaiSubscription,"gpt-6-luna").is_err());
  public.validate_for(crate::provider::ProviderId::OpenaiApi,"gpt-6-luna").unwrap();
  let subscription=crate::provider::ProviderSelection::parse(&serde_json::json!({"provider":"openai_subscription","model":"gpt-6-luna"}),"unused").unwrap();
  assert_eq!(subscription.input_budget_bytes().unwrap(),Some(263_808*4));
 }
}
