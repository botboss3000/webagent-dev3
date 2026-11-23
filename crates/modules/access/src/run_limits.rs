//! Run-sharing limits from the access policy's `run_limits` section: slots kept for
//! administrators, the per-agent cap on active runs and daily usage limits per agent.
//! Per-account limits are on the account levels (`Tier`).
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The `run_limits` section; every field is optional in the file.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RunLimits {
    /// Loop run slots only administrators may take: ordinary runs start while fewer than
    /// `scheduler.max_active_runs - reserved_admin_runs` runs are going.
    pub reserved_admin_runs: u32,
    /// Active runs (queued, running, waiting for approval) of non-administrators on one agent;
    /// absent means half the loop's `scheduler.max_active_runs`, at least 2.
    pub max_active_runs_per_agent: Option<u32>,
    /// Model turns non-administrators may use on one agent per day (UTC); absent: no limit.
    pub agent_daily_turns: Option<u32>,
    /// Known provider cost non-administrators may use on one agent per day, in US dollars.
    pub agent_daily_cost_usd: Option<f64>,
}

impl Default for RunLimits {
    fn default() -> Self {
        Self {
            reserved_admin_runs: 1,
            max_active_runs_per_agent: None,
            agent_daily_turns: None,
            agent_daily_cost_usd: None,
        }
    }
}

impl RunLimits {
    /// Interpret the access-policy document; a missing section takes the defaults.
    pub fn from_policy(policy: &Value) -> Result<Self> {
        let Some(section) = policy.get("run_limits") else {
            return Ok(Self::default());
        };
        if !section.is_object() {
            bail!("Invalid run_limits policy: expected an object");
        }
        let limits: Self = serde_json::from_value(section.clone())
            .map_err(|error| anyhow!("Invalid run_limits policy: {error}"))?;
        if limits.reserved_admin_runs > 64 {
            bail!("run_limits.reserved_admin_runs must be 0..=64");
        }
        if limits
            .max_active_runs_per_agent
            .is_some_and(|cap| cap == 0 || cap > 1_000)
        {
            bail!("run_limits.max_active_runs_per_agent must be 1..=1000");
        }
        if limits
            .agent_daily_turns
            .is_some_and(|turns| turns == 0 || turns > 10_000_000)
        {
            bail!("run_limits.agent_daily_turns must be 1..=10000000");
        }
        if !valid_cost(limits.agent_daily_cost_usd) {
            bail!("run_limits.agent_daily_cost_usd must be a number from 0 to 1000000");
        }
        Ok(limits)
    }

    /// The per-agent cap for a loop ceiling of `global` runs (0: no ceiling, so no default cap).
    #[must_use]
    pub fn agent_cap(&self, global: usize) -> Option<usize> {
        match self.max_active_runs_per_agent {
            Some(cap) => usize::try_from(cap).ok(),
            None if global == 0 => None,
            None => Some((global / 2).max(2)),
        }
    }

    /// Runs going at which ordinary runs stop starting: the ceiling less the reserved slots,
    /// at least 1 (`None` when the loop has no ceiling).
    #[must_use]
    pub fn ordinary_ceiling(&self, global: usize) -> Option<usize> {
        let reserved = usize::try_from(self.reserved_admin_runs).unwrap_or(usize::MAX);
        (global != 0).then(|| global.saturating_sub(reserved).max(1))
    }
}

/// A cost limit is absent or a finite number from 0 to 1,000,000.
#[must_use]
pub fn valid_cost(cost: Option<f64>) -> bool {
    cost.is_none_or(|cost| cost.is_finite() && (0.0..=1_000_000.0).contains(&cost))
}

/// The stricter of two optional limits (absent means no limit).
#[must_use]
pub fn lower<T: PartialOrd>(left: Option<T>, right: Option<T>) -> Option<T> {
    match (left, right) {
        (Some(a), Some(b)) => Some(if b < a { b } else { a }),
        (a, None) => a,
        (None, b) => b,
    }
}

/// Usage counted for one requester or agent on one day.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DailyUsage {
    pub turns: u64,
    /// Known provider cost in millionths of a US dollar.
    pub cost_micros: u64,
}

/// Why a further model turn is refused, or `None` while within `turns` and `cost_usd`.
/// `whose` names the limit ("your account level", "this agent").
#[must_use]
pub fn daily_refusal(
    used: DailyUsage,
    turns: Option<u32>,
    cost_usd: Option<f64>,
    whose: &str,
) -> Option<String> {
    if let Some(limit) = turns {
        if used.turns >= u64::from(limit) {
            return Some(format!(
                "Daily usage limit reached: {whose} allows {limit} model turn{} a day. It resets at 00:00 UTC.",
                if limit == 1 { "" } else { "s" }
            ));
        }
    }
    if let Some(limit) = cost_usd {
        if used.cost_micros as f64 >= limit * 1_000_000.0 {
            return Some(format!(
                "Daily usage limit reached: {whose} allows ${limit:.2} of model use a day. It resets at 00:00 UTC."
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_bounds_and_derived_caps() {
        let limits = RunLimits::from_policy(&json!({})).unwrap();
        assert_eq!(limits, RunLimits::default());
        assert_eq!(limits.agent_cap(10), Some(5));
        assert_eq!(limits.agent_cap(3), Some(2));
        assert_eq!(limits.agent_cap(0), None);
        assert_eq!(limits.ordinary_ceiling(10), Some(9));
        assert_eq!(limits.ordinary_ceiling(1), Some(1));
        assert_eq!(limits.ordinary_ceiling(0), None);
        let set = RunLimits::from_policy(&json!({"run_limits":{"reserved_admin_runs":2,
            "max_active_runs_per_agent":3,"agent_daily_turns":100,"agent_daily_cost_usd":2.5}}))
        .unwrap();
        assert_eq!(set.agent_cap(10), Some(3));
        assert_eq!(set.ordinary_ceiling(10), Some(8));
        for bad in [
            json!({"run_limits":[]}),
            json!({"run_limits":{"reserved_admin_runs":65}}),
            json!({"run_limits":{"max_active_runs_per_agent":0}}),
            json!({"run_limits":{"agent_daily_turns":0}}),
            json!({"run_limits":{"agent_daily_cost_usd":-1}}),
            json!({"run_limits":{"unknown":1}}),
        ] {
            assert!(RunLimits::from_policy(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn daily_limits_refuse_at_the_limit_and_allow_below_it() {
        let used = DailyUsage {
            turns: 300,
            cost_micros: 999_999,
        };
        assert!(daily_refusal(used, Some(300), None, "your account level")
            .unwrap()
            .contains("300 model turns a day"));
        assert!(daily_refusal(used, Some(301), Some(1.0), "this agent").is_none());
        let spent = DailyUsage {
            turns: 1,
            cost_micros: 1_000_000,
        };
        assert!(daily_refusal(spent, None, Some(1.0), "this agent")
            .unwrap()
            .contains("$1.00"));
        assert!(daily_refusal(DailyUsage::default(), None, None, "x").is_none());
        assert_eq!(lower(Some(3), Some(2)), Some(2));
        assert_eq!(lower(None, Some(2)), Some(2));
        assert_eq!(lower::<u32>(None, None), None);
    }
}
