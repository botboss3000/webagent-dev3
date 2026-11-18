//! `sandbox_propose`: propose a sandbox folder's `index.html` as a saved UI element of
//! this agent. Nothing is saved until the user approves it on the agent's Artifacts page.
use super::{elements, only, short, Owner};
use anyhow::{bail, Result};
use serde_json::{json, Value};

/// The checked arguments: `folder` (in the sandbox), `name`, `title`, `description`.
pub(super) fn check(args: &Value) -> Result<Value> {
    only(args, &["folder", "name", "title", "description"])?;
    let folder = short(args, "folder", 200)?;
    let name = short(args, "name", 40)?;
    if !elements::name_ok(name) {
        bail!("name must be 1-40 lowercase letters, digits, - or _");
    }
    Ok(
        json!({"folder":folder,"name":name,"title":short(args, "title", 80)?,
        "description":args["description"].as_str().unwrap_or("")}),
    )
}

/// Records the proposal for `html`, the folder's `index.html`.
pub(super) fn run(
    owner: &Owner,
    sid: &str,
    request: &Value,
    html: &str,
    config: &elements::Config,
    now: u64,
) -> Result<Value> {
    let draft = elements::Draft {
        name: request["name"].as_str().unwrap_or_default(),
        title: request["title"].as_str().unwrap_or_default(),
        description: request["description"].as_str().unwrap_or_default(),
        html,
    };
    let id = elements::propose(
        &owner.root,
        &owner.agent,
        &owner.user,
        sid,
        &draft,
        config,
        now,
    )?;
    Ok(json!({"status":"success","output":format!(
        "Proposed \"{}\" ({} bytes). It is saved only once the user approves it on this agent's Artifacts page; tell them it is waiting.",
        draft.title, html.len()),
        "proposal_id":id,
        "review":format!("/app?tab=agents&agent={}&agentView=artifacts", owner.agent)}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_unknown_arguments_are_refused() {
        assert!(check(&json!({"folder":"toggle","name":"Toggle","title":"t"})).is_err());
        assert!(
            check(&json!({"folder":"toggle","name":"toggle","title":"t","scope":"shared"}))
                .is_err()
        );
        let ok = check(&json!({"folder":"toggle","name":"toggle","title":"t"})).expect("valid");
        assert_eq!(ok["description"], "");
    }
}
