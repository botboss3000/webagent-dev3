//! Registered capability.prepare implementation over confined private pipes.
//! The host owns source reads, authorization and response correspondence checks.
// Confined capability.prepare service.
use serde_json::json;
use webagent_capability_contracts::PrepareRequest;

/// Start only the bounded pure engine protocol; the shared wrapper installs isolation.
fn main() {
    let result = webagent_engine_service::serve(16 * 1024 * 1024, |input| {
        let request: PrepareRequest = serde_json::from_value(input)?;
        let capability = webagent_capability_context::prepare(&request)?;
        Ok(
            json!({"status":"success", "implementation_version":webagent_capability_context::IMPLEMENTATION_VERSION, "capability":capability}),
        )
    });
    if result.is_err() {
        std::process::exit(1);
    }
}
