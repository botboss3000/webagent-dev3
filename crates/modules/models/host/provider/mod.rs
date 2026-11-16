//! Root composition aliases; Models owns transport and Engine owns prepared context.
pub use webagent_engine::model_client::*;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Registered by name in the broker's focused host checks, so the name stays. There are
    // no base prompt files any more: instructions are the agent's prompt slots (the engine's
    // instructions tests cover stripping old compiled seeds).
    fn configured_base_instructions_loads_and_recovers_from_file_changes() {
        assert_eq!(
            Provider::instructions_for_turn("\n\nYou are a tester."),
            "You are a tester."
        );
    }
}
