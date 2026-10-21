//! Root composition aliases; Models owns transport and Engine owns prepared context.
pub use webagent_engine::model_client::*;
#[cfg(test)]
mod tests {use super::*;
    use serde_json::json;
    #[test]
    fn configured_base_instructions_loads_and_recovers_from_file_changes() {
        let path = "config/prompts.json";
        let original = std::fs::read(path).expect("read original prompts file");
        let original_metadata = std::fs::metadata(path).expect("read prompt metadata");
        let original_permissions = original_metadata.permissions();
        let original_modified = original_metadata.modified().expect("read prompt modification time");
        struct RestorePrompts(Vec<u8>, std::fs::Permissions, std::time::SystemTime);
        impl Drop for RestorePrompts {
            /// Restore the original prompt fixture even if the prompt-cache assertion unwinds.
            fn drop(&mut self) {
                let _ = std::fs::write("config/prompts.json", &self.0);
                let _ = std::fs::set_permissions("config/prompts.json", self.1.clone());
                // This compile input must keep its original freshness after the fixture.
                if let Ok(file) = std::fs::File::options().write(true).open("config/prompts.json") {
                    let _ = file.set_times(std::fs::FileTimes::new().set_modified(self.2));
                }
            }
        }
        let restore = RestorePrompts(original.clone(), original_permissions.clone(), original_modified);
        let good = json!({
            "base_instructions": "BASE_MARKER",
            "closing_reporting": "CLOSING_MARKER",
            "jev_guidance": "JEV_MARKER"
        });
        std::fs::write(path, good.to_string()).expect("write valid prompts file");
        let last_good = Provider::configured_base_instructions();
        assert!(last_good.contains("BASE_MARKER"));
        assert!(last_good.contains("CLOSING_MARKER"));
        assert!(last_good.contains("JEV_MARKER"));

        std::fs::write(path, "{ malformed json").expect("write malformed prompts file");
        assert_eq!(Provider::configured_base_instructions(), last_good);
        std::fs::remove_file(path).expect("remove prompts file");
        assert_eq!(Provider::configured_base_instructions(), last_good);

        std::fs::write(path, &original).expect("restore original prompts file");
        assert_eq!(std::fs::read(path).expect("re-read restored prompts file"), original);
        drop(restore);
        Provider::configured_base_instructions();
        assert_eq!(std::fs::read(path).expect("verify restored prompts file"), original);
        assert_eq!(std::fs::metadata(path).expect("verify restored prompt permissions").permissions(), original_permissions);
        assert_eq!(std::fs::metadata(path).expect("verify restored prompt timestamp").modified().unwrap(), original_modified);
    }
}
