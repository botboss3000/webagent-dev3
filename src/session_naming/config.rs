//! Mutable installation prompts; queued jobs retain a validated snapshot.
use crate::*;
use fs2::FileExt;
use std::io::Write;
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub prompt: String,
    pub model_role: model_selection::ModelRole,
    pub attempts_per_job: u64,
    pub retry_delay_ms: u64,
    pub request_timeout_seconds: u64,
    pub recovery_retry_delay_seconds: u64,
    pub max_job_attempts: u64,
}
impl Default for Config {
    fn default() -> Self {
        let shipped: Value = serde_json::from_slice(include_bytes!("../../config/prompts.json"))
            .expect("Valid shipped prompts");
        serde_json::from_value(shipped["session_naming"].clone()).expect("Valid shipped naming settings")
    }
}
impl Config {
    fn validate(&self) -> Result<()> {
        if self.prompt.trim().is_empty() || self.prompt.len() > 8192
            || !matches!(self.model_role, model_selection::ModelRole::System
                | model_selection::ModelRole::Standard | model_selection::ModelRole::Premium)
            || !(1..=10).contains(&self.attempts_per_job)
            || self.retry_delay_ms > 60_000
            || !(1..=300).contains(&self.request_timeout_seconds)
            || !(1..=86_400).contains(&self.recovery_retry_delay_seconds)
            || !(1..=20).contains(&self.max_job_attempts)
        { return Err(anyhow!("Invalid session naming settings")); }
        Ok(())
    }
    fn parse(value: &Value) -> Result<Self> {
        // Accept the earlier prompt-only format; missing settings use shipped defaults.
        let supplied = if value.is_string() { json!({"prompt":value}) } else { value.clone() };
        let mut resolved = serde_json::to_value(Self::default())?;
        let fields = supplied.as_object().ok_or_else(|| anyhow!("Invalid naming section"))?;
        for (key, value) in fields { resolved[key] = value.clone(); }
        let result: Self = serde_json::from_value(resolved)?;
        result.validate()?;
        Ok(result)
    }
    pub(crate) fn load(root: &std::path::Path) -> Self {
        let loaded = (|| -> Result<Self> {
            let path = root.join("config/prompts.json");
            if std::fs::metadata(&path)?.len() > 4 * 1024 * 1024 {
                return Err(anyhow!("Prompts file exceeds bound"));
            }
            let prompts: Value = serde_json::from_slice(&std::fs::read(path)?)?;
            if prompts.get("session_naming").is_none() { return Ok(Self::default()); }
            Self::parse(&prompts["session_naming"])
        })();
        match loaded {
            Ok(config) => config,
            Err(_) => {
                tracing::warn!("Session naming configuration unavailable; using shipped defaults");
                Self::default()
            }
        }
    }
    pub(crate) fn for_session(session: &Value) -> Result<Self> {
        if session["auto_naming"]["config"].is_null() { return Ok(Self::default()); }
        let result: Self = serde_json::from_value(session["auto_naming"]["config"].clone())?;
        result.validate()?;
        Ok(result)
    }
}
pub(crate) fn initialize(root: &std::path::Path) -> Result<()> {
    let directory = root.join("config");
    std::fs::create_dir_all(&directory)?;
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(directory.join(".prompts.lock"))?;
    lock.lock_exclusive()?;
    let path = directory.join("prompts.json");
    let original = match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let mut prompts: Value = serde_json::from_slice(original.as_deref()
        .unwrap_or(include_bytes!("../../config/prompts.json")))?;
    let object = prompts.as_object_mut().ok_or_else(|| anyhow!("Invalid prompts file"))?;
    if object.contains_key("session_naming") && original.is_some() { return Ok(()); }
    object.insert("session_naming".into(), serde_json::to_value(Config::default())?);
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    if let Ok(metadata) = std::fs::metadata(&path) {
        temporary.as_file().set_permissions(metadata.permissions())?;
    }
    serde_json::to_writer_pretty(&mut temporary, &prompts)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    if Some(std::fs::read(&path).unwrap_or_default()) != original && original.is_some() {
        return Err(anyhow!("Prompts changed; retry initialization"));
    }
    if original.is_none() && path.exists() {
        return Err(anyhow!("Prompts changed; retry initialization"));
    }
    temporary.persist(&path).map_err(|error| error.error)?;
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_creates_missing_file_once() {
        let root = tempfile::tempdir().unwrap();
        initialize(root.path()).unwrap();
        let path = root.path().join("config/prompts.json");
        assert!(path.exists());
        let before = std::fs::read(&path).unwrap();
        initialize(root.path()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(Config::load(root.path()), Config::default());
    }
    #[test]
    fn reload_validate_and_seed_without_replacing_other_prompts() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("config");
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("prompts.json");
        std::fs::write(&path, r#"{"base_instructions":"keep","manager_incoming_message":{"a":"keep criteria"}}"#).unwrap();
        initialize(root.path()).unwrap();
        let mut file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(file["base_instructions"], "keep");
        assert_eq!(file["manager_incoming_message"]["a"], "keep criteria");
        assert_eq!(Config::load(root.path()), Config::default());
        file["session_naming"]["prompt"] = json!("Custom title prompt");
        file["session_naming"]["max_job_attempts"] = json!(2);
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        initialize(root.path()).unwrap();
        let first = Config::load(root.path());
        assert_eq!(first.prompt, "Custom title prompt");
        assert_eq!(first.max_job_attempts, 2);
        file["session_naming"]["prompt"] = json!("Later prompt");
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        assert_eq!(Config::load(root.path()).prompt, "Later prompt");
        assert_eq!(first.prompt, "Custom title prompt");
        for (key, value) in [("prompt", json!("")), ("model_role", json!("voice")),
            ("attempts_per_job", json!(0)), ("retry_delay_ms", json!(-1)),
            ("request_timeout_seconds", json!(301)), ("recovery_retry_delay_seconds", json!(0)),
            ("max_job_attempts", json!(21)), ("typo", json!(1))] {
            let mut invalid = serde_json::to_value(Config::default()).unwrap();
            invalid[key] = value;
            assert!(Config::parse(&invalid).is_err(), "{key}");
        }
        std::fs::write(&path, b"malformed").unwrap();
        assert_eq!(Config::load(root.path()), Config::default());
        assert!(initialize(root.path()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"malformed");
        assert_eq!(Config::parse(&json!("Prompt-only")).unwrap().prompt, "Prompt-only");
    }
}
