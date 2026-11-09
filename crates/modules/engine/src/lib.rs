//! Statically linked Engine execution and Context composition over existing owner resources.
#[cfg(all(feature = "linked-context", feature = "live-context"))]
compile_error!("Choose exactly one engine mode: linked-context or live-context");
#[cfg(not(any(feature = "linked-context", feature = "live-context")))]
compile_error!("Choose an engine mode: linked-context or live-context");
pub mod components;
pub mod context_ledger;
pub mod defaults;
pub mod gates;
pub mod model_client;
pub mod output;
pub mod ports;
pub mod publication;
pub mod repository;
pub mod runtime;
pub mod sandbox;
pub mod wake;
pub mod workflow;
pub mod context {
    pub mod abilities;
    pub mod accounting;
    pub mod assembly;
    pub mod assets;
    pub(crate) mod bridge;
    pub mod compaction;
    pub mod input;
    pub mod instructions;
    pub mod memory;
    pub mod retained_recall;
    pub mod retention;
}
pub mod tools {
    pub mod dispatch;
    pub mod execution;
    pub mod failure;
    pub mod outcome;
    pub mod retry_guard;
    pub mod transport;
}
pub use context::{
    accounting as context_accounting, assembly as context_assembly, assets as context_assets,
    compaction as context_compaction, input as context_input, retention as tool_output,
};
pub use defaults as live_assets;
pub use gates as gate_registry;
pub use model_client as provider;
pub use webagent_configuration::digest;
// rustc #100013: preserve generic owner futures' Send contract without wrapping or allocating.
pub(crate) fn send_future<F: std::future::Future + Send>(
    future: F,
) -> impl std::future::Future<Output = F::Output> + Send {
    future
}
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
pub fn key(prefix: &str, id: &str) -> String {
    format!("{prefix}:{id}")
}
pub fn status(run: &serde_json::Value) -> &str {
    run["status"].as_str().unwrap_or("failed")
}
#[cfg(test)]
mod repository_tests;

pub fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}
