//! `sandbox_python`: Python code (`python3 -I -c`) in the session's sandbox folder.
use super::{text, timeout};
use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PythonArgs {
    code: String,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

/// The worker request for `args` (`code`, optional `timeout_seconds`).
pub(super) fn request(args: &Value) -> Result<Value> {
    let python: PythonArgs = webagent_abilities::tool_arguments::read("sandbox_python", args)?;
    Ok(
        json!({"code":text("code", &python.code)?,"timeout_seconds":timeout(python.timeout_seconds)?}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_code_and_a_timeout_reach_the_worker() {
        assert_eq!(
            request(&json!({"code":"print(1)","timeout_seconds":5})).expect("valid request"),
            json!({"code":"print(1)","timeout_seconds":5})
        );
        assert!(request(&json!({"command":"ls"})).is_err());
    }
}
