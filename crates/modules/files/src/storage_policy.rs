//! Per-account-tier storage limits from the live `storage-policy.json`.
//!
//! Tier keys are the access ladder's tier ids. A tier with no entry, or a `null` value, has no
//! limit. Agent-level overrides may only lower these values (later release).
use anyhow::{bail, Result};
use serde::Deserialize;
use std::collections::BTreeMap;

/// Live policy file, relative to the checkout.
pub const LIVE_POLICY: &str = "crates/modules/files/storage-policy.json";
const MAX_POLICY_BYTES: u64 = 64 * 1024;
const MIB: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoragePolicy {
    /// Usage share at which the user is told storage is nearly full.
    pub warning_percent: u8,
    /// Sessions idle this many days have their files compressed.
    pub compress_idle_days: u32,
    pub tiers: BTreeMap<String, TierStorage>,
    /// Compression before encryption; read and validated by `webagent_storage::compression`.
    #[serde(default)]
    pub compression: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TierStorage {
    /// Everything one user owns: database, files and workspace.
    pub user_storage_mb: Option<u64>,
    /// Agents an account at this tier may publish.
    pub max_published_agents: Option<u64>,
    /// Data folder size of each agent the account publishes.
    pub agent_storage_mb: Option<u64>,
}

impl StoragePolicy {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let policy: Self = serde_json::from_slice(bytes)?;
        if !(1..=100).contains(&policy.warning_percent) {
            bail!("warning_percent must be 1..100");
        }
        if policy.compress_idle_days == 0 {
            bail!("compress_idle_days must be positive");
        }
        for tier in policy.tiers.values() {
            for mb in [tier.user_storage_mb, tier.agent_storage_mb]
                .into_iter()
                .flatten()
            {
                if mb.checked_mul(MIB).is_none() {
                    bail!("Storage limit out of range");
                }
            }
        }
        Ok(policy)
    }
    /// Read the live policy file on every use.
    pub fn live() -> Result<Self> {
        Self::read_in(&webagent_configuration::live::roots())
    }
    pub fn read_in(roots: &[std::path::PathBuf]) -> Result<Self> {
        webagent_configuration::live::parsed_from(roots, LIVE_POLICY, MAX_POLICY_BYTES, Self::parse)
    }
    /// Data folder byte limit of each agent owned by an account at `tier`; `None` is unlimited.
    pub fn agent_limit(&self, tier: &str) -> Option<u64> {
        self.tiers
            .get(tier)
            .and_then(|t| t.agent_storage_mb)
            .map(|mb| mb * MIB)
    }
    /// Agents an account at `tier` may own; `None` is unlimited.
    pub fn max_agents(&self, tier: &str) -> Option<u64> {
        self.tiers.get(tier).and_then(|t| t.max_published_agents)
    }
    /// Refuse agent-level user limits above the app ceiling for the same tier.
    pub fn check_agent_limits(
        &self,
        limits: &serde_json::Value,
    ) -> std::result::Result<(), String> {
        let Some(limits) = limits.as_object() else {
            return Err("user_storage_mb must map account tiers to megabytes".into());
        };
        for (tier, value) in limits {
            if value.is_null() {
                continue;
            }
            let mb = value
                .as_u64()
                .ok_or_else(|| format!("Storage limit for {tier} must be a whole number of MB"))?;
            if !self.tiers.contains_key(tier) {
                return Err(format!("Unknown account tier {tier}"));
            }
            if let Some(ceiling) = self.user_limit(tier) {
                if mb * MIB > ceiling {
                    return Err(format!(
                        "Storage limit for {tier} exceeds the app ceiling of {} MB",
                        ceiling / MIB
                    ));
                }
            }
        }
        Ok(())
    }
    /// Byte limit for a user at `tier`; `None` is unlimited.
    pub fn user_limit(&self, tier: &str) -> Option<u64> {
        self.tiers
            .get(tier)
            .and_then(|t| t.user_storage_mb)
            .map(|mb| mb * MIB)
    }
}

/// An agent's lower limit for users at `tier`, from its effective chat configuration.
pub fn agent_user_limit(chat: &serde_json::Value, tier: &str) -> Option<u64> {
    chat["chat_limits"]["storage"]["user_storage_mb"][tier]
        .as_u64()
        .and_then(|mb| mb.checked_mul(MIB))
}

/// Refuse a write of `added` bytes that would take usage past `limit`.
pub fn check_user(limit: Option<u64>, used: u64, added: u64) -> std::result::Result<(), String> {
    match limit {
        Some(limit) if used.saturating_add(added) > limit => Err(format!(
            "Storage limit reached: {} of {} MB used",
            used.div_ceil(MIB),
            limit / MIB
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_policy_parses_and_names_only_ladder_tiers() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let policy = StoragePolicy::read_in(&[root.clone()]).unwrap();
        let access: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join("crates/modules/access/access-policy.json")).unwrap(),
        )
        .unwrap();
        for tier in policy.tiers.keys() {
            assert!(access["tiers"].get(tier).is_some(), "unknown tier {tier}");
        }
    }

    #[test]
    fn limits_and_validation() {
        let policy = StoragePolicy::parse(
            br#"{"warning_percent":80,"compress_idle_days":30,"tiers":{"basic":{"user_storage_mb":2,"max_published_agents":1,"agent_storage_mb":null}}}"#,
        )
        .unwrap();
        assert_eq!(policy.user_limit("basic"), Some(2 * MIB));
        assert_eq!(policy.user_limit("operator"), None);
        assert!(check_user(Some(2 * MIB), MIB, MIB).is_ok());
        assert_eq!(
            check_user(Some(2 * MIB), MIB, MIB + 1).unwrap_err(),
            "Storage limit reached: 1 of 2 MB used"
        );
        assert!(check_user(None, u64::MAX, 1).is_ok());
        assert_eq!(policy.max_agents("basic"), Some(1));
        assert_eq!(policy.agent_limit("basic"), None);
        let chat = serde_json::json!({"chat_limits":{"storage":{"user_storage_mb":{"basic":1}}}});
        assert_eq!(agent_user_limit(&chat, "basic"), Some(MIB));
        assert_eq!(agent_user_limit(&chat, "anonymous"), None);
        assert!(policy
            .check_agent_limits(&serde_json::json!({"basic":2,"other_unset":null}))
            .is_ok());
        assert_eq!(
            policy
                .check_agent_limits(&serde_json::json!({"basic":3}))
                .unwrap_err(),
            "Storage limit for basic exceeds the app ceiling of 2 MB"
        );
        assert!(policy
            .check_agent_limits(&serde_json::json!({"missing":1}))
            .is_err());
        assert!(policy
            .check_agent_limits(&serde_json::json!({"basic":"1"}))
            .is_err());
        for bad in [
            r#"{"warning_percent":0,"compress_idle_days":30,"tiers":{}}"#,
            r#"{"warning_percent":80,"compress_idle_days":0,"tiers":{}}"#,
            r#"{"warning_percent":80,"compress_idle_days":30,"tiers":{"basic":{"other":1}}}"#,
        ] {
            assert!(StoragePolicy::parse(bad.as_bytes()).is_err(), "{bad}");
        }
    }
}
