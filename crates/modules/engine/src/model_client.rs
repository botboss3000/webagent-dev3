//! Static Engine context/projection composition over the actual Models transport.
use anyhow::Result;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
pub use webagent_models::provider::{ProviderId,ProviderSelection};
pub use webagent_models::transport::{ProviderFailure,typed_failure,projection_id};
pub use crate::context::instructions::DEFAULT_INSTRUCTIONS;
#[derive(Clone)]
pub struct Provider(pub(crate) webagent_models::transport::Provider);
impl std::ops::Deref for Provider {
    type Target=webagent_models::transport::Provider;
    fn deref(&self)->&Self::Target {&self.0}
}
impl Default for Provider {fn default()->Self {Self::new()}}
impl Provider {
    pub fn new()->Self {Self(webagent_models::transport::Provider::new())}
    #[cfg(any(test,feature="test-support"))]
    pub fn for_test(endpoint:String)->Self {Self(webagent_models::transport::Provider::for_test(endpoint))}
    pub fn transport(&self)->&webagent_models::transport::Provider {&self.0}
    pub fn with_bridge(self,queue:std::sync::Arc<dyn webagent_models::transport::BridgeQueue>)->Self {Self(self.0.with_bridge(queue))}
    pub fn instructions_for_turn(pinned:&str)->String {crate::context::instructions::instructions_for_turn(pinned)}
    pub fn instructions_for_snapshot(pinned:&str,root:&std::path::Path,source:Option<&std::path::Path>)->Result<String> {crate::context::instructions::instructions_for_snapshot(pinned,root,source)}
    pub fn configured_base_instructions()->String {crate::context::instructions::configured_base_instructions()}
}
struct EngineProjection<'a>(&'a std::path::Path);
impl webagent_models::transport::Projection for EngineProjection<'_> {
    fn project<'a>(&'a self,input:&'a webagent_provider_contracts::RequestV1,cancel:&'a CancellationToken)
        ->impl std::future::Future<Output=Result<webagent_provider_contracts::ResultV1>>+Send+'a {
        crate::components::provider_project(self.0,input,cancel)
    }
}
pub struct CallDraft<'a>(webagent_models::transport::CallDraft<'a>);
impl<'a> CallDraft<'a> {
    pub fn new(selection:&'a ProviderSelection,input:Vec<Value>,tools:Vec<Value>,session:&'a str,instructions:&'a str)->Self {
        Self(webagent_models::transport::CallDraft::new(selection,input,tools,session,instructions))
    }
    pub fn with_reasoning_effort(self,effort:Option<&'a str>)->Self {Self(self.0.with_reasoning_effort(effort))}
    pub async fn prepare(self,root:&std::path::Path,cancel:&CancellationToken)->Result<webagent_models::transport::call::ValidatedCall<'a>> {
        self.0.prepare(&EngineProjection(root),cancel).await
    }
    #[cfg(any(feature="linked-context",test))]
    pub fn validate(self)->Result<webagent_models::transport::call::ValidatedCall<'a>> {
        self.0.validate_with(webagent_provider_projection::project)
    }
}
