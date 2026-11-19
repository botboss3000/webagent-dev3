//! `sandbox_shell`: one `/bin/sh -c` command in the session's sandbox folder.
use super::{text, timeout};
use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ShellArgs {
    command: String,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

/// The worker request for `args` (`command`, optional `timeout_seconds`).
pub(super) fn request(args: &Value) -> Result<Value> {
    let shell: ShellArgs = webagent_abilities::tool_arguments::read("sandbox_shell", args)?;
    Ok(
        json!({"command":text("command", &shell.command)?,"timeout_seconds":timeout(shell.timeout_seconds)?}),
    )
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
