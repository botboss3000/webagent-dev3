//! Run-sharing limits from the access policy's `run_limits` section: slots kept for
//! administrators, the per-agent cap on active runs and daily usage limits per agent.
//! Per-account limits are on the account levels (`Tier`). Limits are counts only (turns,
//! processor seconds): the open app has no money limit (plus tier spec 8.1).
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
    /// Processor seconds all `shell` and `python` commands of one run may use together.
    pub command_cpu_seconds_per_run: Option<u32>,
    /// Processor seconds one account's commands may use per day (UTC).
    pub command_cpu_seconds_per_user_day: Option<u32>,
    /// Commands one run may have going at once.
    pub concurrent_commands_per_run: Option<u32>,
    /// Commands one account's runs may have going at once.
    pub concurrent_commands_per_user: Option<u32>,
    /// Ordinary run slots kept for agents whose owner is on a paid tier (ranked above Free,
    /// below Administrator): runs on other agents (Free) may hold at most the ordinary
    /// ceiling less this many. 0 keeps nothing back; slots are never held empty for long,
    /// since a Free run past `free_max_wait_seconds` is no longer held back.
    pub plus_reserved_runs: u32,
    /// Seconds a Free run waits before it is served like a paid one (first in line and not
    /// held back by `plus_reserved_runs`), so Free is slowed, never starved.
    pub free_max_wait_seconds: u32,
}

