//! Access owns live policy publication, locking, revisions and validation.
use crate::configuration::{validate_configuration, validate_pages};
use crate::storage::{RecordCodec, Repository};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;
/// The live access policy (`live-files.json`), relative to the checkout.
pub const LIVE_POLICY: &str = "crates/modules/access/access-policy.json";
const POLICY_LIMIT: u64 = 4 * 1024 * 1024;
impl<C: RecordCodec> Repository<'_, C> {
    /// The live policy file in processes that save live files; a state-root file in fixtures.
    pub fn access_policy_path(&self) -> std::path::PathBuf {
        if webagent_configuration::live::writable() {
            return std::path::PathBuf::from(LIVE_POLICY);
        }
        self.root.join("config/access-policy.json")
    }
    fn policy_exists(&self) -> bool {
        let path = self.access_policy_path();
        path.is_relative() || path.exists()
    }
    pub fn policy_lock(&self) -> Result<std::fs::File> {
        use fs2::FileExt;
        let directory = self.root.join("config");
        std::fs::create_dir_all(&directory)?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(directory.join("access-policy.lock"))?;
        lock.lock_exclusive()?;
        Ok(lock)
    }
    pub fn read_access_policy(&self) -> Result<Value> {
        let parse = |bytes: &[u8]| -> Result<Value> {
            let value: Value = serde_json::from_slice(bytes)?;
            validate_configuration(&value)?;
            Ok(value)
        };
        let bytes = webagent_configuration::live::read_path(
            &self.access_policy_path(),
            POLICY_LIMIT,
            |bytes| parse(bytes).map(|_| ()),
        )?;
        parse(&bytes)
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
    pub async fn init_policy_tiers(&self) -> Result<()> {
        if self.read_access_policy()?.get("tiers").is_some() {
            return Ok(());
        }
        let rows = sqlx::query("SELECT value FROM entitlement_tiers ORDER BY tier_id")
            .fetch_all(self.app_pool)
            .await?;
        let mut tiers = serde_json::Map::new();
        for row in rows {
            let value = self.unseal(&row.get::<Vec<u8>, _>("value"))?;
            tiers.insert(
                value["id"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Invalid legacy tier"))?
                    .to_owned(),
                value,
            );
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
            let mut anonymous = policy["tiers"]["basic"].clone();
            if anonymous.is_null() {
                return Err(anyhow!("Base account tier missing"));
            }
            anonymous["id"] = json!("anonymous");
            policy["tiers"]["anonymous"] = anonymous;
        }
        policy["tier_order"] = json!(crate::account_ladder::order(&policy)?);
        policy["tier_names"] = json!({"operator":"Admin","basic":"Base","anonymous":"Anonymous"});
        policy["revision"] = json!(policy["revision"]
            .as_u64()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| anyhow!("Policy revision overflow"))?);
        self.write_access_policy(&policy)
    }
    pub fn write_access_policy(&self, value: &Value) -> Result<()> {
        use std::io::Write;
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
        use std::os::unix::fs::OpenOptionsExt;
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
            std::fs::File::open(directory)?.sync_all()?;
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
