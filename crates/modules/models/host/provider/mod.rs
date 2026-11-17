//! Root composition aliases; Models owns transport and Engine owns prepared context.
pub use webagent_engine::model_client::*;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Registered by name in the broker's focused host checks (build-service/executor.py).
    fn instructions_for_turn_strips_leading_blank_lines() {
        assert_eq!(
            Provider::instructions_for_turn("\n\nYou are a tester."),
            "You are a tester."
        );
    }
}
