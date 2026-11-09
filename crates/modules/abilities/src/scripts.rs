//! Session scripts: a saved, named sequence of tool calls for one conversation tree.
//!
//! Pure rules only: the shape a script may take, which tools a step may name, and
//! `{{param}}` substitution. Storage, tree resolution, live policy and dispatch are in the
//! root adapter (`host/base/scripts_adapter.rs`). A script never grants permission: every
//! step is checked against live policy when it is saved and again when it runs.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// The session-script tools; none of them may be a script step.
pub const TOOLS: &[&str] = &["script_save", "script_list", "script_run"];
/// Write tools a phase-1 script may still use; every other step must be a read.
pub const BROWSER_WRITES: &[&str] = &["browser_input"];
const MAX_NAME: usize = 64;
const MAX_DESCRIPTION: usize = 200;
const MAX_PARAMS: usize = 10;
const MAX_VALUE: usize = 2000;
const MAX_ARGUMENTS_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub tool: String,
    #[serde(default = "empty_object")]
    pub arguments: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    pub name: String,
    pub description: String,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub params: Vec<String>,
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

fn identifier(text: &str, max: usize, dash: bool) -> bool {
    !text.is_empty()
        && text.len() <= max
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || (dash && c == '-'))
}

/// Whether `tool` may be a step: never a script tool; otherwise a read, or a Browser
/// Control write. Live policy is checked separately by the caller.
#[must_use]
pub fn step_kind_allowed(tool: &str, is_read: bool) -> bool {
    !TOOLS.contains(&tool) && (is_read || BROWSER_WRITES.contains(&tool))
}

impl Script {
    /// Checks the script's shape and step count; tool permissions are checked by the caller.
    ///
    /// # Errors
    /// A bad name, description, parameter, step count or argument object.
    pub fn validate(&self, max_steps: usize) -> Result<()> {
        if !identifier(&self.name, MAX_NAME, true) {
            bail!("name must be 1-{MAX_NAME} letters, digits, '_' or '-'");
        }
        if self.description.trim().is_empty() || self.description.len() > MAX_DESCRIPTION {
            bail!("description must be 1-{MAX_DESCRIPTION} bytes");
        }
        if self.params.len() > MAX_PARAMS
            || self.params.iter().any(|p| !identifier(p, 32, false))
            || (1..self.params.len()).any(|i| self.params[..i].contains(&self.params[i]))
        {
            bail!("params must be up to {MAX_PARAMS} distinct names of letters, digits or '_'");
        }
        if self.steps.is_empty() || self.steps.len() > max_steps {
            bail!("a script has 1-{max_steps} steps");
        }
        for (index, step) in self.steps.iter().enumerate() {
            if !step.arguments.is_object()
                || serde_json::to_vec(&step.arguments)?.len() > MAX_ARGUMENTS_BYTES
            {
                bail!(
                    "step {} arguments must be an object under 16 KiB",
                    index + 1
                );
            }
        }
        Ok(())
    }

    /// The steps with `{{param}}` replaced by `args` inside string values only. Keys,
    /// tool names and the step list never change, so arguments cannot add or pick tools.
    ///
    /// # Errors
    /// A missing or undeclared argument, or a value that is not a short string.
    pub fn bind(&self, args: &Map<String, Value>) -> Result<Vec<Step>> {
        let mut values = BTreeMap::new();
        for (key, value) in args {
            if !self.params.contains(key) {
                bail!("unknown argument {key}");
            }
            match value.as_str() {
                Some(text) if text.len() <= MAX_VALUE => values.insert(key.as_str(), text),
                _ => bail!("argument {key} must be a string under {MAX_VALUE} bytes"),
            };
        }
        if let Some(missing) = self
            .params
            .iter()
            .find(|p| !values.contains_key(p.as_str()))
        {
            bail!("missing argument {missing}");
        }
        Ok(self
            .steps
            .iter()
            .map(|step| Step {
                tool: step.tool.clone(),
                arguments: substitute(&step.arguments, &values),
            })
            .collect())
    }
}

fn substitute(value: &Value, values: &BTreeMap<&str, &str>) -> Value {
    match value {
        Value::String(text) => {
            let mut out = text.clone();
            for (name, replacement) in values {
                out = out.replace(&format!("{{{{{name}}}}}"), replacement);
            }
            Value::String(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(|v| substitute(v, values)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), substitute(v, values)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn script(steps: Value, params: Value) -> Script {
        serde_json::from_value(json!({"name":"capture","description":"Take shots",
            "steps":steps,"params":params}))
        .unwrap()
    }

    #[test]
    fn substitution_stays_inside_string_values() {
        let s = script(
            json!([{"tool":"browser_read","arguments":{"action":"navigate","url":"{{url}}",
                "{{url}}":"key stays","n":3}}]),
            json!(["url"]),
        );
        s.validate(20).unwrap();
        let injected = r#"x"},{"tool":"file_write","arguments":{"path":"/etc"}}"#;
        let bound = s
            .bind(json!({"url": injected}).as_object().unwrap())
            .unwrap();
        assert_eq!(bound.len(), 1);
        assert_eq!(bound[0].tool, "browser_read");
        assert_eq!(bound[0].arguments["url"], injected);
        assert_eq!(bound[0].arguments["{{url}}"], "key stays");
        assert_eq!(bound[0].arguments["n"], 3);
    }

    #[test]
    fn arguments_must_match_declared_params() {
        let s = script(json!([{"tool":"get_time"}]), json!(["a"]));
        assert!(s.bind(json!({}).as_object().unwrap()).is_err());
        assert!(s
            .bind(json!({"a":"1","b":"2"}).as_object().unwrap())
            .is_err());
        assert!(s.bind(json!({"a":1}).as_object().unwrap()).is_err());
        assert!(s.bind(json!({"a":"1"}).as_object().unwrap()).is_ok());
    }

    #[test]
    fn shape_and_step_kinds() {
        let many = Value::Array(vec![json!({"tool":"get_time"}); 21]);
        assert!(script(many, json!([])).validate(20).is_err());
        assert!(script(json!([]), json!([])).validate(20).is_err());
        assert!(
            script(json!([{"tool":"get_time","arguments":[]}]), json!([]))
                .validate(20)
                .is_err()
        );
        assert!(script(json!([{"tool":"get_time"}]), json!(["a", "a"]))
            .validate(20)
            .is_err());
        assert!(step_kind_allowed("browser_input", false));
        assert!(step_kind_allowed("get_time", true));
        assert!(!step_kind_allowed("file_write", false));
        assert!(!step_kind_allowed("script_run", true));
    }
}
