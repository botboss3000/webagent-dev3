//! `sandbox_shell`: one `/bin/sh -c` command in the session's sandbox folder.
use super::{only, text, timeout};
use anyhow::Result;
use serde_json::{json, Value};

/// The worker request for `args` (`command`, optional `timeout_seconds`).
pub(super) fn request(args: &Value) -> Result<Value> {
    only(args, &["command", "timeout_seconds"])?;
    Ok(json!({"command":text(args, "command")?,"timeout_seconds":timeout(args)?}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_command_and_a_timeout_reach_the_worker() {
        assert_eq!(
            request(&json!({"command":"ls"})).expect("valid request"),
            json!({"command":"ls","timeout_seconds":null})
        );
        assert!(request(&json!({"command":""})).is_err());
        assert!(request(&json!({"command":"ls","timeout_seconds":0})).is_err());
        assert!(request(&json!({"command":"ls","workspace":"other"})).is_err());
    }
}
