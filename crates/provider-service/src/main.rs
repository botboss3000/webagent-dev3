//! Thin isolated provider planner; credentials and network remain in the host.
// Private-pipe provider.project service; Calls engine-service isolation and provider-projection project.
/// Serve bounded pure projections after engine-service installs isolation; reject malformed frames without private diagnostics.
fn main() {
    let result =
        webagent_engine_service::serve(webagent_provider_contracts::MAX_FRAME_BYTES, |value| {
            let request: webagent_provider_contracts::RequestV1 = serde_json::from_value(value)?;
            let projection = webagent_provider_projection::project(&request)?;
            Ok(serde_json::json!({
                "status":"success",
                "implementation_version":webagent_provider_projection::IMPLEMENTATION_VERSION,
                "projection":projection,
            }))
        });
    if result.is_err() {
        std::process::exit(1);
    }
}
