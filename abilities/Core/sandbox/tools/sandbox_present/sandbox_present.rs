//! `sandbox_present`: place one saved UI element in this chat's Artifacts chip, as a card
//! with a title, a short description and the element running in a sandboxed frame.
use super::{elements, short, Owner};
use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::{json, Map, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PresentArgs {
    element: String,
    title: String,
    description: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    params: Option<Map<String, Value>>,
    #[serde(default)]
    id: Option<String>,
}

/// The checked arguments: `element`, optional `scope` (`mine` by default), `title`,
/// `description`, optional `params` (an object handed to the element) and optional `id`.
pub(super) fn check(args: &Value) -> Result<Value> {
    let present: PresentArgs = webagent_abilities::tool_arguments::read("sandbox_present", args)?;
    let element = short("element", &present.element, 40)?;
    if !elements::name_ok(element) {
        bail!("element must name a saved element");
    }
    let scope = elements::Scope::parse(present.scope.as_deref().unwrap_or("mine"))?;
    let params = Value::Object(present.params.unwrap_or_default());
    let id = match present.id.as_deref() {
        None => format!("element-{element}"),
        Some(id) if elements::name_ok(id) => id.to_owned(),
        Some(_) => bail!("id must be 1-40 lowercase letters, digits, - or _"),
    };
    Ok(
        json!({"element":element,"scope":scope.name(),"title":short("title", &present.title, 80)?,
        "description":short("description", &present.description, 300)?,"params":params,"id":id}),
    )
}

/// The session artifact for the request: its id and body. The element must exist.
pub(super) fn artifact(
    owner: &Owner,
    request: &Value,
    config: &elements::Config,
) -> Result<(String, Value)> {
    let name = request["element"].as_str().unwrap_or_default();
    let scope = elements::Scope::parse(request["scope"].as_str().unwrap_or("mine"))?;
    elements::read(&owner.root, &owner.agent, &owner.user, scope, name)?;
    if request["params"].to_string().len() as u64 > config.max_params_kb * 1024 {
        bail!("params must be at most {} KB", config.max_params_kb);
    }
    let title = request["title"].as_str().unwrap_or_default().trim();
    if title.is_empty() {
        bail!("title must not be empty");
    }
    let body = json!({"title":title,
        "sections":[{"label":"Change","value":request["description"]}],
        "element":{"name":name,"scope":scope.name()},"params":request["params"]});
    Ok((request["id"].as_str().unwrap_or_default().to_owned(), body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_shapes() {
        let ok = check(&json!({"element":"toggle","title":"Option A","description":"Step rail"}))
            .expect("valid");
        assert_eq!(
            (ok["scope"].as_str(), ok["id"].as_str()),
            (Some("mine"), Some("element-toggle"))
        );
        assert_eq!(ok["params"], json!({}));
        assert!(
            check(&json!({"element":"toggle","title":"t","description":"d","params":[1]})).is_err()
        );
        assert!(
            check(&json!({"element":"toggle","title":"t","description":"d","scope":"all"}))
                .is_err()
        );
    }
}
