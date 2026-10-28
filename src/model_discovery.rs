//! The model-discovery module: linked into stable (`linked-context`) builds and run
//! as its own service process from the published engine catalog in dev
//! (`live-context`) builds. Model routes authorize the caller and fetch the
//! listing first; the service receives only listing data, never a credential.
use anyhow::Result;
use serde_json::Value;
use webagent_models::discovery::ModelDiscovery;
use webagent_models::provider::ProviderId;

#[cfg(feature = "linked-context")]
struct Handle(webagent_model_discovery::Discovery);
#[cfg(feature = "linked-context")]
impl Handle {
    async fn open() -> Result<Self> { Ok(Self(webagent_model_discovery::Discovery)) }
    fn discovery(&self) -> &impl ModelDiscovery { &self.0 }
}

/// Holding the process keeps an admitted call on the generation it started on.
#[cfg(feature = "live-context")]
struct Handle(#[allow(dead_code)] std::sync::Arc<webagent_module_service::Process>, webagent_models::discovery::ModelDiscoveryClient);
#[cfg(feature = "live-context")]
impl Handle {
    async fn open() -> Result<Self> {
        let process = crate::module_services::supervised("model-discovery")?.current().await?;
        let client = webagent_models::discovery::ModelDiscoveryClient(process.peer().clone());
        Ok(Self(process, client))
    }
    fn discovery(&self) -> &impl ModelDiscovery { &self.1 }
}

/// Opens the module only when a route actually calls it, after authorization and fetch.
pub(crate) struct Service;
impl ModelDiscovery for Service {
    async fn provider_models(&self, provider: ProviderId, rows: &[Value], models_dev: &Value, openrouter: &Value) -> Result<Vec<Value>> {
        Handle::open().await?.discovery().provider_models(provider, rows, models_dev, openrouter).await
    }
    async fn subscription_models(&self, catalog: &Value) -> Result<Value> {
        Handle::open().await?.discovery().subscription_models(catalog).await
    }
}
