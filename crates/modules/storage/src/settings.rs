//! Storage owns the administrator database encryption switch in its live file
//! (`live-files.json`), read by both the API and the loop. The encryption key stays in
//! the state root; this switch is plain configuration shipped with the source.
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use webagent_configuration::live;

/// The live storage settings file, relative to the checkout.
pub const LIVE_SETTINGS: &str = "config/storage/storage-settings.json";
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
    max_connected_users(&value)?;
    pool_connections(&value)?;
    idle_seconds(&value)?;
    connect_wait_ms(&value)?;
    max_blocking_threads(&value)?;
    deleted_session_retention_days(&value)?;
    Ok(value)
}

/// The optional bound on account databases holding open connections at once
/// (`user_database_pools.max_connected`, at least [`crate::pool::MIN_CONNECTED_USERS`]);
/// absence keeps [`crate::pool::DEFAULT_MAX_CONNECTED_USERS`].
pub fn max_connected_users(settings: &Value) -> Result<usize> {
    let Some(value) = settings.pointer("/user_database_pools/max_connected") else {
        return Ok(crate::pool::DEFAULT_MAX_CONNECTED_USERS);
    };
    value
        .as_u64()
        .and_then(|limit| usize::try_from(limit).ok())
        .filter(|limit| *limit >= crate::pool::MIN_CONNECTED_USERS)
        .ok_or_else(|| {
            anyhow!(
                "storage-settings.json user_database_pools.max_connected must be an integer of at least {}",
                crate::pool::MIN_CONNECTED_USERS
            )
        })
}

/// Connections one account database may hold open (`user_database_pools.max_connections`,
/// 1 to [`crate::pool::MAX_POOL_CONNECTIONS`]); absence keeps the default.
pub fn pool_connections(settings: &Value) -> Result<u32> {
    let Some(value) = settings.pointer("/user_database_pools/max_connections") else {
        return Ok(crate::pool::DEFAULT_POOL_CONNECTIONS);
    };
    value
        .as_u64()
        .filter(|connections| (1..=u64::from(crate::pool::MAX_POOL_CONNECTIONS)).contains(connections))
        .map(|connections| connections as u32)
        .ok_or_else(|| {
            anyhow!(
                "storage-settings.json user_database_pools.max_connections must be an integer from 1 to {}",
                crate::pool::MAX_POOL_CONNECTIONS
            )
        })
}

/// Seconds an idle account connection stays open (`user_database_pools.idle_seconds`, 1–60);
/// absence keeps [`crate::pool::DEFAULT_IDLE_SECONDS`].
pub fn idle_seconds(settings: &Value) -> Result<u64> {
    let Some(value) = settings.pointer("/user_database_pools/idle_seconds") else {
        return Ok(crate::pool::DEFAULT_IDLE_SECONDS);
    };
    value
        .as_u64()
        .filter(|seconds| (1..=crate::pool::MAX_IDLE_SECONDS).contains(seconds))
        .ok_or_else(|| {
            anyhow!(
                "storage-settings.json user_database_pools.idle_seconds must be an integer from 1 to {}",
                crate::pool::MAX_IDLE_SECONDS
            )
        })
}

/// Milliseconds a request waits for a free account-database slot
/// (`user_database_pools.connect_wait_ms`, 0–10000); absence keeps the default.
pub fn connect_wait_ms(settings: &Value) -> Result<u64> {
    match settings.pointer("/user_database_pools/connect_wait_ms") {
        None => Ok(crate::pool::DEFAULT_CONNECT_WAIT_MS),
        Some(value) => value
            .as_u64()
            .filter(|millis| *millis <= crate::pool::MAX_CONNECT_WAIT_MS)
            .ok_or_else(|| anyhow!(
                "storage-settings.json user_database_pools.connect_wait_ms must be an integer from 0 to {}",
                crate::pool::MAX_CONNECT_WAIT_MS
            )),
    }
}

/// Threads the runtime may add for blocking work (`process.max_blocking_threads`, 8–256);
/// absence keeps [`DEFAULT_MAX_BLOCKING_THREADS`].
pub fn max_blocking_threads(settings: &Value) -> Result<usize> {
    match settings.pointer("/process/max_blocking_threads") {
        None => Ok(DEFAULT_MAX_BLOCKING_THREADS),
        Some(value) => value
            .as_u64()
            .and_then(|threads| usize::try_from(threads).ok())
            .filter(|threads| (8..=256).contains(threads))
            .ok_or_else(|| anyhow!(
                "storage-settings.json process.max_blocking_threads must be an integer from 8 to 256"
            )),
    }
}

/// Runtime blocking threads unless the settings say otherwise.
pub const DEFAULT_MAX_BLOCKING_THREADS: usize = 64;

/// Days a deleted session's remaining records are kept before they are purged
/// (`deleted_sessions.retention_days`, at least 1); absence keeps
/// [`DEFAULT_DELETED_SESSION_RETENTION_DAYS`].
pub fn deleted_session_retention_days(settings: &Value) -> Result<u64> {
    let Some(value) = settings.pointer("/deleted_sessions/retention_days") else {
        return Ok(DEFAULT_DELETED_SESSION_RETENTION_DAYS);
    };
    value.as_u64().filter(|days| *days >= 1).ok_or_else(|| {
        anyhow!("storage-settings.json deleted_sessions.retention_days must be an integer of at least 1")
    })
}

/// Retention of deleted sessions' remaining records unless the settings say otherwise.
pub const DEFAULT_DELETED_SESSION_RETENTION_DAYS: u64 = 30;

/// Validate the optional administrator encryption switch; absence keeps encryption enabled.
pub fn admin_encryption_enabled(settings: &Value) -> Result<bool> {
    match settings.get("admin_database_encryption") {
        None => Ok(true),
        Some(value) => value["enabled"].as_bool().ok_or_else(|| {
            anyhow!("storage-settings.json requires admin_database_encryption.enabled boolean")
        }),
    }
}
