//! App account ceilings are a separate ladder from agent membership.
use crate::Tier;
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
/// The tiers whose ability and tool grants `tier` gets: itself, then every tier ranked below
/// it in `tier_order`. An unknown or missing tier gets only the most restricted (lowest) tier's
/// grants; an invalid ladder gets none.
#[must_use]
pub fn inherited(policy: &Value, tier: &str) -> Vec<String> {
    let Ok(mut ids) = order(policy) else {
        return Vec::new();
    };
    match ids.iter().position(|id| id == tier) {
        Some(index) => ids.split_off(index),
        None => ids.pop().into_iter().collect(),
    }
}
/// The latest end date a tier may be given (the last second of the year 9999, Unix seconds).
pub const LATEST_TIER_END: u64 = 253_402_300_799;
/// Whether `tier` may be given the end date `until` (Unix seconds) at `now`: only a tier ranked
/// above Free and below Administrator ends, and only in the future, since an ended tier
/// returns the account to Free.
pub fn tier_end_allowed(policy: &Value, tier: &str, until: u64, now: u64) -> Result<()> {
    let ids = order(policy)?;
    let rank = |id: &str| ids.iter().position(|v| v == id);
    let ends = matches!(
        (rank(tier), rank(crate::FREE_TIER)),
        (Some(tier_rank), Some(free)) if tier_rank < free && tier != crate::ADMINISTRATOR_TIER
    );
    if !ends {
        bail!("Only a tier above Free and below Administrator can have an end date");
    }
    if until <= now || until > LATEST_TIER_END {
        bail!("A tier's end date must be in the future");
    }
    Ok(())
}
/// Whether `tier` may be granted by a paid subscription or given an end date: it exists and
/// ranks above Free and below Administrator (no other tier, never Administrator).
pub fn grantable(policy: &Value, tier: &str) -> Result<()> {
    let ids = order(policy)?;
    let rank = |id: &str| ids.iter().position(|v| v == id);
    match (rank(tier), rank(crate::FREE_TIER)) {
        (Some(tier_rank), Some(free)) if tier_rank < free && tier != crate::ADMINISTRATOR_TIER => {
            Ok(())
        }
        _ => bail!("Only a tier above Free and below Administrator can be granted"),
    }
}
/// The higher-ranked of two tiers; a tier missing from the ladder ranks lowest.
pub fn higher<'a>(policy: &Value, left: &'a str, right: &'a str) -> Result<&'a str> {
    let ids = order(policy)?;
    let rank = |id: &str| ids.iter().position(|v| v == id).unwrap_or(usize::MAX);
    Ok(if rank(right) < rank(left) {
        right
    } else {
        left
    })
}
pub fn effective(policy: &Value, id: &str) -> Result<Tier> {
    let ids = order(policy)?;
    let index = ids
        .iter()
        .position(|v| v == id)
        .ok_or_else(|| anyhow!("Account tier not found"))?;
    let mut result: Tier = serde_json::from_value(policy["tiers"][id].clone())?;
    result.validate()?;
    for upper in &ids[..index] {
        let tier: Tier = serde_json::from_value(policy["tiers"][upper].clone())?;
        tier.validate()?;
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
        // An upper level's daily limit caps every level below it (absent: no limit).
        result.daily_turns = crate::run_limits::lower(result.daily_turns, tier.daily_turns);
        result.owner_limits = result.owner_limits.within(&tier.owner_limits);
        if let Some(ceiling) = &tier.model_policy {
            if let Some(model) = &mut result.model_policy {
                model.allow_byo &= ceiling.allow_byo;
                model.max_byo_entries = model.max_byo_entries.min(ceiling.max_byo_entries);
            } else {
                result.model_policy = Some(crate::ModelPolicy {
                    allow_byo: ceiling.allow_byo,
                    max_byo_entries: ceiling.max_byo_entries,
                });
            }
        }
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn byo_limits_of_an_upper_account_level_cap_lower_levels() {
        let parent = json!({"id":"administrator","revision":1,"allowed_models":["*"],"max_sessions":8,"max_active_runs":2,
            "model_policy":{"allow_byo":false,"max_byo_entries":2}});
        let mut child = parent.clone();
        child["id"] = json!("free");
        child["model_policy"] = Value::Null;
        let mut policy = json!({"tiers":{"administrator":parent,"free":child},"tier_order":["administrator","free"]});
        let model = effective(&policy, "free").unwrap().model_policy.unwrap();
        assert!(!model.allow_byo);
        assert_eq!(model.max_byo_entries, 2);
        policy["tiers"]["free"]["model_policy"] = json!({"allow_byo":true,"max_byo_entries":5});
        let model = effective(&policy, "free").unwrap().model_policy.unwrap();
        assert!(!model.allow_byo);
        assert_eq!(model.max_byo_entries, 2);
    }
    #[test]
    fn a_tier_inherits_the_grants_of_every_tier_ranked_below_it() {
        let tier = |id: &str| json!({"id":id,"revision":1,"allowed_models":["*"],"max_sessions":8,"max_active_runs":2});
        let mut policy = json!({"tiers":{"administrator":tier("administrator"),"plus":tier("plus"),
            "free":tier("free"),"anonymous":tier("anonymous")},
            "tier_order":["administrator","plus","free","anonymous"]});
        let names = |ids: &[&str]| ids.iter().map(|id| (*id).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            inherited(&policy, "administrator"),
            names(&["administrator", "plus", "free", "anonymous"])
        );
        assert_eq!(
            inherited(&policy, "plus"),
            names(&["plus", "free", "anonymous"])
        );
        assert_eq!(inherited(&policy, "free"), names(&["free", "anonymous"]));
        // Unknown and missing tiers fall to the most restricted tier, never above it.
        assert_eq!(inherited(&policy, "gold"), names(&["anonymous"]));
        assert_eq!(inherited(&policy, ""), names(&["anonymous"]));
        // A broken ladder grants nothing.
        policy["tier_order"] = json!(["free", "administrator", "plus", "anonymous"]);
        assert!(inherited(&policy, "plus").is_empty());
    }
    /// The shipped policy ranks Plus between Administrator and Free, with every catalog model.
    #[test]
    fn the_shipped_policy_ranks_plus_above_free() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../config/access/policy.json"
        );
        let policy: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        crate::configuration::validate_configuration(&policy).unwrap();
        assert_eq!(
            order(&policy).unwrap(),
            ["administrator", "plus", "free", "anonymous"]
        );
        assert_eq!(policy["tier_names"]["plus"], "Plus");
        let plus = effective(&policy, "plus").unwrap();
        assert!(plus.allows_model("any-catalog-model"));
        // Provisional values that fit the server's 10 runs at once (spec 8.2).
        assert_eq!(plus.max_active_runs, 9);
        assert_eq!(plus.owner_limits.runs_at_once, Some(9));
        crate::run_limits::check_ceiling(&policy, 10).unwrap();
        assert!(crate::run_limits::check_ceiling(&policy, 8).is_err());
        assert_eq!(inherited(&policy, "plus"), ["plus", "free", "anonymous"]);
    }
    #[test]
    fn paid_grants_are_only_tiers_between_free_and_administrator() {
        let tier = |id: &str| json!({"id":id,"revision":1,"allowed_models":["*"],"max_sessions":8,"max_active_runs":2});
        let policy = json!({"tiers":{"administrator":tier("administrator"),"plus":tier("plus"),
            "free":tier("free"),"anonymous":tier("anonymous")},
            "tier_order":["administrator","plus","free","anonymous"]});
        grantable(&policy, "plus").unwrap();
        for id in ["administrator", "free", "anonymous", "gold", ""] {
            assert!(grantable(&policy, id).is_err(), "{id}");
        }
        assert_eq!(higher(&policy, "free", "plus").unwrap(), "plus");
        assert_eq!(
            higher(&policy, "administrator", "plus").unwrap(),
            "administrator"
        );
        assert_eq!(higher(&policy, "gold", "free").unwrap(), "free");
    }
    #[test]
    fn only_a_tier_between_free_and_administrator_ends_and_only_later() {
        let tier = |id: &str| json!({"id":id,"revision":1,"allowed_models":["*"],"max_sessions":8,"max_active_runs":2});
        let policy = json!({"tiers":{"administrator":tier("administrator"),"plus":tier("plus"),
            "free":tier("free"),"anonymous":tier("anonymous")},
            "tier_order":["administrator","plus","free","anonymous"]});
        tier_end_allowed(&policy, "plus", 101, 100).unwrap();
        for (id, until) in [
            ("administrator", 101),
            ("free", 101),
            ("anonymous", 101),
            ("gold", 101),
            ("plus", 100),
            ("plus", 99),
            ("plus", LATEST_TIER_END + 1),
        ] {
            assert!(
                tier_end_allowed(&policy, id, until, 100).is_err(),
                "{id} {until}"
            );
        }
    }
    #[test]
    fn upper_account_ceiling_limits_models_and_quotas() {
        let tier = json!({"id":"administrator","revision":1,"allowed_models":["model_a"],"max_sessions":8,"max_active_runs":2});
        let mut base = tier.clone();
        base["id"] = json!("free");
        base["allowed_models"] = json!(["*"]);
        base["max_sessions"] = json!(100);
        let policy = json!({"tiers":{"administrator":tier,"free":base},"tier_order":["administrator","free"]});
        let resolved = effective(&policy, "free").unwrap();
        assert_eq!(resolved.max_sessions, 8);
        assert_eq!(resolved.allowed_models, vec!["model_a"]);
        let mut invalid = policy;
        invalid["tier_order"] = json!(["free", "administrator"]);
        assert!(effective(&invalid, "free").is_err());
    }
}
