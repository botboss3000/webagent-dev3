//! Storage owns the administrator database encryption switch in its live file
//! (`live-files.json`), read by both the API and the loop. The encryption key stays in
//! the state root; this switch is plain configuration shipped with the source.
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use webagent_configuration::live;

/// The live storage settings file, relative to the checkout.
pub const LIVE_SETTINGS: &str = "crates/modules/storage/storage-settings.json";
const SETTINGS_LIMIT: u64 = 64 * 1024;

/// The live file where the process saves through the broker; otherwise (tests) the state root.
pub fn settings_path(root: &Path) -> PathBuf {
    if live::writable() {
        return PathBuf::from(LIVE_SETTINGS);
    }
    root.join("config/storage-settings.json")
}

// A missing file keeps encryption on; a malformed one stops writes instead of exposing
// data (a broken live edit serves its last good version).
pub fn read_settings(root: &Path) -> Result<Value> {
    match live::parsed_path(&settings_path(root), SETTINGS_LIMIT, parse) {
        Err(error) if live::missing(&error) => Ok(json!({})),
        other => other,
    }
}

/// A settings object whose encryption switch, when present, is a boolean.
pub fn parse(bytes: &[u8]) -> Result<Value> {
    let value: Value = serde_json::from_slice(bytes)?;
    if !value.is_object() {
        return Err(anyhow!("storage-settings.json must be an object"));
    }
    admin_encryption_enabled(&value)?;
    Ok(value)
}

/// Validate the optional administrator encryption switch; absence keeps encryption enabled.
pub fn admin_encryption_enabled(settings: &Value) -> Result<bool> {
    match settings.get("admin_database_encryption") {
        None => Ok(true),
        Some(value) => value["enabled"].as_bool().ok_or_else(|| {
            anyhow!("storage-settings.json requires admin_database_encryption.enabled boolean")
        }),
    }
}
