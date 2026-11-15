//! Ladder behavior and malformed-order regression tests.
use crate::{
    capabilities::{membership_level, CapabilityLevels, Exposure},
    Permission, Relationship,
};
use serde_json::json;

#[test]
fn intermediate_ceiling_limits_every_lower_level_without_rewriting_configuration() {
    let mut levels = CapabilityLevels::default();
    for level in levels.levels.values_mut() {
        level
            .ability_permissions
            .insert("web_access".into(), Permission::Auto);
    }
    levels
        .levels
        .get_mut("member")
        .unwrap()
        .ability_permissions
        .insert("web_access".into(), Permission::Ask);
    levels
        .levels
        .get_mut("member")
        .unwrap()
        .tool_exposure
        .insert("web_search".into(), Exposure::Off);
    assert_eq!(
        levels
            .effective("visitor")
            .unwrap()
            .ability_permission("web_access"),
        Permission::Ask
    );
    assert_eq!(
        levels.effective("visitor").unwrap().tool_exposure["web_search"],
        Exposure::Off
    );
    assert_eq!(
        levels.levels["visitor"].ability_permission("web_access"),
        Permission::Auto
    );
    assert_eq!(
        levels
            .effective("operator")
            .unwrap()
            .ability_permission("web_access"),
        Permission::Auto
    );
}

#[test]
fn wildcard_is_ignored_and_exact_entries_intersect_across_the_ladder() {
    let mut levels = CapabilityLevels::default();
    levels
        .levels
        .get_mut("operator")
        .unwrap()
        .tool_permissions
        .insert("*".into(), Permission::Ask);
    levels
        .levels
        .get_mut("member")
        .unwrap()
        .tool_permissions
        .insert("file_write".into(), Permission::Deny);
    let result = levels.effective("visitor").unwrap();
    // The retired wildcard no longer overrides tool defaults.
    assert_eq!(result.tool_permission("file_read"), Permission::Auto);
    assert_eq!(result.tool_permission("file_write"), Permission::Deny);
}

#[test]
fn malformed_order_and_level_names_cannot_grant_operator_authority() {
    let mut levels = CapabilityLevels::default();
    levels.order.swap(0, 1);
    assert!(levels.effective("visitor").is_err());
    levels = CapabilityLevels::default();
    levels.order[1] = "operator".into();
    assert!(levels.validate().is_err());
    assert!(membership_level(
        Some(&json!({"capability_level_id":"operator"})),
        Relationship::Member
    )
    .is_err());
    assert_eq!(
        membership_level(
            Some(&json!({"agent_administrator":true})),
            Relationship::Member
        )
        .unwrap(),
        "operator"
    );
    for reserved in ["administrator", "base"] {
        let mut levels = CapabilityLevels::default();
        let mut level = levels.levels["member"].clone();
        level.id = reserved.into();
        levels.levels.insert(reserved.into(), level);
        levels.order.insert(1, reserved.into());
        assert!(levels.validate().is_err());
    }
}

#[test]
fn legacy_levels_receive_a_complete_bounded_order() {
    let mut old = serde_json::to_value(CapabilityLevels::default()).unwrap();
    old.as_object_mut().unwrap().remove("order");
    let mut admin = old["levels"]
        .as_object_mut()
        .unwrap()
        .remove("operator")
        .unwrap();
    admin["id"] = json!("administrator");
    old["levels"]["administrator"] = admin;
    let levels = CapabilityLevels::from_metadata(&json!({"capability_levels":old})).unwrap();
    assert_eq!(levels.order, vec!["operator", "member", "visitor"]);
}

#[test]
fn raising_an_ancestor_keeps_explicit_lower_restrictions() {
    let mut levels = CapabilityLevels::default();
    levels
        .levels
        .get_mut("operator")
        .unwrap()
        .tool_permissions
        .insert("file_write".into(), Permission::Deny);
    levels
        .levels
        .get_mut("member")
        .unwrap()
        .tool_permissions
        .insert("file_write".into(), Permission::Ask);
    assert_eq!(
        levels
            .effective("visitor")
            .unwrap()
            .tool_permission("file_write"),
        Permission::Deny
    );
    levels
        .levels
        .get_mut("operator")
        .unwrap()
        .tool_permissions
        .insert("file_write".into(), Permission::Auto);
    assert_eq!(
        levels
            .effective("visitor")
            .unwrap()
            .tool_permission("file_write"),
        Permission::Ask
    );
}

#[test]
fn abilities_follow_tool_json_and_legacy_grants_are_dropped() {
    // Unsaved levels, a legacy base map, and saved grants with a tool wildcard.
    let saved = json!({"capability_levels":{"revision":2,"order":["operator","member","visitor"],"levels":{
        "operator":{"id":"operator","name":"Operator","revision":1,"ability_permissions":{"web_access":"auto","terminal_control":"deny"},"tool_permissions":{"*":"auto","shell":"deny"}},
        "member":{"id":"member","name":"Member","revision":1,"ability_permissions":{"web_access":"auto"},"tool_permissions":{}},
        "visitor":{"id":"visitor","name":"Visitor","revision":1,"ability_permissions":{},"tool_permissions":{}}}}});
    for metadata in [
        json!({}),
        json!({"ability_permissions":{"codebase_admin":"auto","*":"auto"}}),
        saved,
    ] {
        let levels = CapabilityLevels::from_metadata(&metadata).unwrap();
        for id in ["operator", "member", "visitor"] {
            assert!(levels.levels[id].ability_permissions.is_empty(), "{id}");
            assert!(
                !levels.levels[id].tool_permissions.contains_key("*"),
                "{id}"
            );
            let level = levels.effective(id).unwrap();
            for ability in [
                "codebase_admin",
                "web_access",
                "new_plugin",
                "terminal_control",
            ] {
                assert_eq!(
                    level.ability_permission(ability),
                    Permission::Auto,
                    "{id} {ability}"
                );
            }
        }
    }
    // Specific tool overrides stay.
    let levels = CapabilityLevels::from_metadata(&json!({"capability_levels":{"revision":2,"order":["operator","member","visitor"],"levels":{
        "operator":{"id":"operator","name":"Operator","revision":1,"ability_permissions":{},"tool_permissions":{"*":"auto","shell":"deny"}},
        "member":{"id":"member","name":"Member","revision":1,"ability_permissions":{},"tool_permissions":{}},
        "visitor":{"id":"visitor","name":"Visitor","revision":1,"ability_permissions":{},"tool_permissions":{}}}}})).unwrap();
    assert_eq!(
        levels
            .effective("operator")
            .unwrap()
            .tool_permission("shell"),
        Permission::Deny
    );
}

