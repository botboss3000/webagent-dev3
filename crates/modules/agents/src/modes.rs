//! Agent-owned named mode definitions; Engine retains permission enforcement.
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
/// The built-in modes' live file, in the prompts folder.
const DEFAULTS_FILE: &str = "config/configuration/prompts/execution_modes.json";
/// The same file as built, used only when the live one is missing or invalid.
const PACKAGED_DEFAULTS: &str =
    include_str!("../../../../config/configuration/prompts/execution_modes.json");
const BUILTIN: [&str; 3] = ["ask", "plan", "auto"];
/// The built-in Ask, Plan and Auto modes from the live prompts file.
pub fn defaults() -> Value {
    webagent_configuration::live::parsed(DEFAULTS_FILE, 256 * 1024, parse_defaults)
        .or_else(|_| parse_defaults(PACKAGED_DEFAULTS.as_bytes()))
        .expect("the packaged execution modes are valid")
}
/// Read the modes file: an object keyed by mode id, each with label, description and prompt.
fn parse_defaults(bytes: &[u8]) -> Result<Value> {
    let file: Value = serde_json::from_slice(bytes)?;
    let mut modes = Vec::with_capacity(BUILTIN.len());
    for id in BUILTIN {
        let entry = &file[id];
        let field = |name: &str, limit: usize| {
            entry[name]
                .as_str()
                .filter(|value| !value.trim().is_empty() && value.chars().count() <= limit)
                .ok_or_else(|| anyhow!("Execution mode {id} needs a {name} of 1..{limit}"))
        };
        let turn = entry["turn_guidance"].as_str().unwrap_or("");
        let turn_label = entry["turn_label"].as_str().unwrap_or("");
        if turn.chars().count() > 4000 || turn_label.chars().count() > 60 {
            bail!("Execution mode {id} needs a turn_label up to 60 and turn_guidance up to 4000");
        }
        modes.push(json!({"id":id,"label":field("label",40)?,
            "description":field("description",240)?,"prompt":field("prompt",30000)?,
            "turn_label":turn_label,"turn_guidance":turn,"builtin":true}));
    }
    Ok(json!(modes))
}
pub fn normalize(raw: &Value) -> Result<Value> {
    let mut modes = defaults().as_array().unwrap().clone();
    if raw.is_null() {
        return Ok(json!(modes));
    }
    let entries = raw
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Modes must be an array"))?;
    if entries.len() > 20 {
        bail!("At most 20 modes");
    }
    let mut seen = std::collections::HashSet::new();
    for entry in entries {
        let id = entry["id"].as_str().unwrap_or("");
        if id.is_empty()
            || id.len() > 32
            || !id.as_bytes()[0].is_ascii_lowercase()
            || !id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
            || !seen.insert(id)
        {
            bail!("Invalid or duplicate mode id");
        }
        let label = entry["label"].as_str().unwrap_or(id).trim();
        let description = entry["description"].as_str().unwrap_or("");
        let prompt = entry["prompt"].as_str().unwrap_or("");
        // A saved override without the per-turn fields keeps the built-in ones.
        let inherited = |name: &str| {
            entry[name].as_str().map(str::to_owned).unwrap_or_else(|| {
                modes
                    .iter()
                    .find(|m| m["id"] == id)
                    .and_then(|m| m[name].as_str())
                    .unwrap_or("")
                    .to_owned()
            })
        };
        let turn = inherited("turn_guidance");
        let turn_label = inherited("turn_label");
        if label.is_empty()
            || label.chars().count() > 40
            || description.chars().count() > 240
            || prompt.chars().count() > 30000
            || turn.chars().count() > 4000
            || turn_label.chars().count() > 60
        {
            bail!("Invalid mode fields");
        }
        let builtin = BUILTIN.contains(&id);
        let value = json!({"id":id,"label":label,"description":description,"prompt":prompt,
            "turn_label":turn_label,"turn_guidance":turn,"builtin":builtin});
        if let Some(i) = modes.iter().position(|m| m["id"] == id) {
            modes[i] = value;
        } else {
            modes.push(value);
        }
    }
    if modes.len() > 20 {
        bail!("At most 17 custom modes");
    }
    Ok(json!(modes))
}

/// Resolve definitions from the existing encrypted agent metadata.
pub fn config(metadata: &Value) -> Result<Value> {
    let modes = normalize(&metadata["execution_modes"])?;
    let default = metadata["default_execution_mode"].as_str().unwrap_or("ask");
    if !modes
        .as_array()
        .is_some_and(|entries| entries.iter().any(|m| m["id"] == default))
    {
        bail!("Invalid default mode");
    }
    Ok(json!({"execution_modes":modes,"default_execution_mode":default}))
}

pub fn resolve(config: &Value, id: &str) -> Result<Value> {
    config["execution_modes"]
        .as_array()
        .and_then(|modes| modes.iter().find(|m| m["id"] == id))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Unknown execution mode"))
}

#[cfg(test)]
mod defaults_tests {
    use super::*;
    #[test]
    fn packaged_file_defines_the_builtin_modes_and_bad_files_are_refused() {
        let modes = parse_defaults(PACKAGED_DEFAULTS.as_bytes()).unwrap();
        let ids: Vec<_> = modes
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].clone())
            .collect();
        assert_eq!(ids, [json!("ask"), json!("plan"), json!("auto")]);
        assert!(modes[1]["prompt"]
            .as_str()
            .unwrap()
            .contains("set_execution_mode"));
        assert!(
            parse_defaults(br#"{"ask":{"label":"Ask","description":"d","prompt":"p"}}"#).is_err()
        );
        assert!(parse_defaults(b"[]").is_err());
    }
}
