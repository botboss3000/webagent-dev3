//! Model discovery contract. The host fetches provider listings with the caller's
//! authorized credential and the public metadata catalogs; `webagent-model-discovery`
//! normalizes them, linked into stable builds and as its own service process
//! (`model-discovery`) in dev builds. The service never sees a credential.
use crate::provider::ProviderId;
use serde_json::Value;
use webagent_module_service::module_interface;

#[module_interface(version = 1)]
pub trait ModelDiscovery {
    /// Public capability rows for one provider's `/models` listing rows: at most 256
    /// unique valid ids in listing order, enriched from the models.dev and OpenRouter catalogs.
    async fn provider_models(&self,provider:ProviderId,rows:&[Value],models_dev:&Value,openrouter:&Value)->anyhow::Result<Vec<Value>>;
    /// Visible ChatGPT subscription models in server order, from the Codex model catalog.
    async fn subscription_models(&self,catalog:&Value)->anyhow::Result<Value>;
}
