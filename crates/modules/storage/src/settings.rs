//! Storage consumes `config/app-settings.json#/admin_database_encryption/enabled`.
//! Generic Configuration owns that document's publication; Storage owns the meaning.
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::path::Path;

// Both API and loop processes read the same data-root policy. A missing file
// keeps encryption on; a malformed policy stops writes instead of exposing data.
pub fn read_app_settings(root: &Path) -> Result<Value> {
    match std::fs::read(root.join("config/app-settings.json")) {
        Ok(bytes) => {
            let value: Value = serde_json::from_slice(&bytes)?;
            if !value.is_object() {
                return Err(anyhow!("app-settings.json must be an object"));
            }
            Ok(value)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e.into()),
    }
}

/// Validate the optional administrator encryption switch; absence keeps encryption enabled.
pub fn admin_encryption_enabled(settings: &Value) -> Result<bool> {
    match settings.get("admin_database_encryption") {
        None => Ok(true),
        Some(value) => value["enabled"].as_bool().ok_or_else(|| {
            anyhow!("app-settings.json requires admin_database_encryption.enabled boolean")
        }),
    }
}
