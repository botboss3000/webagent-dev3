//! App account ceilings are a separate ladder from agent membership.
use crate::{Tier, TierGrants};
use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use std::collections::BTreeSet;
pub fn order(policy: &Value) -> Result<Vec<String>> {
    let tiers = policy["tiers"]
        .as_object()
        .ok_or_else(|| anyhow!("Account tiers missing"))?;
    let order: Vec<String> = if let Some(order) = policy.get("tier_order") {
        serde_json::from_value(order.clone())?
    } else {
        let mut ids = Vec::new();
        if tiers.contains_key(crate::ADMINISTRATOR_TIER) {
            ids.push(crate::ADMINISTRATOR_TIER.into());
        }
        ids.extend(
            tiers
                .keys()
                .filter(|id| {
                    ![
                        crate::ADMINISTRATOR_TIER,
                        crate::FREE_TIER,
                        crate::ANONYMOUS_TIER,
                    ]
                    .contains(&id.as_str())
                })
                .cloned(),
        );
        for id in [crate::FREE_TIER, crate::ANONYMOUS_TIER] {
            if tiers.contains_key(id) {
                ids.push(id.into());
            }
        }
        ids
    };
    if order.len() != tiers.len()
        || order.iter().collect::<BTreeSet<_>>().len() != order.len()
        || order.iter().any(|id| !tiers.contains_key(id))
        || (tiers.contains_key(crate::ADMINISTRATOR_TIER)
            && order.first().map(String::as_str) != Some(crate::ADMINISTRATOR_TIER))
        || (tiers.contains_key(crate::ANONYMOUS_TIER)
            && order.last().map(String::as_str) != Some(crate::ANONYMOUS_TIER))
    {
        bail!("Invalid app account ladder");
    }
    Ok(order)
}
pub fn effective(policy: &Value, id: &str) -> Result<Tier> {
    let ids = order(policy)?;
    let index = ids
        .iter()
        .position(|v| v == id)
        .ok_or_else(|| anyhow!("Account tier not found"))?;
    let mut result: Tier = serde_json::from_value(policy["tiers"][id].clone())?;
    result.validate()?;
    // Grants resolve per tier (named, wildcard, then entitlement group) before they meet,
    // so a group grant applies at the tier that makes it.
    let mut chain = vec![TierGrants::of(&result)];
    for upper in &ids[..index] {
        let tier: Tier = serde_json::from_value(policy["tiers"][upper].clone())?;
        tier.validate()?;
        chain.push(TierGrants::of(&tier));
        if !tier.allowed_models.iter().any(|m| m == "*") {
            if result.allowed_models.iter().any(|m| m == "*") {
                result.allowed_models = tier.allowed_models.clone();
            } else {
                result
                    .allowed_models
                    .retain(|m| tier.allowed_models.contains(m));
            }
        }
        result.max_sessions = result.max_sessions.min(tier.max_sessions);
        result.max_active_runs = result.max_active_runs.min(tier.max_active_runs);
        if tier.model_policy.is_none() {
            if let Some(model) = &mut result.model_policy {
                model.roster_id = None;
                model.allowed_entry_ids.clear();
            }
        }
        if let Some(ceiling) = &tier.model_policy {
            if let Some(model) = &mut result.model_policy {
                model.allow_byo &= ceiling.allow_byo;
                model.max_byo_entries = model.max_byo_entries.min(ceiling.max_byo_entries);
                if model.roster_id != ceiling.roster_id {
                    model.allowed_entry_ids.clear();
                } else {
                    model
                        .allowed_entry_ids
                        .retain(|entry| ceiling.allowed_entry_ids.contains(entry));
                }
            } else {
                result.model_policy = Some(crate::ModelPolicy {
                    roster_id: None,
                    allowed_entry_ids: Default::default(),
                    allow_byo: ceiling.allow_byo,
                    max_byo_entries: ceiling.max_byo_entries,
                });
            }
        }
    }
    // Named grants resolve in each tier (named, wildcard, then group) before they meet.
    if chain.len() > 1 {
        result.ability_permissions = TierGrants::merge(&chain, false);
        result.tools = TierGrants::merge(&chain, true);
    }
    result.chain = chain;
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Permission;
    use serde_json::json;
    #[test]
    fn platform_grants_require_both_child_and_ancestor() {
        let parent = json!({"id":"administrator","revision":1,"allowed_models":["*"],"tools":{},"max_sessions":8,"max_active_runs":2,
            "model_policy":{"roster_id":"platform","allowed_entry_ids":["entry"],"allow_byo":false,"max_byo_entries":0}});
        let mut child = parent.clone();
        child["id"] = json!("free");
        child["model_policy"] = Value::Null;
        let mut policy = json!({"tiers":{"administrator":parent,"free":child},"tier_order":["administrator","free"]});
        let resolved = effective(&policy, "free").unwrap();
        let model = resolved.model_policy.unwrap();
        assert!(model.roster_id.is_none());
        assert!(model.allowed_entry_ids.is_empty());
        assert!(!model.allow_byo);
        policy["tiers"]["free"]["model_policy"] =
            policy["tiers"]["administrator"]["model_policy"].clone();
        policy["tiers"]["administrator"]["model_policy"] = Value::Null;
        let resolved = effective(&policy, "free").unwrap();
        assert!(resolved.model_policy.unwrap().allowed_entry_ids.is_empty());
    }
    #[test]
    fn upper_account_ceiling_limits_permissions_models_and_quotas() {
        let tier = json!({"id":"administrator","revision":1,"allowed_models":["model_a"],"tools":{"*":"ask"},"ability_permissions":{"*":"auto"},"max_sessions":8,"max_active_runs":2});
        let mut base = tier.clone();
        base["id"] = json!("free");
        base["tools"] = json!({"*":"auto"});
        base["allowed_models"] = json!(["*"]);
        base["max_sessions"] = json!(100);
        let policy = json!({"tiers":{"administrator":tier,"free":base},"tier_order":["administrator","free"]});
        let resolved = effective(&policy, "free").unwrap();
        assert_eq!(resolved.tool_permission("file_write"), Permission::Ask);
        assert_eq!(resolved.max_sessions, 8);
        assert_eq!(resolved.allowed_models, vec!["model_a"]);
        let mut invalid = policy;
        invalid["tier_order"] = json!(["free", "administrator"]);
        assert!(effective(&invalid, "free").is_err());
    }
    #[test]
    fn plans_grant_entitlement_groups_and_named_grants_win() {
        crate::install_entitlements(
            |id| match id {
                "web_search" | "web_access" => Some("web_read"),
                _ => None,
            },
            |name| match name {
                "web_read" | "get_weather" => Some("web_read"),
                _ => None,
            },
        );
        let tier = |id: &str, extra: Value| {
            let mut tier = json!({"id":id,"revision":1,"allowed_models":["*"],"tools":{},
            "max_sessions":8,"max_active_runs":2});
            for (key, value) in extra.as_object().unwrap() {
                tier[key] = value.clone();
            }
            tier
        };
        let policy = json!({"tier_order":["administrator","free"],"tiers":{
            "administrator":tier("administrator",json!({"entitlements":{"web_read":"auto"}})),
            "free":tier("free",json!({"entitlements":{"web_read":"auto"},"ability_permissions":{"web_access":"ask"}}))}});
        let basic = effective(&policy, "free").unwrap();
        assert_eq!(basic.ability_permission("web_search"), Permission::Auto);
        assert_eq!(
            basic.ability_permission("web_access"),
            Permission::Ask,
            "a named grant wins over the group"
        );
        assert_eq!(
            basic.ability_permission("codebase_admin"),
            Permission::Deny,
            "ungrouped stays closed"
        );
        assert_eq!(basic.tool_permission("web_read"), Permission::Auto);
        // An upper tier without the group still caps a lower tier that grants it.
        let mut capped = policy.clone();
        capped["tiers"]["administrator"]["entitlements"] = json!({});
        assert_eq!(
            effective(&capped, "free")
                .unwrap()
                .ability_permission("web_search"),
            Permission::Deny
        );
        assert!(serde_json::from_value::<Tier>(tier(
            "free",
            json!({"entitlements":{"Web Read":"auto"}})
        ))
        .unwrap()
        .validate()
        .is_err());
    }
    #[test]
    fn manifest_templates_fill_only_unnamed_grants_per_tier() {
        crate::install_templates(
            |id, tier| (id == "wiki" && tier != "ghost").then_some(Permission::Auto),
            |name, tier| match (name, tier) {
                ("wiki_get", _) => Some(Permission::Auto),
                ("wiki_create", "administrator") => Some(Permission::Auto),
                _ => None,
            },
        );
        let tier = |id: &str, tools: Value, abilities: Value| {
            json!({"id":id,"revision":1,"allowed_models":["*"],"tools":tools,
                "ability_permissions":abilities,"max_sessions":8,"max_active_runs":2})
        };
        let mut policy = json!({"tier_order":["administrator","free"],"tiers":{
            "administrator":tier("administrator",json!({"*":"auto"}),json!({})),
            "free":tier("free",json!({}),json!({}))}});
        let basic = effective(&policy, "free").unwrap();
        assert_eq!(basic.ability_permission("wiki"), Permission::Auto);
        assert_eq!(basic.tool_permission("wiki_get"), Permission::Auto);
        assert_eq!(basic.tool_permission("wiki_create"), Permission::Deny);
        let operator = effective(&policy, "administrator").unwrap();
        assert_eq!(operator.tool_permission("wiki_create"), Permission::Auto);
        // A tier's own named grant or wildcard overrides the template.
        policy["tiers"]["free"]["ability_permissions"] = json!({"wiki":"deny"});
        policy["tiers"]["free"]["tools"] = json!({"*":"ask"});
        let basic = effective(&policy, "free").unwrap();
        assert_eq!(basic.ability_permission("wiki"), Permission::Deny);
        assert_eq!(basic.tool_permission("wiki_get"), Permission::Ask);
    }
}