#[test]
fn defined_tools_fall_back_to_role_defaults_then_deny() {
    // Only `fixture_` tools have definitions here, so other tests keep the legacy Auto.
    fn defaults(id: &str, rungs: &[String]) -> Option<Permission> {
        let access = match id {
            "fixture_write" => {
                [("operator", Permission::Auto), ("member", Permission::Ask)].as_slice()
            }
            "fixture_read" => [("visitor", Permission::Auto)].as_slice(),
            _ => return None,
        };
        Some(
            rungs
                .iter()
                .find_map(|rung| {
                    access
                        .iter()
                        .find(|(role, _)| role == rung)
                        .map(|(_, p)| *p)
                })
                .unwrap_or(Permission::Deny),
        )
    }
    crate::capabilities::install_tool_defaults(defaults);
    let mut levels = CapabilityLevels::default();
    let mut trusted = levels.levels["member"].clone();
    trusted.id = "trusted".into();
    trusted.name = "Trusted".into();
    levels.levels.insert("trusted".into(), trusted);
    levels.order.insert(1, "trusted".into());
    let level = |levels: &CapabilityLevels, id: &str| levels.effective(id).unwrap();
    assert_eq!(
        level(&levels, "operator").tool_permission("fixture_write"),
        Permission::Auto
    );
    // A custom level without its own default takes the next rung down.
    assert_eq!(
        level(&levels, "trusted").tool_permission("fixture_write"),
        Permission::Ask
    );
    assert_eq!(
        level(&levels, "visitor").tool_permission("fixture_write"),
        Permission::Deny
    );
    // Every level meets the levels above it, each at its own default.
    assert_eq!(
        level(&levels, "member").tool_permission("fixture_read"),
        Permission::Auto
    );
    levels
        .levels
        .get_mut("operator")
        .unwrap()
        .tool_permissions
        .insert("fixture_read".into(), Permission::Ask);
    assert_eq!(
        level(&levels, "visitor").tool_permission("fixture_read"),
        Permission::Ask
    );
    // A saved permission replaces the default; undefined tools keep Auto.
    levels
        .levels
        .get_mut("visitor")
        .unwrap()
        .tool_permissions
        .insert("fixture_write".into(), Permission::Auto);
    assert_eq!(
        level(&levels, "visitor").tool_permission("fixture_write"),
        Permission::Ask
    );
    assert_eq!(
        level(&levels, "visitor").tool_permission("unrelated"),
        Permission::Auto
    );
}

#[test]
fn saves_refuse_only_new_out_of_order_cases() {
    let mut levels = CapabilityLevels::default();
    for level in levels.levels.values_mut() {
        level
            .ability_permissions
            .insert("web_access".into(), Permission::Ask);
    }
    let ordered = levels.clone();
    levels.check_order(&ordered).unwrap();
    let raise = |levels: &mut CapabilityLevels, id: &str, key: &str, to: Permission| {
        levels
            .levels
            .get_mut(id)
            .unwrap()
            .ability_permissions
            .insert(key.into(), to);
    };
    raise(&mut levels, "visitor", "web_access", Permission::Auto);
    let error = levels.check_order(&ordered).unwrap_err().to_string();
    assert!(
        error.contains("Visitor") && error.contains("Member"),
        "{error}"
    );
    // Reads still clamp the out-of-order level instead of failing.
    assert_eq!(
        levels
            .effective("visitor")
            .unwrap()
            .ability_permission("web_access"),
        Permission::Ask
    );
    // An out-of-order ladder saved earlier: Operator denies what both lower levels allow.
    let mut legacy = ordered.clone();
    raise(&mut legacy, "operator", "web_access", Permission::Deny);
    // Unrelated saves and step-by-step fixes from the bottom up go through.
    let mut next = legacy.clone();
    raise(&mut next, "operator", "notes", Permission::Auto);
    next.check_order(&legacy).unwrap();
    let mut fixed = legacy.clone();
    raise(&mut fixed, "visitor", "web_access", Permission::Deny);
    fixed.check_order(&legacy).unwrap();
    // A new case is still refused.
    let mut worse = legacy.clone();
    raise(&mut worse, "operator", "browser", Permission::Deny);
    assert!(worse.check_order(&legacy).is_err());
    levels
        .levels
        .get_mut("visitor")
        .unwrap()
        .tool_exposure
        .insert("web_search".into(), Exposure::Off);
    raise(&mut levels, "visitor", "web_access", Permission::Deny);
    levels.check_order(&ordered).unwrap();
    levels
        .levels
        .get_mut("member")
        .unwrap()
        .skill_exposure
        .insert("notes".into(), Exposure::Off);
    assert!(levels.check_order(&ordered).is_err());
}
