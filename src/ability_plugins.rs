//! Thin composition of current Abilities catalog and existing trusted worker transport.
use crate::*;
pub(crate) use webagent_abilities::package::{Package,AutomationService,automation_service};
pub(crate) fn current_schemas()->Vec<Value> {
 static EMPTY:std::sync::OnceLock<Arc<abilities::Catalog>>=std::sync::OnceLock::new();
 let catalog=match crate::live_assets::catalog(EMPTY.get_or_init(||Arc::new(abilities::Catalog::empty()))) {
  Ok(value)=>value,Err(_)=>{tracing::error!("Ability catalog unavailable");return Vec::new();}
 };
 catalog.all_schemas().into_iter().filter(|s|catalog.plugin_binding(s["name"].as_str().unwrap_or("")).is_some()).collect()
}
pub(crate) async fn execute(app:&App,sid:&str,name:&str,args:&Value,invocation:&str,binding:&Value,cancel:CancellationToken)->Result<Value> {
 let current=crate::live_assets::catalog(&app.abilities)?;
 match webagent_abilities::package::prepare_invocation(&current,name,args,invocation,binding)? {
  webagent_abilities::package::Invocation::Denied(value)=>Ok(value),
  webagent_abilities::package::Invocation::Ready(trusted)=>crate::tools::execute_trusted("ability_plugin_invoke",args,sid,&app.tool_socket,Some(trusted),cancel).await,
 }
}

#[cfg(test)]
mod tests {
 #[test] fn closed_schema_rejects_unknown_and_wrong_types(){webagent_abilities::plugins_testing::tests::closed_schema_rejects_unknown_and_wrong_types::<crate::abilities::NativeHost>();}
}
#[cfg(test)]
mod catalog_tests {
 #[test] fn new_tools_and_guidance_are_loaded_without_native_registration(){webagent_abilities::plugins_testing::catalog_tests::new_tools_and_guidance_are_loaded_without_native_registration::<crate::abilities::NativeHost>();}
 #[test] fn helper_and_runtime_asset_updates_change_generation_while_legacy_bindings_remain_verifiable(){webagent_abilities::plugins_testing::catalog_tests::helper_and_runtime_asset_updates_change_generation_while_legacy_bindings_remain_verifiable::<crate::abilities::NativeHost>();}
 #[test] fn pending_packages_withdraw_bindings_and_cannot_declare_trusted_core_kind(){webagent_abilities::plugins_testing::catalog_tests::pending_packages_withdraw_bindings_and_cannot_declare_trusted_core_kind::<crate::abilities::NativeHost>();}
 #[test] fn runtime_asset_declarations_do_not_change_the_compile_cache_key(){webagent_abilities::plugins_testing::catalog_tests::runtime_asset_declarations_do_not_change_the_compile_cache_key::<crate::abilities::NativeHost>();}
 #[test] fn incompatible_protocol_and_core_schema_collision_are_rejected(){webagent_abilities::plugins_testing::catalog_tests::incompatible_protocol_and_core_schema_collision_are_rejected::<crate::abilities::NativeHost>();}
}
#[cfg(test)]
mod diagnostics_native_contract_tests {
 #[test] fn diagnostics_retains_native_authority_and_rejects_incompatible_packages(){webagent_abilities::plugins_testing::diagnostics_native_contract_tests::diagnostics_retains_native_authority_and_rejects_incompatible_packages::<crate::abilities::NativeHost>();}
}
#[cfg(test)]
mod native_automation_contract_tests {
 #[test] fn native_registration_rejects_host_and_tool_escalation(){webagent_abilities::plugins_testing::native_automation_contract_tests::native_registration_rejects_host_and_tool_escalation::<crate::abilities::NativeHost>();}
 #[test] fn native_inputs_reject_filesystem_substitution(){webagent_abilities::plugins_testing::native_automation_contract_tests::native_inputs_reject_filesystem_substitution::<crate::abilities::NativeHost>();}
 #[test] fn native_artifact_and_assets_have_independent_version_bindings(){webagent_abilities::plugins_testing::native_automation_contract_tests::native_artifact_and_assets_have_independent_version_bindings::<crate::abilities::NativeHost>();}
}
