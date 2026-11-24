//! Running Browser Control limits from the live `browser-limits.json`, and the oldest Browser
//! Extension version the server still serves.
use anyhow::{bail, Result};
use serde_json::Value;

/// Live limits file, relative to the checkout.
pub const LIVE_LIMITS: &str = "config/browser/browser-limits.json";
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
    /// The oldest Browser Extension version that may take actions (D4); older installs are
    /// told to update.
    pub min_extension_version: (u32, u32, u32),
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
            min_extension_version: (0, 2, 0),
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
        const KEYS: [&str; 8] = [
            "_instructions",
            "schema_version",
            "max_running_per_app",
            "max_running_per_user",
            "idle_seconds",
            "queued_idle_seconds",
            "max_wait_seconds",
            "min_extension_version",
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
        // Optional: a file without it keeps the default minimum.
        let min_extension_version = match value.get("min_extension_version") {
            None => Self::default().min_extension_version,
            Some(text) => text
                .as_str()
                .and_then(crate::extension_version)
                .ok_or_else(|| anyhow::anyhow!("min_extension_version must look like 0.2.0"))?,
        };
        Ok(Self {
            per_app: usize::try_from(per_app)?,
            per_user: usize::try_from(per_user)?,
            idle_seconds,
            queued_idle_seconds,
            max_wait_seconds,
            min_extension_version,
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
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn shipped_file_parses_to_the_defaults() {
        // The file is live; a lasting change of one of its values changes `Limits::default`
        // in the same patch, so a missing or broken file falls back to what is in force.
        let bytes = include_bytes!("../../../../config/browser/browser-limits.json");
        assert_eq!(Limits::parse(bytes).unwrap(), Limits::default());
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
        let newer =
            Limits::parse(format!("{base},\"min_extension_version\":\"0.3.0\"}}").as_bytes());
        assert_eq!(newer.unwrap().min_extension_version, (0, 3, 0));
        assert_eq!(
            Limits::parse(format!("{base}}}").as_bytes())
                .unwrap()
                .min_extension_version,
            (0, 2, 0)
        );
        for bad in ["\"0.3\"", "3", "\"latest\""] {
            let text = format!("{base},\"min_extension_version\":{bad}}}");
            assert!(Limits::parse(text.as_bytes()).is_err(), "{bad}");
        }
    }
}
