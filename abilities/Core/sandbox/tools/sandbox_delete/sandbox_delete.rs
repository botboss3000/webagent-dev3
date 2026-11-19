//! `sandbox_delete`: delete one of the user's saved UI elements with this agent. Shared
//! elements are unshared with `sandbox_share`. Needs approval in Ask mode.
use super::{elements, short, Owner};
use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteArgs {
    name: String,
}

/// The checked arguments: `name`.
pub(super) fn check(args: &Value) -> Result<Value> {
    let delete: DeleteArgs = webagent_abilities::tool_arguments::read("sandbox_delete", args)?;
    Ok(json!({"name":short("name", &delete.name, 40)?}))
}

pub(super) fn run(owner: &Owner, request: &Value) -> Result<Value> {
    let name = request["name"].as_str().unwrap_or_default();
    elements::delete(
        &owner.root,
        &owner.agent,
        &owner.user,
        elements::Scope::Mine,
        name,
    )?;
    Ok(json!({"status":"success","output":format!("Deleted {name}")}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_only_a_name() {
        assert!(check(&json!({"name":"toggle"})).is_ok());
        assert!(check(&json!({"name":"toggle","scope":"shared"})).is_err());
    }
}
