//! Abilities infrastructure composition; native/domain implementations remain with their owners.
use anyhow::Result;
use serde_json::Value;
use std::path::PathBuf;
#[derive(Clone)]
pub(crate) struct NativeHost;
pub(crate) type Catalog = webagent_abilities::Catalog<NativeHost>;
pub(crate) use webagent_abilities::{
    near_word, public_context, search_words, AbilityContext, AbilityInclusion, SkillInclusion,
};
/// The folder holding the shipped abilities of a desktop install (`V3_ABILITIES_ROOT` is
/// its `abilities/`), only in the desktop process role.
fn desktop_base() -> Option<PathBuf> {
    if std::env::var_os("V3_PROCESS_ROLE").is_none_or(|role| role != "desktop") {
        return None;
    }
    let abilities = PathBuf::from(std::env::var_os("V3_ABILITIES_ROOT")?);
    let base = abilities.parent()?.to_path_buf();
    (base.join("abilities") == abilities).then_some(base)
}

/// Where live ability edits apply: the published release (or a desktop install's shipped
/// abilities) as the base, and the checkout's `abilities/`. `Err(base)` when there is no
/// checkout to lay over it; `Ok(None)`/`Err(None)` mean no base either.
fn live_parts() -> Result<std::result::Result<(PathBuf, PathBuf), Option<PathBuf>>> {
    let checkout = webagent_configuration::paths::checkout().join("abilities");
    Ok(
        match webagent_configuration::prompts::default_source(None)? {
            Some(base) if !checkout.is_dir() => Err(Some(base)),
            Some(base) => Ok((base, checkout)),
            None => match desktop_base() {
                Some(base) if checkout.is_dir() => Ok((base, checkout)),
                _ => Err(None),
            },
        },
    )
}

fn live_cache() -> PathBuf {
    webagent_configuration::paths::runtime().join("ability-live")
}

/// Why live ability edits at `abilities` may not be used over `base`: the release check
/// (`crate::ability_problems`, in `src/app.rs`) against the release whose code serves the tools.
fn live_problems(abilities: &std::path::Path, base: &std::path::Path) -> Vec<String> {
    crate::ability_problems(abilities, Some(base))
}

/// Problems App Default edits (checkout-relative `abilities/...` paths; `None` deletes) would
/// cause once written to the live checkout; empty when they may be saved. Checked on what
/// the live abilities would become, with the same rules a release and the overlay apply.
pub(crate) fn live_edit_problems(edits: &[(String, Option<String>)]) -> Result<Vec<String>> {
    let Ok((base, checkout)) = live_parts()? else {
        anyhow::bail!("This app has no live ability files to save into");
    };
    let edits = edits
        .iter()
        .map(|(path, body)| {
            let relative = path
                .strip_prefix("abilities/")
                .ok_or_else(|| anyhow::anyhow!("{path} is not an ability file"))?;
            Ok((
                PathBuf::from(relative),
                body.as_ref().map(|text| text.as_bytes().to_vec()),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let root = live_cache().join(format!(".check-{}", uuid::Uuid::new_v4()));
    let built = webagent_abilities::live::candidate(&base, &checkout, &edits, &root);
    let problems = built.map(|()| live_problems(&root.join("abilities"), &base));
    let _ = std::fs::remove_dir_all(&root);
    Ok(problems?)
}

impl webagent_abilities::Host for NativeHost {
    /// The published release, with the checkout's ability JSON and Markdown laid over it
    /// when they differ (ability files are live; see `webagent_abilities::live`), as long
    /// as they pass the release check; otherwise the last good version stays.
    /// An installed desktop app has no publication: its shipped `app/abilities` is the base.
    fn selected_source() -> Result<Option<PathBuf>> {
        let (base, checkout) = match live_parts()? {
            Ok(parts) => parts,
            Err(base) => return Ok(base),
        };
        Ok(Some(webagent_abilities::live::overlay(
            &base,
            &checkout,
            &live_cache(),
            |root| {
                let problems = live_problems(root, &base);
                for problem in problems.iter().take(5) {
                    eprintln!("Live ability edit refused: {problem}");
                }
                problems.is_empty()
            },
        )))
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
    /// Test builds keep the group: the shared catalog tests use Administrator fixtures, and
    /// the native fixture catalog leaves the real pack out when the tier is off.
    fn administrator_tier() -> bool {
        cfg!(any(feature = "administrator", test))
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
    fn mcp_available() -> bool {
        crate::mcp::bridge::available()
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
    fn every_enabled_tool_is_sent_and_off_stays_hidden() {
        webagent_abilities::catalog_testing::tests::every_enabled_tool_is_sent_and_off_stays_hidden::<
            crate::abilities::NativeHost,
        >();
    }
    #[test]
    fn diagnostic_projection_rejects_content_and_unknown_identifiers() {
        webagent_abilities::catalog_testing::tests::diagnostic_projection_rejects_content_and_unknown_identifiers::<crate::abilities::NativeHost>();
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
    fn group_tools_are_sent_without_loading() {
        webagent_abilities::catalog_testing::tests::group_tools_are_sent_without_loading::<
            crate::abilities::NativeHost,
        >();
    }
    #[test]
    fn base_stays_on_and_skills_default_discoverable() {
        webagent_abilities::catalog_testing::tests::base_stays_on_and_skills_default_discoverable::<
            crate::abilities::NativeHost,
        >();
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
    fn unsupported_tool_does_not_expose_catalog_or_skill() {
        webagent_abilities::catalog_testing::tests::unsupported_tool_does_not_expose_catalog_or_skill::<crate::abilities::NativeHost>();
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
    #[cfg_attr(not(administrator_pack), ignore = "needs the Administrator pack")]
    fn production_seed_manifest_loads() {
        webagent_abilities::catalog_testing::tests::production_seed_manifest_loads::<
            crate::abilities::NativeHost,
        >();
    }
    #[test]
    fn web_access_loading_and_visibility_gates() {
        webagent_abilities::catalog_testing::tests::web_access_loading_and_visibility_gates::<
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
}
