//! Root supplies existing registered-agent selection and configuration owner operations.
use anyhow::Result;
pub use webagent_manager::configuration::IncomingMessage;
pub async fn incoming(store:&crate::store::Store,agent:&str)->Result<IncomingMessage> {
 webagent_manager::settings::incoming(&store.manager_repository(),agent).await
}
pub async fn snapshot(store:&crate::store::Store,agent:&str)->Result<(IncomingMessage,u64)> {
 webagent_manager::settings::snapshot(&store.manager_repository(),agent).await
}
pub(crate) fn save_prompt(root:&std::path::Path,agent:&str,prompt:&str)->Result<()> {
 webagent_manager::settings::save_prompt(root,agent,prompt,crate::live_assets::prompt_defaults)
}
#[cfg(test)]fn read_prompt(root:&std::path::Path,agent:&str)->Result<Option<String>> {
 webagent_manager::settings::read_prompt(root,agent,crate::live_assets::resolved_prompts)
}
#[cfg(test)]
mod prompt_file_tests {
    use super::*;
    #[test]
    fn prompts_are_agent_scoped_and_preserve_existing_fields() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("config")).unwrap();
        let path = root.path().join("config/prompts.json");
        std::fs::write(&path, r#"{"base_instructions":"keep me","other":{"x":1}}"#).unwrap();
        save_prompt(root.path(), "one", "Clothing only").unwrap();
        save_prompt(root.path(), "two", "Hardware only").unwrap();
        assert_eq!(
            read_prompt(root.path(), "one").unwrap().as_deref(),
            Some("Clothing only")
        );
        assert_eq!(
            read_prompt(root.path(), "two").unwrap().as_deref(),
            Some("Hardware only")
        );
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(value["base_instructions"], "keep me");
        assert_eq!(value["other"]["x"], 1);
    }
}
