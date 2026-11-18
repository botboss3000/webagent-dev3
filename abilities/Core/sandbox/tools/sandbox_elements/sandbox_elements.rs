//! `sandbox_elements`: this agent's saved UI elements for the user and the shared ones,
//! without their HTML (read it from `saved/` in the sandbox).
use super::{elements, only, Owner};
use anyhow::Result;
use serde_json::{json, Value};

/// No arguments.
pub(super) fn check(args: &Value) -> Result<Value> {
    if !args.is_null() {
        only(args, &[])?;
    }
    Ok(json!({}))
}

pub(super) fn run(owner: &Owner) -> Result<Value> {
    let list = elements::list(&owner.root, &owner.agent, &owner.user)?;
    Ok(json!({"status":"success","output":list,
        "note":"Saved elements are read-only in the sandbox at saved/mine/<name>/index.html and saved/shared/<name>/index.html."}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_no_arguments() {
        assert!(check(&json!({})).is_ok());
        assert!(check(&json!({"user":"someone"})).is_err());
    }
}
