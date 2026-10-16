//! Registered capability.prepare implementation over confined private pipes.
//! The host owns source reads, authorization and response correspondence checks.
// @code-index {"version":1,"feature":"capability-composition","aliases":["live context","source correspondence"],"purpose":"Confined capability.prepare service. Config: plugins/engine/capability-context/component.json -> limits and runtime_requirements govern host admission; the pure callback does not read configuration.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"plugins/engine/capability-context/component.json","pointers":["/limits/max_request_bytes","/limits/max_response_bytes","/limits/timeout_ms","/limits/max_concurrency","/runtime_requirements"]}],"opportunities":[]}}
use serde_json::json;
use webagent_capability_contracts::PrepareRequest;

/// Start only the bounded pure engine protocol; the shared wrapper installs isolation.
fn main() {
    let result = webagent_engine_service::serve(16 * 1024 * 1024, |input| {
        let request: PrepareRequest = serde_json::from_value(input)?;
        let capability = webagent_capability_context::prepare(&request)?;
        Ok(json!({"status":"success", "implementation_version":webagent_capability_context::IMPLEMENTATION_VERSION, "capability":capability}))
    });
    if result.is_err() { std::process::exit(1); }
}
