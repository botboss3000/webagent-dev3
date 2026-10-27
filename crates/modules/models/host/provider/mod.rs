//! Root composition aliases; Models owns transport and Engine owns prepared context.
pub use webagent_engine::model_client::*;
#[cfg(test)]
mod tests {use super::*;
    #[test]
    // Registered by name in the broker's focused host checks; last-good recovery is covered by webagent_configuration::live.
    fn configured_base_instructions_loads_and_recovers_from_file_changes() {
        let composed = Provider::configured_base_instructions();
        for name in ["base_instructions", "closing_reporting", "jev_guidance"] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(webagent_configuration::prompts::PROMPTS).join(format!("{name}.md"));
            let text = std::fs::read_to_string(path).unwrap();
            assert!(composed.contains(text.trim()), "{name}");
        }
    }
}
