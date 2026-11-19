//! `sandbox_save`: save a sandbox folder's `index.html` as one of this agent's UI elements
//! for the user, kept across all their conversations with the agent. The tool needs
//! approval, so it follows the session's execution mode: in Ask the user approves it in the
//! chat, in Auto it runs.
use super::{elements, short, Owner};
use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SaveArgs {
    folder: String,
    name: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
}

/// The checked arguments: `folder` (in the sandbox), `name`, `title`, `description`.
pub(super) fn check(args: &Value) -> Result<Value> {
    let save: SaveArgs = webagent_abilities::tool_arguments::read("sandbox_save", args)?;
    let folder = short("folder", &save.folder, 200)?;
    let name = short("name", &save.name, 40)?;
    if !elements::name_ok(name) {
        bail!("name must be 1-40 lowercase letters, digits, - or _");
    }
    Ok(
        json!({"folder":folder,"name":name,"title":short("title", &save.title, 80)?,
        "description":save.description.as_deref().unwrap_or("")}),
    )
}

/// Saves `html`, the folder's `index.html`.
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
    let saved = elements::save(
        &owner.root,
        &owner.agent,
        &owner.user,
        sid,
        &draft,
        config,
        now,
    )?;
    Ok(json!({"status":"success","output":format!(
        "Saved \"{}\" (revision {}, {} bytes). Place it in a chat with sandbox_present.",
        draft.title, saved["revision"], html.len()),"element":saved}))
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
