//! `sandbox_present`: place one saved UI element in this chat's Artifacts chip, as a card
//! with a title, a short description and the element running in a sandboxed frame.
use super::{elements, only, short, Owner};
use anyhow::{bail, Result};
use serde_json::{json, Value};

/// The checked arguments: `element`, optional `scope` (`mine` by default), `title`,
/// `description`, optional `params` (an object handed to the element) and optional `id`.
pub(super) fn check(args: &Value) -> Result<Value> {
    only(
        args,
        &["element", "scope", "title", "description", "params", "id"],
    )?;
    let element = short(args, "element", 40)?;
    if !elements::name_ok(element) {
        bail!("element must name a saved element");
    }
    let scope = elements::Scope::parse(args["scope"].as_str().unwrap_or("mine"))?;
    let params = match &args["params"] {
        Value::Null => json!({}),
        Value::Object(_) => args["params"].clone(),
        _ => bail!("params must be an object"),
    };
    let id = match args["id"].as_str() {
        None => format!("element-{element}"),
        Some(id) if elements::name_ok(id) => id.to_owned(),
        Some(_) => bail!("id must be 1-40 lowercase letters, digits, - or _"),
    };
    Ok(
        json!({"element":element,"scope":scope.name(),"title":short(args, "title", 80)?,
        "description":short(args, "description", 300)?,"params":params,"id":id}),
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
