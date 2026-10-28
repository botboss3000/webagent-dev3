//! Model-discovery module service process: answers the host's `ModelDiscovery` calls.
use std::sync::Arc;
use webagent_module_service::{Limits, Service};
use webagent_model_discovery::Discovery;
use webagent_models::discovery::ModelDiscoveryServer;

fn main() {
    let result = webagent_module_service::run(
        Limits::default(),
        Arc::new(|_| vec![Arc::new(ModelDiscoveryServer(Arc::new(Discovery))) as Arc<dyn Service>]),
    );
    if result.is_err() {
        std::process::exit(1);
    }
}
