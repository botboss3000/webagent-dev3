//! Thin metadata-only background planner service; authority stays in the host.
fn main() {
 let result=webagent_engine_service::serve(65536,|value| {
  let input:webagent_background_contracts::Request=serde_json::from_value(value)?;
  let plan=webagent_background_dispatch::plan(&input)?;
  Ok(serde_json::json!({"status":"success","implementation_version":webagent_background_dispatch::IMPLEMENTATION_VERSION,"plan":plan}))
 });
 if result.is_err(){std::process::exit(1);}
}
