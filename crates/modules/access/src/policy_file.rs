//! Access owns live policy publication, locking, revisions and validation.
use crate::configuration::{validate_configuration, validate_pages};
use crate::storage::{RecordCodec, Repository};
use crate::Tier;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use uuid::Uuid;
/// The live app policy (`live-files.json`), relative to the checkout: access, tiers and, in
/// its `storage` section, storage limits.
pub const LIVE_POLICY: &str = "config/access/policy.json";
const POLICY_LIMIT: u64 = 4 * 1024 * 1024;
/// The loop's live runtime file, read here only for its server-wide run limit.
const LOOP_RUNTIME: &str = "config/engine/loop-runtime.json";

/// The server-wide run limit, the loop's `scheduler.max_active_runs` (0: none). 0 too where
/// this process does not see the live files (fixtures) or the file cannot be read; the loop
/// itself refuses an unreadable runtime file.
pub fn server_run_ceiling() -> usize {
    if !webagent_configuration::live::writable() {
        return 0;
    }
    std::fs::read(LOOP_RUNTIME)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|runtime| runtime["scheduler"]["max_active_runs"].as_u64())
        .and_then(|ceiling| usize::try_from(ceiling).ok())
        .unwrap_or(0)
}

/// A policy as loaded: valid, and asking no tier for more runs at once than the server holds.
fn parse_policy(bytes: &[u8]) -> Result<Value> {
    let value: Value = serde_json::from_slice(bytes)?;
    validate_configuration(&value)?;
    crate::run_limits::check_ceiling(&value, server_run_ceiling())?;
    Ok(value)
}
/// Fresh-install tiers; which abilities their owners' agents get is in the ability manifests.
fn fresh_install_tiers() -> [Tier; 2] {
    let operator = Tier {
        id: crate::ADMINISTRATOR_TIER.into(),
        revision: 1,
        model_policy: None,
        allowed_models: vec!["*".into()],
        max_sessions: 10_000,
        max_active_runs: 200,
        daily_turns: None,
        owner_limits: Default::default(),
    };
    let basic = Tier {
        id: crate::FREE_TIER.into(),
        revision: 1,
        model_policy: None,
        allowed_models: Vec::new(),
        max_sessions: 10,
        max_active_runs: 2,
        daily_turns: None,
        owner_limits: Default::default(),
    };
    [operator, basic]
}
/// The live policy file in processes that save live files; a state-root file in fixtures.
pub fn policy_path(root: &std::path::Path) -> std::path::PathBuf {
    if webagent_configuration::live::writable() {
        return std::path::PathBuf::from(LIVE_POLICY);
    }
    root.join("config/access-policy.json")
}
fn parse_digested(bytes: &[u8]) -> Result<(Value, String)> {
    let value = parse_policy(bytes)?;
    let digest = webagent_configuration::digest(&value);
    Ok((value, digest))
}
/// The digest `live_access` records as the policy revision, from the same parser cache, for a
/// revision comparison (stream B4) that needs no account or agent read.
pub fn policy_digest(root: &std::path::Path) -> Result<String> {
    webagent_configuration::live::parsed_path(&policy_path(root), POLICY_LIMIT, parse_digested)
        .map(|(_, digest)| digest)
}
impl<C: RecordCodec> Repository<'_, C> {
    /// The live policy file in processes that save live files; a state-root file in fixtures.
    pub fn access_policy_path(&self) -> std::path::PathBuf {
        policy_path(self.root)
    }
    fn policy_exists(&self) -> bool {
        let path = self.access_policy_path();
        path.is_relative() || path.exists()
    }
    /// Serializes policy saves across the API and loop processes. Runtime coordination lives
    /// in the shared `/runtime/control` mount, never in `state/`; fixtures use their state root.
    pub fn policy_lock_path(&self) -> std::path::PathBuf {
        if webagent_configuration::live::writable() {
            // Cross-process lock for policy saves; app-only (the tool worker cannot reach it).
            return webagent_configuration::paths::runtime().join("control/access-policy.lock");
        }
        self.root.join("access-policy.lock")
    }
    pub fn policy_lock(&self) -> Result<std::fs::File> {
        let path = self.policy_lock_path();
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory)?;
        }
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)?;
        lock.lock()?;
        Ok(lock)
    }
    pub fn read_access_policy(&self) -> Result<Value> {
        webagent_configuration::live::parsed_path(
            &self.access_policy_path(),
            POLICY_LIMIT,
            parse_policy,
        )
    }
    /// The live policy with its digest, computed once per version of the file (the live
    /// parser cache keeps both) instead of hashing the whole policy at every access check.
    pub fn read_access_policy_digested(&self) -> Result<(Value, String)> {
        webagent_configuration::live::parsed_path(
            &self.access_policy_path(),
            POLICY_LIMIT,
            parse_digested,
        )
    }
    pub fn save_page_policy(&self, pages: Value, expected: u64) -> Result<Value> {
        validate_pages(&pages)?;
        let _lock = self.policy_lock()?;
        let mut policy = self.read_access_policy()?;
        if policy["revision"].as_u64().unwrap_or(0) != expected {
            return Err(anyhow!("Policy revision changed"));
        }
        policy["pages"] = pages;
        policy["revision"] = json!(expected
            .checked_add(1)
            .ok_or_else(|| anyhow!("Policy revision overflow"))?);
        self.write_access_policy(&policy)?;
        Ok(json!({"pages":policy["pages"],"revision":policy["revision"]}))
    }
    /// Seed the fresh-install tiers when the policy has none. Installed checkouts ship them in
    /// the live file, so this only fills state-root policies (fixtures).
    pub async fn init_policy_tiers(&self) -> Result<()> {
        if self.read_access_policy()?.get("tiers").is_some() {
            return Ok(());
        }
        let mut tiers = serde_json::Map::new();
        for tier in fresh_install_tiers() {
            tier.validate()?;
            tiers.insert(tier.id.clone(), serde_json::to_value(&tier)?);
        }
        let _lock = self.policy_lock()?;
        let mut policy = self.read_access_policy()?;
        if policy.get("tiers").is_none() {
            policy["tiers"] = Value::Object(tiers);
            policy["revision"] = json!(policy["revision"].as_u64().unwrap_or(0) + 1);
            self.write_access_policy(&policy)?;
        }
        Ok(())
    }
    pub fn ensure_account_ladder(&self) -> Result<()> {
        let _lock = self.policy_lock()?;
        let mut policy = self.read_access_policy()?;
        if policy.get("tier_order").is_some() {
            crate::account_ladder::order(&policy)?;
            return Ok(());
        }
        if policy["tiers"].get("anonymous").is_none() {
            let mut anonymous = policy["tiers"][crate::FREE_TIER].clone();
            if anonymous.is_null() {
                return Err(anyhow!("Free account tier missing"));
            }
            anonymous["id"] = json!("anonymous");
            policy["tiers"]["anonymous"] = anonymous;
        }
        policy["tier_order"] = json!(crate::account_ladder::order(&policy)?);
        policy["tier_names"] =
            json!({"administrator":"Administrator","free":"Free","anonymous":"Anonymous"});
        policy["revision"] = json!(policy["revision"]
            .as_u64()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| anyhow!("Policy revision overflow"))?);
        self.write_access_policy(&policy)
    }
    pub fn write_access_policy(&self, value: &Value) -> Result<()> {
        use std::io::Write;
        // Never write a policy the next load would refuse (it would fail every access check):
        // a tier asking for more runs at once than the server holds is refused here.
        crate::run_limits::check_ceiling(value, server_run_ceiling())?;
        let path = self.access_policy_path();
        if let Some(relative) = path.to_str().filter(|_| path.is_relative()) {
            // Callers hold policy_lock(); the broker also fences on the bytes read here.
            let current = webagent_configuration::live::read(relative, POLICY_LIMIT, |_| Ok(()))?;
            let mut body = serde_json::to_vec_pretty(value)?;
            body.push(b'\n');
            return webagent_configuration::live::write(relative, &body, &current);
        }
        let directory = path
            .parent()
            .ok_or_else(|| anyhow!("Invalid policy path"))?;
        std::fs::create_dir_all(directory)?;
        use webagent_platform::fs::OpenOptionsExt;
        let temporary = directory.join(format!(".access-policy-{}.tmp", Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(&serde_json::to_vec_pretty(value)?)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            std::fs::rename(&temporary, &path)?;
            webagent_platform::sync_dir(directory)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result?;
        Ok(())
    }
    pub fn init_access_policy(&self, legacy: Option<&Value>) -> Result<()> {
        if self.policy_exists() {
            self.read_access_policy()?;
            return Ok(());
        }

        let registration = webagent_accounts::RegistrationMode::from_legacy(legacy).policy_name();
        let _lock = self.policy_lock()?;
        if self.policy_exists() {
            self.read_access_policy()?;
            return Ok(());
        }
        self.write_access_policy(
            &json!({"schema_version":1,"revision":1,"registration":registration}),
        )
    }
    pub fn set_registration_policy(&self, registration: &str) -> Result<()> {
        self.set_registration_policy_revision(registration, None)
    }
    pub fn set_registration_policy_revision(
        &self,
        registration: &str,
        expected: Option<u64>,
    ) -> Result<()> {
        let mode = webagent_accounts::RegistrationMode::from_policy(
            &json!({"registration":registration}),
        )?;
        let _lock = self.policy_lock()?;
        let mut value = self.read_access_policy()?;
        if expected.is_some_and(|revision| revision != value["revision"].as_u64().unwrap_or(0)) {
            return Err(anyhow!("Policy revision changed"));
        }
        value["revision"] = json!(value["revision"]
            .as_u64()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| anyhow!("Policy revision overflow"))?);
        mode.write_policy(&mut value)?;
        self.write_access_policy(&value)
    }
}
