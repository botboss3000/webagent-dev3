//! Mutable installation prompts; queued jobs retain a validated snapshot.
use crate::*;
pub(crate) use webagent_chat::naming::Config;
// Chat owns `prompts.json#/session_naming` semantics; the host supplies the installed defaults directory.
#[cfg(test)]
pub(crate) fn defaults()->Result<Config> {Config::defaults_at(None,&crate::live_assets::fallback_directory())}
#[cfg(test)]
pub(super) fn parse(value:&Value)->Result<Config> {Config::parse_at(value,None,&crate::live_assets::fallback_directory())}
pub(crate) fn load(root:&std::path::Path)->Result<Config> {Config::load_at(root,None,&crate::live_assets::fallback_directory())}
pub(crate) fn for_session(session:&Value)->Result<Config> {Config::for_session_at(session,None,&crate::live_assets::fallback_directory())}
pub(crate) fn initialize(root:&std::path::Path)->Result<()> {webagent_chat::naming::initialize_at(root,None,&crate::live_assets::fallback_directory())}
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
        assert_eq!(load(root.path()).unwrap(), defaults().unwrap());
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
        assert_eq!(load(root.path()).unwrap(), defaults().unwrap());
        file["session_naming"]["prompt"] = json!("Custom title prompt");
        file["session_naming"]["max_job_attempts"] = json!(2);
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        initialize(root.path()).unwrap();
        let first = load(root.path()).unwrap();
        assert_eq!(first.prompt, "Custom title prompt");
        assert_eq!(first.max_job_attempts, 2);
        file["session_naming"]["prompt"] = json!("Later prompt");
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        assert_eq!(load(root.path()).unwrap().prompt, "Later prompt");
        assert_eq!(first.prompt, "Custom title prompt");
        for (key, value) in [("prompt", json!("")), ("model_role", json!("voice")),
            ("attempts_per_job", json!(0)), ("retry_delay_ms", json!(-1)),
            ("request_timeout_seconds", json!(301)), ("recovery_retry_delay_seconds", json!(0)),
            ("max_job_attempts", json!(21)), ("typo", json!(1))] {
            let mut invalid = serde_json::to_value(defaults().unwrap()).unwrap();
            invalid[key] = value;
            assert!(parse(&invalid).is_err(), "{key}");
        }
        std::fs::write(&path, b"malformed").unwrap();
        assert!(load(root.path()).is_err());
        assert!(initialize(root.path()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"malformed");
        assert_eq!(parse(&json!("Prompt-only")).unwrap().prompt, "Prompt-only");
    }
}
