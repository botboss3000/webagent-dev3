//! Ladder behavior and malformed-order regression tests.
use crate::{Permission,capabilities::{CapabilityLevels,Exposure,membership_level},Relationship};
use serde_json::json;

#[test]
fn intermediate_ceiling_limits_every_lower_level_without_rewriting_configuration() {
    let mut levels=CapabilityLevels::default();
    for level in levels.levels.values_mut() {level.ability_permissions.insert("web_access".into(),Permission::Auto);}
    levels.levels.get_mut("member").unwrap().ability_permissions.insert("web_access".into(),Permission::Ask);
    levels.levels.get_mut("member").unwrap().tool_exposure.insert("web_search".into(),Exposure::Off);
    assert_eq!(levels.effective("visitor").unwrap().ability_permission("web_access"),Permission::Ask);
    assert_eq!(levels.effective("visitor").unwrap().tool_exposure["web_search"],Exposure::Off);
    assert_eq!(levels.levels["visitor"].ability_permission("web_access"),Permission::Auto);
    assert_eq!(levels.effective("operator").unwrap().ability_permission("web_access"),Permission::Auto);
}

#[test]
fn wildcard_and_exact_entries_intersect_across_the_ladder() {
    let mut levels=CapabilityLevels::default();
    levels.levels.get_mut("operator").unwrap().tool_permissions.insert("*".into(),Permission::Ask);
    levels.levels.get_mut("member").unwrap().tool_permissions.insert("file_write".into(),Permission::Deny);
    let result=levels.effective("visitor").unwrap();
    assert_eq!(result.tool_permission("file_read"),Permission::Ask);
    assert_eq!(result.tool_permission("file_write"),Permission::Deny);
}

#[test]
fn malformed_order_and_level_names_cannot_grant_operator_authority() {
    let mut levels=CapabilityLevels::default();
    levels.order.swap(0,1);assert!(levels.effective("visitor").is_err());
    levels=CapabilityLevels::default();levels.order[1]="operator".into();assert!(levels.validate().is_err());
    assert!(membership_level(Some(&json!({"capability_level_id":"operator"})),Relationship::Member).is_err());
    assert_eq!(membership_level(Some(&json!({"agent_administrator":true})),Relationship::Member).unwrap(),"operator");
    for reserved in ["administrator","base"] {
        let mut levels=CapabilityLevels::default();let mut level=levels.levels["member"].clone();level.id=reserved.into();
        levels.levels.insert(reserved.into(),level);levels.order.insert(1,reserved.into());assert!(levels.validate().is_err());
    }
}

#[test]
fn legacy_levels_receive_a_complete_bounded_order() {
    let mut old=serde_json::to_value(CapabilityLevels::default()).unwrap();
    old.as_object_mut().unwrap().remove("order");
    let mut admin=old["levels"].as_object_mut().unwrap().remove("operator").unwrap();
    admin["id"]=json!("administrator");old["levels"]["administrator"]=admin;
    let levels=CapabilityLevels::from_metadata(&json!({"capability_levels":old})).unwrap();
    assert_eq!(levels.order,vec!["operator","member","visitor"]);
}

#[test]
fn raising_an_ancestor_keeps_explicit_lower_restrictions() {
    let mut levels=CapabilityLevels::default();
    levels.levels.get_mut("operator").unwrap().tool_permissions.insert("*".into(),Permission::Deny);
    levels.levels.get_mut("member").unwrap().tool_permissions.insert("file_write".into(),Permission::Ask);
    assert_eq!(levels.effective("visitor").unwrap().tool_permission("file_write"),Permission::Deny);
    levels.levels.get_mut("operator").unwrap().tool_permissions.insert("*".into(),Permission::Auto);
    assert_eq!(levels.effective("visitor").unwrap().tool_permission("file_write"),Permission::Ask);
}

#[test]
fn unsaved_levels_grant_no_abilities_and_never_fall_back_to_codebase_admin() {
    // No saved levels, and a legacy base map that once granted codebase_admin.
    for metadata in [json!({}),json!({"ability_permissions":{"codebase_admin":"auto","*":"auto"}})] {
        let levels=CapabilityLevels::from_metadata(&metadata).unwrap();
        assert!(levels.unset());
        assert_eq!(levels.tools_notice(),Some(crate::capabilities::NO_TOOLS_NOTICE));
        for id in ["operator","member","visitor"] {
            let level=levels.effective(id).unwrap();
            for ability in ["codebase_admin","web_access","new_plugin"] {
                assert_eq!(level.ability_permission(ability),Permission::Deny,"{id} {ability}");
            }
        }
    }
    let mut saved=CapabilityLevels::default();saved.revision=1;
    assert!(!saved.unset());assert_eq!(saved.tools_notice(),None);
}

#[test]
fn defined_tools_fall_back_to_role_defaults_then_deny() {
    // Only `fixture_` tools have definitions here, so other tests keep the legacy Auto.
    fn defaults(id:&str,rungs:&[String])->Option<Permission> {
        let access=match id {
            "fixture_write"=>[("operator",Permission::Auto),("member",Permission::Ask)].as_slice(),
            "fixture_read"=>[("visitor",Permission::Auto)].as_slice(),
            _=>return None,
        };
        Some(rungs.iter().find_map(|rung|access.iter().find(|(role,_)|role==rung).map(|(_,p)|*p)).unwrap_or(Permission::Deny))
    }
    crate::capabilities::install_tool_defaults(defaults);
    let mut levels=CapabilityLevels::default();
    let mut trusted=levels.levels["member"].clone();trusted.id="trusted".into();trusted.name="Trusted".into();
    levels.levels.insert("trusted".into(),trusted);levels.order.insert(1,"trusted".into());
    let level=|levels:&CapabilityLevels,id:&str|levels.effective(id).unwrap();
    assert_eq!(level(&levels,"operator").tool_permission("fixture_write"),Permission::Auto);
    // A custom level without its own default takes the next rung down.
    assert_eq!(level(&levels,"trusted").tool_permission("fixture_write"),Permission::Ask);
    assert_eq!(level(&levels,"visitor").tool_permission("fixture_write"),Permission::Deny);
    // Every level meets the levels above it, each at its own default.
    assert_eq!(level(&levels,"member").tool_permission("fixture_read"),Permission::Auto);
    levels.levels.get_mut("operator").unwrap().tool_permissions.insert("fixture_read".into(),Permission::Ask);
    assert_eq!(level(&levels,"visitor").tool_permission("fixture_read"),Permission::Ask);
    // A saved permission replaces the default; undefined tools keep Auto.
    levels.levels.get_mut("visitor").unwrap().tool_permissions.insert("fixture_write".into(),Permission::Auto);
    assert_eq!(level(&levels,"visitor").tool_permission("fixture_write"),Permission::Ask);
    assert_eq!(level(&levels,"visitor").tool_permission("unrelated"),Permission::Auto);
}
