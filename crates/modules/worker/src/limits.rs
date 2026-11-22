//! Resource limits for every workspace command and terminal, from the live
//! `config/worker/command-limits.json`. Each limit counts one command's whole process tree
//! (every process it starts, with their threads), not each process on its own.
use serde_json::{json, Value};
use std::io;
use std::path::Path;

/// Live limits file, relative to the checkout.
pub const LIVE_LIMITS: &str = "config/worker/command-limits.json";
const MAX_BYTES: u64 = 16 * 1024;
const MIB: u64 = 1024 * 1024;

/// What one command or terminal may use across all its processes together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandLimits {
    /// Private (anonymous) memory of all its processes together.
    pub memory_bytes: u64,
    /// Processes and threads it may run at once.
    pub processes: u64,
    /// CPU time of all its processes together, ended ones included.
    pub cpu_seconds: u64,
}

impl Default for CommandLimits {
    /// Used when the live file cannot be read.
    fn default() -> Self {
        Self {
            memory_bytes: 1024 * MIB,
            processes: 256,
            cpu_seconds: 1800,
        }
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn bounded(value: &Value, key: &str, min: u64, max: u64) -> io::Result<u64> {
    match value.get(key).and_then(Value::as_u64) {
        Some(number) if (min..=max).contains(&number) => Ok(number),
        _ => Err(invalid(format!(
            "{key} must be an integer from {min} to {max}"
        ))),
    }
}

impl CommandLimits {
    /// Parses and bounds the live file; unknown keys are refused.
    ///
    /// # Errors
    /// Invalid JSON, an unknown key or a value out of range.
    pub fn parse(bytes: &[u8]) -> io::Result<Self> {
        let value: Value = serde_json::from_slice(bytes)?;
        let Some(object) = value.as_object() else {
            return Err(invalid("Command limits must be an object"));
        };
        const KEYS: [&str; 5] = [
            "_instructions",
            "schema_version",
            "memory_mb",
            "processes",
            "cpu_seconds",
        ];
        if object.keys().any(|key| !KEYS.contains(&key.as_str())) {
            return Err(invalid("Unknown command limit"));
        }
        if value["schema_version"] != 1 {
            return Err(invalid("schema_version must be 1"));
        }
        Ok(Self {
            memory_bytes: bounded(&value, "memory_mb", 64, 16384)? * MIB,
            processes: bounded(&value, "processes", 8, 4096)?,
            cpu_seconds: bounded(&value, "cpu_seconds", 10, 36000)?,
        })
    }

    /// The limits in `path`, read on each command so edits apply to the next one; the
    /// defaults when the file is missing, too large or invalid.
    #[must_use]
    pub fn read(path: &Path) -> Self {
        std::fs::metadata(path)
            .ok()
            .filter(|meta| meta.len() <= MAX_BYTES)
            .and_then(|_| std::fs::read(path).ok())
            .and_then(|bytes| Self::parse(&bytes).ok())
            .unwrap_or_default()
    }

    /// The form the worker hands a supervisor (`V3_COMMAND_LIMITS`).
    #[must_use]
    pub fn to_env(self) -> String {
        json!({"memory_bytes":self.memory_bytes,"processes":self.processes,
            "cpu_seconds":self.cpu_seconds})
        .to_string()
    }

    /// Reads [`Self::to_env`]'s form; the defaults when it is absent or malformed, so a
    /// supervisor never runs a command unlimited.
    #[must_use]
    pub fn from_env(text: Option<&str>) -> Self {
        let parsed = text.and_then(|text| serde_json::from_str::<Value>(text).ok());
        let field = |key: &str| parsed.as_ref().and_then(|value| value[key].as_u64());
        match (
            field("memory_bytes"),
            field("processes"),
            field("cpu_seconds"),
        ) {
            (Some(memory_bytes), Some(processes), Some(cpu_seconds)) => Self {
                memory_bytes,
                processes,
                cpu_seconds,
            },
            _ => Self::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_file_parses_to_the_defaults() {
        let bytes = include_bytes!("../../../../config/worker/command-limits.json");
        assert_eq!(
            CommandLimits::parse(bytes).expect("valid"),
            CommandLimits::default()
        );
    }

    #[test]
    fn refuses_out_of_range_and_unknown() {
        let base = r#"{"schema_version":1,"memory_mb":1024,"processes":256,"cpu_seconds":1800"#;
        assert!(CommandLimits::parse(format!("{base}}}").as_bytes()).is_ok());
        assert!(CommandLimits::parse(format!("{base},\"extra\":1}}").as_bytes()).is_err());
        let tiny = base.replace("\"memory_mb\":1024", "\"memory_mb\":1");
        assert!(CommandLimits::parse(format!("{tiny}}}").as_bytes()).is_err());
        let missing = base.replace(",\"cpu_seconds\":1800", "");
        assert!(CommandLimits::parse(format!("{missing}}}").as_bytes()).is_err());
    }

    #[test]
    fn the_supervisor_form_round_trips_and_falls_back_to_the_defaults() {
        let limits = CommandLimits {
            memory_bytes: 128 * MIB,
            processes: 16,
            cpu_seconds: 20,
        };
        assert_eq!(CommandLimits::from_env(Some(&limits.to_env())), limits);
        assert_eq!(CommandLimits::from_env(None), CommandLimits::default());
        assert_eq!(
            CommandLimits::from_env(Some("{\"processes\":1}")),
            CommandLimits::default()
        );
    }

    #[test]
    fn a_missing_file_reads_as_the_defaults() {
        let dir = tempfile::tempdir().expect("temporary directory");
        assert_eq!(
            CommandLimits::read(&dir.path().join("absent.json")),
            CommandLimits::default()
        );
    }
}
