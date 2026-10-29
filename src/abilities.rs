//! Abilities infrastructure composition; native/domain implementations remain with their owners.
use anyhow::Result;
use serde_json::Value;
use std::path::PathBuf;
#[derive(Clone)]
pub(crate) struct NativeHost;
pub(crate) type Catalog = webagent_abilities::Catalog<NativeHost>;
pub(crate) use webagent_abilities::{
    is_discovery, near_word, public_context, search_words, AbilityContext, AbilityInclusion,
    SkillInclusion,
};
impl webagent_abilities::Host for NativeHost {
    fn selected_source() -> Result<Option<PathBuf>> {
        webagent_configuration::prompts::default_source(None)
    }
    fn native_schemas_for_catalog(root: &std::path::Path, publication: bool) -> Result<Vec<Value>> {
        crate::live_assets::native_schemas_for_catalog(root, publication)
    }
    fn native_schemas_without_administrator() -> Vec<Value> {
        crate::tools::native_schemas_without_administrator()
    }
    fn native_schemas() -> Vec<Value> {
        crate::tools::native_schemas()
    }
    fn administrator_supported(name: &str) -> bool {
        crate::tools::administrator::supported_schema_name(name)
    }
    fn module_known(name: &str) -> bool {
        crate::modules::package_root(name).is_some()
    }
    fn module_available(name: &str) -> bool {
        crate::modules::available(name)
    }
    fn optimization_folder() -> PathBuf {
        crate::optimization::folder()
    }
    fn optimization_declaration() -> Result<Option<(String, Value)>> {
        crate::optimization::declaration()
    }
    fn optimization_available() -> bool {
        crate::optimization::available()
    }
    fn optimization_generation() -> Option<String> {
        crate::optimization::generation()
    }
    fn optimization_guidance() -> Option<String> {
        crate::optimization::guidance()
    }
    fn mcp_folder() -> PathBuf {
        crate::mcp::bridge::folder()
    }
    fn mcp_available() -> bool {
        crate::mcp::bridge::available()
    }
    fn mcp_schemas() -> Vec<Value> {
        crate::mcp::all_schemas()
    }
    fn mcp_guidance() -> &'static str {
        crate::mcp::GUIDANCE
    }
    fn drift() -> std::sync::Arc<webagent_abilities::drift::Drift> {
        crate::ability_drift::current()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn index_guidance_is_confined_to_registered_published_assets() {
        webagent_abilities::catalog_testing::tests::index_guidance_is_confined_to_registered_published_assets::<crate::abilities::NativeHost>();
    }
    #[test]
    fn visible_large_guidance_is_complete_and_discovery_is_not_activation() {
        webagent_abilities::catalog_testing::tests::visible_large_guidance_is_complete_and_discovery_is_not_activation::<crate::abilities::NativeHost>();
    }
    #[test]
    fn discoverable_activation_is_durable_without_reloading_or_revealing_discoverable_tools() {
        webagent_abilities::catalog_testing::tests::discoverable_activation_is_durable_without_reloading_or_revealing_discoverable_tools::<crate::abilities::NativeHost>();
    }
    #[test]
    fn diagnostic_projection_rejects_content_and_unknown_identifiers() {
        webagent_abilities::catalog_testing::tests::diagnostic_projection_rejects_content_and_unknown_identifiers::<crate::abilities::NativeHost>();
    }
    #[test]
    fn search_activation_preserves_visible_tools_and_respects_denials() {
        webagent_abilities::catalog_testing::tests::search_activation_preserves_visible_tools_and_respects_denials::<crate::abilities::NativeHost>();
    }
    #[test]
    fn natural_task_search_finds_permitted_ability_and_tools() {
        webagent_abilities::catalog_testing::tests::natural_task_search_finds_permitted_ability_and_tools::<crate::abilities::NativeHost>();
    }
    #[test]
    fn changed_manifest_withdraws_cached_loaded_tools_until_reload() {
        webagent_abilities::catalog_testing::tests::changed_manifest_withdraws_cached_loaded_tools_until_reload::<crate::abilities::NativeHost>();
    }
    #[test]
    fn bundled_skill_access_is_exact_scoped_and_withdraws_changed_assets() {
        webagent_abilities::catalog_testing::tests::bundled_skill_access_is_exact_scoped_and_withdraws_changed_assets::<crate::abilities::NativeHost>();
    }
    #[test]
    fn symlink_replacement_cannot_restore_cached_bundled_guidance() {
        webagent_abilities::catalog_testing::tests::symlink_replacement_cannot_restore_cached_bundled_guidance::<crate::abilities::NativeHost>();
    }
    #[test]
    fn model_index_advertises_discoverable_abilities_without_hidden_tools() {
        webagent_abilities::catalog_testing::tests::model_index_advertises_discoverable_abilities_without_hidden_tools::<crate::abilities::NativeHost>();
    }
    #[test]
    fn manifest_and_skill_are_loaded_without_exposing_denied_tools() {
        webagent_abilities::catalog_testing::tests::manifest_and_skill_are_loaded_without_exposing_denied_tools::<crate::abilities::NativeHost>();
    }
    #[test]
    fn configuration_skill_rejects_unknown_and_reads_current_assets() {
        webagent_abilities::catalog_testing::tests::configuration_skill_rejects_unknown_and_reads_current_assets::<crate::abilities::NativeHost>();
    }
    #[test]
    fn duplicate_tools_fail_closed() {
        webagent_abilities::catalog_testing::tests::duplicate_tools_fail_closed::<
            crate::abilities::NativeHost,
        >();
    }
    #[test]
    fn missing_catalog_or_ability_removes_tools_from_runtime() {
        webagent_abilities::catalog_testing::tests::missing_catalog_or_ability_removes_tools_from_runtime::<crate::abilities::NativeHost>();
    }
    #[test]
    fn partial_readiness_exposes_only_compiled_tools_and_withdraws_on_edit() {
        webagent_abilities::catalog_testing::tests::partial_readiness_exposes_only_compiled_tools_and_withdraws_on_edit::<crate::abilities::NativeHost>();
    }
    #[test]
    fn catalog_only_v2_tool_is_listed_but_never_executable() {
        webagent_abilities::catalog_testing::tests::catalog_only_v2_tool_is_listed_but_never_executable::<crate::abilities::NativeHost>();
    }
    #[test]
    fn tool_gate_distinguishes_missing_tools_from_hidden_tools() {
        webagent_abilities::catalog_testing::tests::tool_gate_distinguishes_missing_tools_from_hidden_tools::<crate::abilities::NativeHost>();
    }
    #[test]
    fn catalog_version_changes_with_skill() {
        webagent_abilities::catalog_testing::tests::catalog_version_changes_with_skill::<
            crate::abilities::NativeHost,
        >();
    }
    #[test]
    fn visible_skills_do_not_depend_on_tool_schema_exposure() {
        webagent_abilities::catalog_testing::tests::visible_skills_do_not_depend_on_tool_schema_exposure::<crate::abilities::NativeHost>();
    }
    #[test]
    fn visible_guidance_and_schema_respect_suppression() {
        webagent_abilities::catalog_testing::tests::visible_guidance_and_schema_respect_suppression::<
            crate::abilities::NativeHost,
        >();
    }
    #[test]
    fn visible_guidance_filters_denied_ability_and_tool_text() {
        webagent_abilities::catalog_testing::tests::visible_guidance_filters_denied_ability_and_tool_text::<crate::abilities::NativeHost>();
    }
    #[test]
    fn persisted_active_discoverable_ability_exposes_schemas() {
        webagent_abilities::catalog_testing::tests::persisted_active_discoverable_ability_exposes_schemas::<crate::abilities::NativeHost>();
    }
    #[test]
    fn agent_off_revokes_loaded_ability_but_keeps_configuration_row() {
        webagent_abilities::catalog_testing::tests::agent_off_revokes_loaded_ability_but_keeps_configuration_row::<crate::abilities::NativeHost>();
    }
    #[test]
    fn bundled_skill_mode_is_independent_of_ability_mode() {
        webagent_abilities::catalog_testing::tests::bundled_skill_mode_is_independent_of_ability_mode::<crate::abilities::NativeHost>();
    }
    #[test]
    fn ability_activation_preserves_independent_skill_visibility() {
        webagent_abilities::catalog_testing::tests::ability_activation_preserves_independent_skill_visibility::<crate::abilities::NativeHost>();
    }
    #[test]
    fn typed_pending_entries_never_expose_tools_or_guidance() {
        webagent_abilities::catalog_testing::tests::typed_pending_entries_never_expose_tools_or_guidance::<crate::abilities::NativeHost>();
    }
    #[test]
    fn installed_tool_ownership_and_identifiers_fail_closed() {
        webagent_abilities::catalog_testing::tests::installed_tool_ownership_and_identifiers_fail_closed::<crate::abilities::NativeHost>();
    }
    #[test]
    fn unsupported_allow_rule_does_not_expose_catalog_or_skill() {
        webagent_abilities::catalog_testing::tests::unsupported_allow_rule_does_not_expose_catalog_or_skill::<crate::abilities::NativeHost>();
    }
    #[test]
    fn malformed_and_oversized_assets_fail_closed() {
        webagent_abilities::catalog_testing::tests::malformed_and_oversized_assets_fail_closed::<
            crate::abilities::NativeHost,
        >();
    }
    #[test]
    fn symlink_assets_fail_closed() {
        webagent_abilities::catalog_testing::tests::symlink_assets_fail_closed::<
            crate::abilities::NativeHost,
        >();
    }
    #[test]
    fn current_catalog_withdrawal_preserves_strict_snapshot_validation() {
        webagent_abilities::catalog_testing::tests::current_catalog_withdrawal_preserves_strict_snapshot_validation::<crate::abilities::NativeHost>();
    }
    #[test]
    fn selected_tool_markers_preserve_off_and_suppressed_controls() {
        webagent_abilities::catalog_testing::tests::selected_tool_markers_preserve_off_and_suppressed_controls::<crate::abilities::NativeHost>();
    }
    #[test]
    fn code_index_comment_maintenance_skill_is_discoverable() {
        webagent_abilities::catalog_testing::tests::code_index_comment_maintenance_skill_is_discoverable::<crate::abilities::NativeHost>();
    }
    #[test]
    fn production_seed_manifest_loads() {
        webagent_abilities::catalog_testing::tests::production_seed_manifest_loads::<
            crate::abilities::NativeHost,
        >();
    }
    #[test]
    fn web_access_loading_policy_and_visibility_gates() {
        webagent_abilities::catalog_testing::tests::web_access_loading_policy_and_visibility_gates::<
            crate::abilities::NativeHost,
        >();
    }
}
#[cfg(test)]
mod optional_module_tests {
    #[test]
    fn absent_optional_module_loads_without_exposing_execution() {
        webagent_abilities::catalog_testing::optional_module_tests::absent_optional_module_loads_without_exposing_execution::<crate::abilities::NativeHost>();
    }
}
#[cfg(test)]
mod optional_settings_schema_tests {
    #[test]
    fn optional_schema_enforces_full_declared_types_keys_and_numeric_bounds() {
        webagent_abilities::catalog_testing::optional_settings_schema_tests::optional_schema_enforces_full_declared_types_keys_and_numeric_bounds::<crate::abilities::NativeHost>();
    }
    #[test]
    fn malformed_optional_declarations_fail_closed() {
        webagent_abilities::catalog_testing::optional_settings_schema_tests::malformed_optional_declarations_fail_closed::<crate::abilities::NativeHost>();
    }
    #[test]
    fn capability_guidance_reads_selected_live_sources_and_withdraws_invalid_or_denied_assets() {
        webagent_abilities::catalog_testing::optional_settings_schema_tests::capability_guidance_reads_selected_live_sources_and_withdraws_invalid_or_denied_assets::<crate::abilities::NativeHost>();
    }
    #[test]
    fn capability_guidance_filters_current_authorized_tool_names_and_modes() {
        webagent_abilities::catalog_testing::optional_settings_schema_tests::capability_guidance_filters_current_authorized_tool_names_and_modes::<crate::abilities::NativeHost>();
    }
    #[test]
    fn selected_state_is_call_local_and_preserves_off_and_suppression() {
        webagent_abilities::catalog_testing::optional_settings_schema_tests::selected_state_is_call_local_and_preserves_off_and_suppression::<crate::abilities::NativeHost>();
    }
}

#[cfg(test)]
impl webagent_abilities::TestHost for NativeHost {
    fn native_fixture() -> Catalog {
        crate::runtime_tests::native_catalog()
    }
    fn policy_fixture() -> Value {
        crate::policy_default()
    }
}
