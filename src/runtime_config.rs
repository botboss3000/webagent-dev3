//! Native catalog and legacy-storage composition for Engine-owned runtime JSON.
use crate::workflow;
use anyhow::Result;
#[cfg(test)]
use std::path::{Path, PathBuf};
pub use webagent_engine::runtime::config::{
    Cache, Context, ModelBudget, Scheduler, Tools, Workflow,
};
#[derive(Clone)]
pub struct NativeSchemas;
impl webagent_engine::runtime::config::SchemaCatalog for NativeSchemas {
    fn schemas() -> Result<Vec<serde_json::Value>> {
        Ok(crate::tools::schemas())
    }
}
pub type Config = webagent_engine::runtime::config::Config<NativeSchemas>;
/// Save runtime settings: through the broker for the live file, directly for a fixture file.
pub(crate) async fn save(app: &crate::App, config: &Config) -> Result<()> {
    config.validate()?;
    let Some(relative) = app
        .runtime_path
        .to_str()
        .filter(|_| app.runtime_path.is_relative())
    else {
        return config.persist(&app.runtime_path);
    };
    let current = crate::live_files::current(relative, 1024 * 1024)?;
    crate::live_files::save(
        app,
        relative,
        &crate::live_files::pretty(&serde_json::to_value(config)?)?,
        &current,
    )
    .await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_and_validation() {
        let c = Config::default();
        c.validate().unwrap();
        assert_eq!(c.scheduler.max_active_runs, 0);
        let mut c = c;
        c.scheduler.max_active_runs = 16;
        c.validate().unwrap();
        c.scheduler.max_active_runs = 17;
        assert!(c.validate().is_err());
    }
    #[test]
    fn atomic_roundtrip_and_invalid_present() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config/loop-runtime.json");
        Config::default().persist(&p).unwrap();
        assert_eq!(Config::read(&p).unwrap().context.max_context_bytes, 262144);
        std::fs::write(&p, b"{}").unwrap();
        assert!(Config::read(&p).is_err());
    }
}

#[cfg(test)]
mod parallel_tool_tests {
    use super::*;
    #[test]
    fn old_config_gets_bounded_defaults_and_bad_modes_fail_closed() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config/loop-runtime.json");
        let mut old = serde_json::to_value(Config::default()).unwrap();
        old.as_object_mut().unwrap().remove("tools");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, serde_json::to_vec(&old).unwrap()).unwrap();
        let mut c = Config::read(&p).unwrap();
        assert_eq!(c.tools.max_parallel_calls, 4);
        assert_eq!(
            c.tools.parallel_modes.get("file_read").map(String::as_str),
            Some("concurrent")
        );
        assert!(c.tools.parallel_modes.get("file_write").is_none());
        c.tools
            .parallel_modes
            .insert("file_write".into(), "concurrent".into());
        c.validate().unwrap();
        c.tools
            .parallel_modes
            .insert("unknown".into(), "concurrent".into());
        assert!(c.validate().is_err());
        c.tools.parallel_modes.remove("unknown");
        c.tools
            .parallel_modes
            .insert("file_read".into(), "serial".into());
        assert!(c.validate().is_err());
    }
}
#[cfg(test)]
mod model_budget_tests {
    use super::*;
    // MODEL-BUDGET-REGRESSION: unknown models preserve bytes; explicit estimates tighten only.
    #[test]
    fn profiles_tighten_and_reserve_output() {
        let mut c = Config::default();
        let selection = crate::provider::ProviderSelection::parse(
            &serde_json::json!({"provider":"deepseek","model":"fixture"}),
            "unused",
        )
        .unwrap();
        assert_eq!(c.context.ceiling(&selection).unwrap(), 262144);
        c.context.model_profiles.insert(
            "deepseek/fixture".into(),
            ModelBudget {
                context_tokens: 1000,
                output_reserve_tokens: 100,
                bytes_per_token: 2,
            },
        );
        c.validate().unwrap();
        assert_eq!(c.context.ceiling(&selection).unwrap(), 1800);
        c.context
            .model_profiles
            .get_mut("deepseek/fixture")
            .unwrap()
            .output_reserve_tokens = 1000;
        assert!(c.validate().is_err());
    }
}

#[cfg(test)]
mod luna_budget_tests {
    use super::*;
    #[test]
    fn published_luna_context_and_operator_ceiling_are_independent() {
        let luna = crate::provider::ProviderSelection {
            limits: None,
            provider: crate::provider::ProviderId::OpenaiSubscription,
            model: "gpt-6-luna".into(),
        };
        let other = crate::provider::ProviderSelection {
            limits: None,
            provider: crate::provider::ProviderId::OpenaiSubscription,
            model: "fixture".into(),
        };
        let mut context = Config::default().context;
        assert_eq!(context.ceiling(&luna).unwrap(), 262_144);
        assert_eq!(context.ceiling(&other).unwrap(), 262_144);
        context.model_profiles.insert(
            "openai_subscription/gpt-6-luna".into(),
            ModelBudget {
                context_tokens: 1_000_000,
                output_reserve_tokens: 8192,
                bytes_per_token: 8,
            },
        );
        assert_eq!(context.ceiling(&luna).unwrap(), 262_144);
        context.model_profiles.insert(
            "openai_subscription/gpt-6-luna".into(),
            ModelBudget {
                context_tokens: 10_000,
                output_reserve_tokens: 1000,
                bytes_per_token: 2,
            },
        );
        assert_eq!(context.ceiling(&luna).unwrap(), 18_000);
        context.max_context_bytes = 4096;
        assert_eq!(context.ceiling(&luna).unwrap(), 4096);
    }
}

#[cfg(test)]
mod background_rescan_tests {
    use super::*;
    /// Legacy schema-two files preserve their values and default only the newly absent setting.
    #[test]
    fn absent_interval_defaults_without_rewriting_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loop-runtime.json");
        let mut value = serde_json::to_value(Config::default()).unwrap();
        value["scheduler"]
            .as_object_mut()
            .unwrap()
            .remove("background_rescan_interval_ms");
        value["scheduler"]["poll_interval_ms"] = serde_json::json!(700);
        let original = serde_json::to_vec(&value).unwrap();
        std::fs::write(&path, &original).unwrap();
        let config = Config::read(&path).unwrap();
        assert_eq!(config.scheduler.background_rescan_interval_ms, 5000);
        assert_eq!(config.scheduler.poll_interval_ms, 700);
        assert_eq!(std::fs::read(path).unwrap(), original);
    }
    /// Every positive u64 interval is representable by chunked host waiting.
    #[test]
    fn whole_positive_milliseconds_have_no_policy_ceiling() {
        for interval in [1, 250, 5000, 15000, u64::MAX] {
            let mut config = Config::default();
            config.scheduler.background_rescan_interval_ms = interval;
            config.validate().unwrap();
        }
        let mut config = Config::default();
        config.scheduler.background_rescan_interval_ms = 0;
        assert!(config.validate().is_err());
        for invalid in [
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!("250"),
            serde_json::Value::Null,
        ] {
            let mut value = serde_json::to_value(Config::default()).unwrap();
            value["scheduler"]["background_rescan_interval_ms"] = invalid;
            assert!(serde_json::from_value::<Config>(value).is_err());
        }
    }
}
