//! Access owns live policy publication, locking, revisions and validation.
use crate::storage::{RecordCodec,Repository};
use crate::configuration::{validate_configuration,validate_pages};
use anyhow::{anyhow,Result};
use serde_json::{json,Value};
use sqlx::Row;
use uuid::Uuid;
impl<C: RecordCodec> Repository<'_, C> {
    pub fn access_policy_path(&self) -> std::path::PathBuf {
        self.root.join("config/access-policy.json")
    }
    pub fn policy_lock(&self) -> Result<std::fs::File> {
        use fs2::FileExt;
        let directory=self.root.join("config");
        std::fs::create_dir_all(&directory)?;
        let lock=std::fs::OpenOptions::new().create(true).read(true).write(true).open(directory.join("access-policy.lock"))?;
        lock.lock_exclusive()?;
        Ok(lock)
    }
    pub fn read_access_policy(&self) -> Result<Value> {
        let value: Value = serde_json::from_slice(&std::fs::read(self.access_policy_path())?)?;
        validate_configuration(&value)?;
        Ok(value)
    }
    pub fn save_page_policy(&self, pages:Value, expected:u64) -> Result<Value> {
        validate_pages(&pages)?;
        let _lock=self.policy_lock()?;
        let mut policy=self.read_access_policy()?;
        if policy["revision"].as_u64().unwrap_or(0)!=expected {return Err(anyhow!("Policy revision changed"));}
        policy["pages"]=pages;
        policy["revision"]=json!(expected.checked_add(1).ok_or_else(||anyhow!("Policy revision overflow"))?);
        self.write_access_policy(&policy)?;
        Ok(json!({"pages":policy["pages"],"revision":policy["revision"]}))
    }
    pub async fn init_policy_tiers(&self) -> Result<()> {
        if self.read_access_policy()?.get("tiers").is_some() {return Ok(());}
        let rows=sqlx::query("SELECT value FROM entitlement_tiers ORDER BY tier_id").fetch_all(self.app_pool).await?;
        let mut tiers=serde_json::Map::new();
        for row in rows {let value=self.unseal(&row.get::<Vec<u8>,_>("value"))?;tiers.insert(value["id"].as_str().ok_or_else(||anyhow!("Invalid legacy tier"))?.to_owned(),value);}
        let _lock=self.policy_lock()?;
        let mut policy=self.read_access_policy()?;
        if policy.get("tiers").is_none() {policy["tiers"]=Value::Object(tiers);policy["revision"]=json!(policy["revision"].as_u64().unwrap_or(0)+1);self.write_access_policy(&policy)?;}
        Ok(())
    }
    pub fn write_access_policy(&self, value: &Value) -> Result<()> {
        use std::io::Write;
        let path=self.access_policy_path();
        let directory=path.parent().ok_or_else(||anyhow!("Invalid policy path"))?;
        std::fs::create_dir_all(directory)?;
        use std::os::unix::fs::OpenOptionsExt;
        let temporary=directory.join(format!(".access-policy-{}.tmp",Uuid::new_v4()));
        let result=(|| -> Result<()> {
            let mut file=std::fs::OpenOptions::new().write(true).create_new(true)
                .mode(0o600).open(&temporary)?;
            file.write_all(&serde_json::to_vec_pretty(value)?)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            std::fs::rename(&temporary,&path)?;
            std::fs::File::open(directory)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {let _=std::fs::remove_file(&temporary);}
        result?;
        Ok(())
    }
    pub fn init_access_policy(&self, legacy: Option<&Value>) -> Result<()> {
        if self.access_policy_path().exists() {
            self.read_access_policy()?;
            return Ok(());
        }

        let registration=if legacy.and_then(|v|v["access_mode"].as_str())==Some("public_registered") {"open"} else {"restricted"};
        let _lock=self.policy_lock()?;
        if self.access_policy_path().exists() {self.read_access_policy()?;return Ok(());}
        self.write_access_policy(&json!({"schema_version":1,"revision":1,"registration":registration}))
    }
    pub fn set_registration_policy(&self, registration: &str) -> Result<()> {
        self.set_registration_policy_revision(registration,None)
    }
    pub fn set_registration_policy_revision(&self, registration: &str, expected:Option<u64>) -> Result<()> {
        if !matches!(registration,"open"|"restricted") {return Err(anyhow!("Invalid registration policy"));}
        let _lock=self.policy_lock()?;
        let mut value=self.read_access_policy()?;
        if expected.is_some_and(|revision|revision!=value["revision"].as_u64().unwrap_or(0)) {return Err(anyhow!("Policy revision changed"));}
        value["revision"]=json!(value["revision"].as_u64().unwrap_or(0).checked_add(1).ok_or_else(||anyhow!("Policy revision overflow"))?);
        value["registration"]=json!(registration);
        self.write_access_policy(&value)
    }
}
