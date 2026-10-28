//! The model-discovery service process answers exactly as the linked implementation.
use serde_json::{json, Value};
use std::sync::Arc;
use webagent_model_discovery::Discovery;
use webagent_models::discovery::{ModelDiscovery, ModelDiscoveryClient};
use webagent_models::provider::ProviderId;
use webagent_module_service::{Limits, Process, Services};

async fn exercise(discovery: &impl ModelDiscovery) -> Vec<Value> {
    let rows = vec![
        json!({"id": "vendor/gpt-x", "api_key": "must-not-project"}),
        json!({"id": "vendor/gpt-x"}),
        json!({"id": "bad\nmodel"}),
        json!({"id": "imagen-3", "supportedGenerationMethods": ["generateImages"]}),
    ];
    let models_dev = json!({"openai": {"models": {"gpt-x": {"modalities": {"input": ["text", "image"], "output": ["text"]}, "limit": {"context": 1000}}}}});
    let openrouter = json!({"data": [{"id": "imagen-3", "description": "Images"}]});
    let catalog = json!({"models": [{"slug": "new-gpt", "visibility": "list"}, {"slug": "hidden", "visibility": "hide"}]});
    vec![
        json!(discovery.provider_models(ProviderId::OpenaiApi, &rows, &models_dev, &openrouter).await.unwrap()),
        json!(discovery.provider_models(ProviderId::Openrouter, &[], &Value::Null, &Value::Null).await.unwrap()),
        discovery.subscription_models(&catalog).await.unwrap(),
        json!(discovery.subscription_models(&json!({})).await.is_err()),
    ]
}

#[tokio::test]
async fn service_process_matches_linked_discovery() {
    let command = tokio::process::Command::new(env!("CARGO_BIN_EXE_webagent-models-service"));
    let no_host_services: Services = Arc::new(|_| Vec::new());
    let process = Process::spawn(command, Limits::default(), &no_host_services)
        .expect("model discovery service starts");
    let live = exercise(&ModelDiscoveryClient(process.peer().clone())).await;
    let linked = exercise(&Discovery).await;
    assert_eq!(live, linked);
    assert!(!live[0].to_string().contains("must-not-project"));
    assert_eq!(live[0].as_array().unwrap().len(), 2);
}
