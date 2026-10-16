//! Thin private-pipe asset planner; the shared wrapper owns framing and confinement.
/// Dispatch typed pure requests; no host store, credentials or authority enter this process.
fn main() {
 let result=webagent_engine_service::serve(webagent_assets_contracts::MAX_BYTES,|value| {
  let request:webagent_assets_contracts::Request=serde_json::from_value(value)?;
  let plan=webagent_context_assets::plan(&request)?;
  Ok(serde_json::json!({"status":"success","implementation_version":webagent_context_assets::IMPLEMENTATION_VERSION,"plan":plan}))
 });
 if result.is_err(){std::process::exit(1);}
}