impl Default for RunLimits {
    fn default() -> Self {
        Self {
            reserved_admin_runs: 1,
            max_active_runs_per_agent: None,
            agent_daily_turns: None,
            command_cpu_seconds_per_run: Some(3_600),
            command_cpu_seconds_per_user_day: Some(14_400),
            concurrent_commands_per_run: Some(2),
            concurrent_commands_per_user: Some(4),
            plus_reserved_runs: 0,
            free_max_wait_seconds: 120,
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
        if limits.plus_reserved_runs > 64 {
            bail!("run_limits.plus_reserved_runs must be 0..=64");
        }
        if limits.free_max_wait_seconds > 86_400 {
            bail!("run_limits.free_max_wait_seconds must be 0..=86400");
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
        for (name, value, max) in [
            (
                "command_cpu_seconds_per_run",
                limits.command_cpu_seconds_per_run,
                360_000,
            ),
            (
                "command_cpu_seconds_per_user_day",
                limits.command_cpu_seconds_per_user_day,
                8_640_000,
            ),
            (
                "concurrent_commands_per_run",
                limits.concurrent_commands_per_run,
                64,
            ),
            (
                "concurrent_commands_per_user",
                limits.concurrent_commands_per_user,
                256,
            ),
        ] {
            if value.is_some_and(|value| value == 0 || value > max) {
                bail!("run_limits.{name} must be 1..={max}");
            }
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

    /// Held slots Free runs may take at once: the ordinary ceiling less `plus_reserved_runs`,
    /// at least 1 (`None`: nothing is kept back).
    #[must_use]
    pub fn free_cap(&self, global: usize) -> Option<usize> {
        let reserved = usize::try_from(self.plus_reserved_runs).unwrap_or(usize::MAX);
        if reserved == 0 {
            return None;
        }
        self.ordinary_ceiling(global)
            .map(|ordinary| ordinary.saturating_sub(reserved).max(1))
    }
}

/// Refuse a policy whose tier values ask for more runs at once than the server can hold
/// (plus tier spec 8.2): every tier but Administrator's `max_active_runs` and owner limits'
/// `runs_at_once` and `agent_users_at_once` must be at most `ceiling`, the loop's
/// `scheduler.max_active_runs` (0: no ceiling, nothing to check).
pub fn check_ceiling(policy: &Value, ceiling: usize) -> Result<()> {
    if ceiling == 0 {
        return Ok(());
    }
    for (id, definition) in policy["tiers"].as_object().into_iter().flatten() {
        if id == crate::ADMINISTRATOR_TIER {
            continue;
        }
        let owner = &definition["owner_limits"];
        for (name, value) in [
            ("max_active_runs", &definition["max_active_runs"]),
            ("owner_limits.runs_at_once", &owner["runs_at_once"]),
            (
                "owner_limits.agent_users_at_once",
                &owner["agent_users_at_once"],
            ),
        ] {
            if value
                .as_u64()
                .is_some_and(|value| value > u64::try_from(ceiling).unwrap_or(u64::MAX))
            {
                bail!(
                    "Tier {id}: {name} {value} is above the server's limit of {ceiling} runs at once"
                );
            }
        }
    }
    Ok(())
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

/// Why a further model turn is refused after `used` turns today, or `None` while within
/// `turns`. `whose` names the limit ("your account level", "this agent").
#[must_use]
pub fn daily_refusal(used: u64, turns: Option<u32>, whose: &str) -> Option<String> {
    let limit = turns?;
    (used >= u64::from(limit)).then(|| {
        format!(
            "Daily usage limit reached: {whose} allows {limit} model turn{} a day. It resets at 00:00 UTC.",
            if limit == 1 { "" } else { "s" }
        )
    })
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
            "max_active_runs_per_agent":3,"agent_daily_turns":100}}))
        .unwrap();
        assert_eq!(set.agent_cap(10), Some(3));
        assert_eq!(set.ordinary_ceiling(10), Some(8));
        for bad in [
            json!({"run_limits":[]}),
            json!({"run_limits":{"reserved_admin_runs":65}}),
            json!({"run_limits":{"max_active_runs_per_agent":0}}),
            json!({"run_limits":{"agent_daily_turns":0}}),
            json!({"run_limits":{"agent_daily_cost_usd":1}}),
            json!({"run_limits":{"unknown":1}}),
            json!({"run_limits":{"command_cpu_seconds_per_run":0}}),
            json!({"run_limits":{"concurrent_commands_per_user":257}}),
        ] {
            assert!(RunLimits::from_policy(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn tier_run_values_above_the_server_limit_are_refused_except_administrators() {
        let policy = |plus: u64, owner: u64| {
            json!({"tiers":{"administrator":{"max_active_runs":200},
                "plus":{"max_active_runs":plus,"owner_limits":{"runs_at_once":owner,
                "agent_users_at_once":3}},"free":{"max_active_runs":2}}})
        };
        check_ceiling(&policy(10, 9), 10).unwrap();
        check_ceiling(&policy(25, 25), 0).unwrap();
        let refused = check_ceiling(&policy(25, 9), 10).unwrap_err().to_string();
        assert!(
            refused.contains("Tier plus: max_active_runs 25"),
            "{refused}"
        );
        assert!(check_ceiling(&policy(9, 11), 10).is_err());
        // Free's cap keeps the paid reserve back from the ordinary ceiling, at least 1.
        let mut limits = RunLimits::default();
        assert_eq!(limits.free_cap(10), None);
        limits.plus_reserved_runs = 3;
        assert_eq!(limits.free_cap(10), Some(6));
        limits.plus_reserved_runs = 20;
        assert_eq!(limits.free_cap(10), Some(1));
        assert_eq!(limits.free_cap(0), None);
        assert!(RunLimits::from_policy(&json!({"run_limits":{"plus_reserved_runs":65}})).is_err());
    }

    #[test]
    fn daily_limits_refuse_at_the_limit_and_allow_below_it() {
        assert!(daily_refusal(300, Some(300), "your account level")
            .unwrap()
            .contains("300 model turns a day"));
        assert!(daily_refusal(300, Some(301), "this agent").is_none());
        assert!(daily_refusal(u64::MAX, None, "x").is_none());
        assert_eq!(lower(Some(3), Some(2)), Some(2));
        assert_eq!(lower(None, Some(2)), Some(2));
        assert_eq!(lower::<u32>(None, None), None);
    }
}
