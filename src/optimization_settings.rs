use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    #[default]
    Balanced,
    Speed,
    Memory,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Profiling {
    None,
    Cpu,
    Allocations,
    #[default]
    Both,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    Quick,
    #[default]
    Standard,
    Extended,
}

/// Persisted preferences and the immutable effective budget admitted to the broker.
/// Deserialization is strict; callers must validate before saving or admitting work.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct OptimizationSettings {
    pub priority: Priority,
    pub attempts: u32,
    pub total_seconds: u32,
    pub profiling: Profiling,
    pub cpu_cores: u32,
    pub ram_mb: u32,
    pub coverage: Coverage,
    pub max_slowdown_percent: u32,
    pub max_memory_regression_percent: u32,
    pub allow_dependencies: bool,
}

impl Default for OptimizationSettings {
    fn default() -> Self {
        Self {
            priority: Priority::Balanced,
            attempts: 3,
            total_seconds: 900,
            profiling: Profiling::Both,
            cpu_cores: 1,
            ram_mb: 1024,
            coverage: Coverage::Standard,
            max_slowdown_percent: 0,
            max_memory_regression_percent: 0,
            allow_dependencies: false,
        }
    }
}

impl OptimizationSettings {
    pub fn validate(&self) -> Result<(), String> {
        for (name, value, low, high) in [
            ("attempts", self.attempts, 1, 10),
            ("total_seconds", self.total_seconds, 1, 1800),
            ("cpu_cores", self.cpu_cores, 1, 2),
            ("ram_mb", self.ram_mb, 128, 4096),
            ("max_slowdown_percent", self.max_slowdown_percent, 0, 25),
            (
                "max_memory_regression_percent",
                self.max_memory_regression_percent,
                0,
                25,
            ),
        ] {
            if !(low..=high).contains(&value) {
                return Err(format!("{name} must be between {low} and {high}"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptimizeRequest {
    pub prompt: String,
    pub settings: OptimizationSettings,
}

/// Parse only an explicit command at the start of the user message. Overrides
/// clone preferences and never mutate persisted settings. Flags are recognized
/// only before the request text; '--' terminates option parsing.
pub fn parse_optimize(
    input: &str,
    preferences: &OptimizationSettings,
) -> Result<Option<OptimizeRequest>, String> {
    let input = input.trim_start();
    let Some(rest) = input.strip_prefix("/optimize") else {
        return Ok(None);
    };
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return Ok(None);
    }
    let mut settings = preferences.clone();
    let mut remainder = rest.trim_start();
    let mut seen = std::collections::BTreeSet::new();
    while remainder.starts_with("--") {
        let end = remainder
            .find(char::is_whitespace)
            .unwrap_or(remainder.len());
        let option = &remainder[..end];
        remainder = remainder[end..].trim_start();
        if option == "--" {
            break;
        }
        let (key, value) = option[2..]
            .split_once('=')
            .ok_or_else(|| "optimization overrides require --key=value".to_string())?;
        if !seen.insert(key) {
            return Err(format!("duplicate optimization override: {key}"));
        }
        let number = || {
            value
                .parse::<u32>()
                .map_err(|_| format!("{key} requires an unsigned integer"))
        };
        match key {
            "attempts" => settings.attempts = number()?,
            "total_seconds" => settings.total_seconds = number()?,
            "cpu_cores" => settings.cpu_cores = number()?,
            "ram_mb" => settings.ram_mb = number()?,
            "max_slowdown_percent" => settings.max_slowdown_percent = number()?,
            "max_memory_regression_percent" => settings.max_memory_regression_percent = number()?,
            "priority" => {
                settings.priority = match value {
                    "balanced" => Priority::Balanced,
                    "speed" => Priority::Speed,
                    "memory" => Priority::Memory,
                    _ => return Err("priority must be balanced, speed or memory".into()),
                }
            }
            "profiling" => {
                settings.profiling = match value {
                    "none" => Profiling::None,
                    "cpu" => Profiling::Cpu,
                    "allocations" => Profiling::Allocations,
                    "both" => Profiling::Both,
                    _ => return Err("profiling must be none, cpu, allocations or both".into()),
                }
            }
            "coverage" => {
                settings.coverage = match value {
                    "quick" => Coverage::Quick,
                    "standard" => Coverage::Standard,
                    "extended" => Coverage::Extended,
                    _ => return Err("coverage must be quick, standard or extended".into()),
                }
            }
            "allow_dependencies" => {
                settings.allow_dependencies = match value {
                    "true" => true,
                    "false" => false,
                    _ => return Err("allow_dependencies must be true or false".into()),
                }
            }
            _ => return Err(format!("unknown optimization override: {key}")),
        }
    }
    settings.validate()?;
    let prompt = remainder.trim().to_string();
    if prompt.is_empty() {
        return Err("/optimize requires a workload request".into());
    }
    Ok(Some(OptimizeRequest { prompt, settings }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_persistence_are_strict() {
        let settings: OptimizationSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings, OptimizationSettings::default());
        assert_eq!(settings.attempts, 3);
        assert_eq!(settings.total_seconds, 900);
        assert_eq!((settings.cpu_cores, settings.ram_mb), (1, 1024));
        assert!(!settings.allow_dependencies);
        assert!(serde_json::from_str::<OptimizationSettings>(r#"{"unknown":1}"#).is_err());
        assert!(serde_json::from_str::<OptimizationSettings>(r#"{"attempts":-1}"#).is_err());
        assert_eq!(
            serde_json::from_value::<OptimizationSettings>(
                serde_json::to_value(&settings).unwrap()
            )
            .unwrap(),
            settings
        );
    }

    #[test]
    fn every_override_is_effective_without_persisting() {
        let preferences = OptimizationSettings::default();
        let parsed = parse_optimize("/optimize --priority=memory --attempts=2 --total_seconds=60 --profiling=allocations --cpu_cores=2 --ram_mb=4096 --coverage=extended --max_slowdown_percent=10 --max_memory_regression_percent=5 --allow_dependencies=true Measure recovery", &preferences).unwrap().unwrap();
        assert_eq!(parsed.prompt, "Measure recovery");
        assert_eq!(
            parsed.settings,
            OptimizationSettings {
                priority: Priority::Memory,
                attempts: 2,
                total_seconds: 60,
                profiling: Profiling::Allocations,
                cpu_cores: 2,
                ram_mb: 4096,
                coverage: Coverage::Extended,
                max_slowdown_percent: 10,
                max_memory_regression_percent: 5,
                allow_dependencies: true,
            }
        );
        assert_eq!(preferences, OptimizationSettings::default());
    }

    #[test]
    fn commands_do_not_trigger_from_prose_or_prefixes() {
        let preferences = OptimizationSettings::default();
        for input in [
            "Please /optimize workload",
            "/optimizer workload",
            "/optimize-now workload",
        ] {
            assert!(parse_optimize(input, &preferences).unwrap().is_none());
        }
        let parsed = parse_optimize(
            " /optimize -- --attempts=100 literal workload",
            &preferences,
        )
        .unwrap()
        .unwrap();
        assert_eq!(parsed.prompt, "--attempts=100 literal workload");
        assert_eq!(parsed.settings.attempts, 3);
        assert_eq!(
            parse_optimize("/optimize request with --attempts=100", &preferences)
                .unwrap()
                .unwrap()
                .prompt,
            "request with --attempts=100"
        );
    }

    #[test]
    fn invalid_override_rejects_whole_request() {
        let preferences = OptimizationSettings::default();
        for flag in [
            "--attempts=0",
            "--attempts=11",
            "--total_seconds=0",
            "--total_seconds=1801",
            "--cpu_cores=0",
            "--cpu_cores=3",
            "--ram_mb=127",
            "--ram_mb=4097",
            "--max_slowdown_percent=26",
            "--max_memory_regression_percent=26",
            "--allow_dependencies=yes",
            "--priority=unknown",
            "--coverage=unknown",
            "--profiling=unknown",
            "--attempts=-1",
            "--attempts=1.2",
            "--unknown=1",
            "--attempts",
            "--attempts=1 --attempts=2",
        ] {
            assert!(
                parse_optimize(&format!("/optimize {flag} workload"), &preferences).is_err(),
                "{flag}"
            );
        }
        assert!(parse_optimize("/optimize", &preferences).is_err());
        let mut invalid = preferences;
        invalid.ram_mb = 1;
        assert!(parse_optimize("/optimize workload", &invalid).is_err());
    }

    #[test]
    fn exact_budget_boundaries_are_valid() {
        for (attempts, total_seconds, cpu_cores, ram_mb, regression) in
            [(1, 1, 1, 128, 0), (10, 1800, 2, 4096, 25)]
        {
            OptimizationSettings {
                attempts,
                total_seconds,
                cpu_cores,
                ram_mb,
                max_slowdown_percent: regression,
                max_memory_regression_percent: regression,
                ..Default::default()
            }
            .validate()
            .unwrap();
        }
    }
}
