//! Agent lifecycle, identity and named execution modes, independent of root App/Store.
//! Configuration stays in existing encrypted agent metadata; no agents.json is introduced.
pub mod modes;
pub mod repository;
pub mod service;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NewAgent {
    pub client_request_id: String,
    pub name: String,
    pub description: Option<String>,
}
impl NewAgent {
    pub fn validate(&self) -> Result<()> {
        if self.client_request_id.is_empty() || self.client_request_id.len() > 128 {
            bail!("Invalid agent fields");
        }
        validate_identity(Some(&self.name), self.description.as_deref())
    }
    pub fn request_digest(&self) -> Result<String> {
        digest(&json!({"name":self.name,"description":self.description}))
    }
    /// Keep the published request-derived identity and crash retry contract.
    pub fn id(&self, owner: &str) -> Result<String> {
        Ok(format!(
            "agent-{}",
            &digest(&json!({"owner":owner,"request_id":self.client_request_id}))?[..32]
        ))
    }
    pub fn receipt_key(&self, owner: &str) -> String {
        format!("receipt:model-agent:{owner}:{}", self.client_request_id)
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct IdentityUpdate {
    pub revision: i64,
    pub name: Option<String>,
    pub description: Option<String>,
    pub execution_modes: Option<Value>,
    pub default_execution_mode: Option<String>,
}
impl IdentityUpdate {
    pub fn validate(&self) -> Result<()> {
        if self.revision < 0
            || self.revision == i64::MAX
            || (self.name.is_none()
                && self.description.is_none()
                && self.execution_modes.is_none()
                && self.default_execution_mode.is_none())
        {
            bail!("Invalid identity update");
        }
        validate_identity(self.name.as_deref(), self.description.as_deref())
    }
    /// Update only Agents fields. Neighbor-owned settings and identity authority survive.
    pub fn apply(&self, metadata: &mut Value) -> Result<()> {
        if !metadata.is_object() {
            bail!("Invalid agent metadata");
        }
        let modes = modes::normalize(
            self.execution_modes
                .as_ref()
                .unwrap_or(&metadata["execution_modes"]),
        )?;
        let requested = self
            .default_execution_mode
            .as_deref()
            .unwrap_or(metadata["default_execution_mode"].as_str().unwrap_or("ask"));
        let default = if modes
            .as_array()
            .is_some_and(|entries| entries.iter().any(|m| m["id"] == requested))
        {
            requested.to_owned()
        } else if self.default_execution_mode.is_some() {
            bail!("Unknown default mode");
        } else {
            "ask".to_owned()
        };
        metadata["execution_modes"] = modes;
        metadata["default_execution_mode"] = json!(default);
        Ok(())
    }
}

pub fn validate_identity(name: Option<&str>, description: Option<&str>) -> Result<()> {
    if name.is_some_and(|s| s.trim().is_empty() || s.len() > 200)
        || description.is_some_and(|s| s.len() > 2000)
    {
        bail!("Name must contain 1–200 bytes; description at most 2000 bytes");
    }
    Ok(())
}

/// Archive statuses: `archived` (was active) and `archived_disabled` (was disabled).
pub fn archived(agent: &Value) -> bool {
    agent["status"]
        .as_str()
        .is_some_and(|status| status.starts_with("archived"))
}

pub fn validate_status(revision: u64, status: &str) -> Result<()> {
    if !matches!(status, "active" | "disabled") || revision >= i64::MAX as u64 {
        bail!("Invalid agent status");
    }
    Ok(())
}

fn digest(value: &Value) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(value)?)))
}

#[cfg(test)]
mod tests;
