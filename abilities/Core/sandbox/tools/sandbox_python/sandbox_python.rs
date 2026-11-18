//! `sandbox_python`: Python code (`python3 -I -c`) in the session's sandbox folder.
use super::{only, text, timeout};
use anyhow::Result;
use serde_json::{json, Value};

/// The worker request for `args` (`code`, optional `timeout_seconds`).
pub(super) fn request(args: &Value) -> Result<Value> {
    only(args, &["code", "timeout_seconds"])?;
    Ok(json!({"code":text(args, "code")?,"timeout_seconds":timeout(args)?}))
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
