//! Running Browser Control limits from the live `browser-limits.json`.
use anyhow::{bail, Result};
use serde_json::Value;

/// Live limits file, relative to the checkout.
pub const LIVE_LIMITS: &str = "crates/modules/browser/browser-limits.json";
const MAX_BYTES: u64 = 16 * 1024;

/// How many private browsers run at once and how long an idle one stays open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub per_app: usize,
    pub per_user: usize,
    pub idle_seconds: u64,
    pub queued_idle_seconds: u64,
    /// How long a tool call waits in the queue before it answers that the browser is busy.
    pub max_wait_seconds: u64,
}

impl Default for Limits {
    /// Used when the live file cannot be read.
    fn default() -> Self {
        Self {
            per_app: 5,
            per_user: 4,
            idle_seconds: 180,
            queued_idle_seconds: 30,
            max_wait_seconds: 600,
        }
    }
}

fn bounded(value: &Value, key: &str, min: u64, max: u64) -> Result<u64> {
    match value.get(key).and_then(Value::as_u64) {
        Some(number) if (min..=max).contains(&number) => Ok(number),
        _ => bail!("{key} must be an integer from {min} to {max}"),
    }
}

impl Limits {
    /// Parses and bounds the live file; unknown keys are refused.
    ///
    /// # Errors
    /// Invalid JSON, an unknown key or a value out of range.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let value: Value = serde_json::from_slice(bytes)?;
        let Some(object) = value.as_object() else {
            bail!("Browser limits must be an object");
        };
        const KEYS: [&str; 7] = [
            "_instructions",
            "schema_version",
            "max_running_per_app",
            "max_running_per_user",
            "idle_seconds",
            "queued_idle_seconds",
            "max_wait_seconds",
        ];
        if object.keys().any(|key| !KEYS.contains(&key.as_str())) {
            bail!("Unknown browser limit");
        }
        if value["schema_version"] != 1 {
            bail!("schema_version must be 1");
        }
        let per_app = bounded(&value, "max_running_per_app", 1, 50)?;
        let per_user = bounded(&value, "max_running_per_user", 1, 50)?;
        let idle_seconds = bounded(&value, "idle_seconds", 30, 3600)?;
        let queued_idle_seconds = bounded(&value, "queued_idle_seconds", 10, idle_seconds)?;
        let max_wait_seconds = bounded(&value, "max_wait_seconds", 30, 3600)?;
        Ok(Self {
            per_app: usize::try_from(per_app)?,
            per_user: usize::try_from(per_user)?,
            idle_seconds,
            queued_idle_seconds,
            max_wait_seconds,
        })
    }

    /// The live limits; the defaults when the file is missing or invalid.
    #[must_use]
    pub fn live() -> Self {
        webagent_configuration::live::parsed(LIVE_LIMITS, MAX_BYTES, Self::parse)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_file_parses() {
        let bytes = include_bytes!("../browser-limits.json");
        let limits = Limits::parse(bytes).unwrap();
        assert_eq!(limits, Limits::default());
    }

    #[test]
    fn refuses_out_of_range_and_unknown() {
        let base = r#"{"schema_version":1,"max_running_per_app":5,"max_running_per_user":4,"idle_seconds":180,"queued_idle_seconds":30,"max_wait_seconds":600"#;
        assert!(Limits::parse(format!("{base}}}").as_bytes()).is_ok());
        assert!(Limits::parse(format!("{base},\"extra\":1}}").as_bytes()).is_err());
        let zero = base.replace("\"max_running_per_user\":4", "\"max_running_per_user\":0");
        assert!(Limits::parse(format!("{zero}}}").as_bytes()).is_err());
        let slow = base.replace("\"queued_idle_seconds\":30", "\"queued_idle_seconds\":600");
        assert!(Limits::parse(format!("{slow}}}").as_bytes()).is_err());
    }
}
