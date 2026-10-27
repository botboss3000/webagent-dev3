//! Mutable installation prompts; queued jobs retain a validated snapshot.
use crate::*;
pub(crate) use webagent_chat::naming::Config;
// Chat owns the live session-naming prompt files; the host supplies the defaults directory argument.
#[cfg(test)]
pub(crate) fn defaults()->Result<Config> {Config::defaults_at(None,&crate::live_assets::fallback_directory())}
#[cfg(test)]
pub(super) fn parse(value:&Value)->Result<Config> {Config::parse_at(value,None,&crate::live_assets::fallback_directory())}
pub(crate) fn load(root:&std::path::Path)->Result<Config> {Config::load_at(root,fixture_source(root),&crate::live_assets::fallback_directory())}
/// Tests may place a prompt tree in their state root; installations always read the live files.
#[cfg(test)]
fn fixture_source(root:&std::path::Path)->Option<&std::path::Path> {root.join(webagent_configuration::prompts::PROMPTS).is_dir().then_some(root)}
#[cfg(not(test))]
fn fixture_source(_root:&std::path::Path)->Option<&std::path::Path> {None}
pub(crate) fn for_session(session:&Value)->Result<Config> {Config::for_session_at(session,None,&crate::live_assets::fallback_directory())}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_files_load_and_invalid_settings_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        // No saved copy: the state root never affects the live prompt files.
        assert_eq!(load(root.path()).unwrap(), defaults().unwrap());
        assert!(!defaults().unwrap().prompt.is_empty());
        for (key, value) in [("prompt", json!("")), ("model_role", json!("voice")),
            ("attempts_per_job", json!(0)), ("retry_delay_ms", json!(-1)),
            ("request_timeout_seconds", json!(301)), ("recovery_retry_delay_seconds", json!(0)),
            ("max_job_attempts", json!(21)), ("typo", json!(1))] {
            let mut invalid = serde_json::to_value(defaults().unwrap()).unwrap();
            invalid[key] = value;
            assert!(parse(&invalid).is_err(), "{key}");
        }
        assert_eq!(parse(&json!("Prompt-only")).unwrap().prompt, "Prompt-only");
    }
}
