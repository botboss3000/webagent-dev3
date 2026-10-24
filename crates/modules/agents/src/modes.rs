//! Agent-owned named mode definitions; Engine retains permission enforcement.
use anyhow::{bail, Result};
use serde_json::{json, Value};
pub fn defaults() -> Value {
    json!([
     {"id":"ask","label":"Ask","description":"Investigate, answer, and recommend without making changes.","permission_policy":"read_only","prompt":"Investigate with read-only tools and deliver an answer or proposal. Do not execute changes.","builtin":true},
     {"id":"plan","label":"Plan","description":"Investigate deeply and deliver an execution-ready plan.","permission_policy":"read_only","prompt":"Research with read-only tools and deliver an execution-ready plan. Maintain Plan Overview and canonical Task Progress; implementation stays pending. Do not execute changes.","builtin":true},
     {"id":"auto","label":"Auto","description":"Work autonomously with write-capable permissions; tool calls do not ask for approval.","permission_policy":"write","prompt":"Execute the requested work autonomously. Denied tools and access limits still apply; tool calls that would otherwise ask for approval proceed automatically. Verify results before claiming completion.","builtin":true}
    ])
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
        let policy = entry["permission_policy"].as_str().unwrap_or("read_only");
        if label.is_empty()
            || label.chars().count() > 40
            || description.chars().count() > 240
            || prompt.chars().count() > 30000
            || !matches!(policy, "read_only" | "write")
        {
            bail!("Invalid mode fields");
        }
        let builtin = matches!(id, "ask" | "plan" | "auto");
        let value = json!({"id":id,"label":label,"description":description,"prompt":prompt,"permission_policy":policy,"builtin":builtin});
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
