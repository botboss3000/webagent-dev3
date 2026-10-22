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
