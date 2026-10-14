//! Thin reusable confined pipe wrapper; no Docker, persistence or host credentials.
fn main() {
 let result=webagent_engine_service::serve(65536,|value| {
  let input:webagent_boot_contracts::Request=serde_json::from_value(value)?;
  let plan=webagent_boot_supervisor::plan(&input)?;
  Ok(serde_json::json!({"status":"success","implementation_version":webagent_boot_supervisor::IMPLEMENTATION_VERSION,"plan":plan}))
 });
 if result.is_err(){std::process::exit(1);}
}
