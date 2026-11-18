//! `sandbox_share` (operators only, by its `role_access`): share one of the operator's saved
//! UI elements with every user of this agent, or stop sharing it.
use super::{elements, only, short, Owner};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};

/// The checked arguments: `name` and `shared` (true to share, false to stop).
pub(super) fn check(args: &Value) -> Result<Value> {
    only(args, &["name", "shared"])?;
    let shared = args["shared"]
        .as_bool()
        .ok_or_else(|| anyhow!("shared must be true or false"))?;
    Ok(json!({"name":short(args, "name", 40)?,"shared":shared}))
}

pub(super) fn run(
    owner: &Owner,
    request: &Value,
    config: &elements::Config,
    now: u64,
) -> Result<Value> {
    let name = request["name"].as_str().unwrap_or_default();
    let shared = request["shared"] == true;
    let output = elements::share(
        &owner.root,
        &owner.agent,
        &owner.user,
        name,
        shared,
        config,
        now,
    )?;
    Ok(json!({"status":"success","output":output}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_an_explicit_choice() {
        assert!(check(&json!({"name":"toggle"})).is_err());
        assert_eq!(
            check(&json!({"name":"toggle","shared":false})).expect("valid")["shared"],
            false
        );
    }
}
