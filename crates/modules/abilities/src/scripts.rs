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
/// An abuse cap only; the schema asks for one short sentence.
pub const MAX_DESCRIPTION: usize = 1000;
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
    pub params: Vec<Param>,
}

/// A declared `{{name}}` parameter. A plain string is accepted as a name alone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "ParamInput")]
pub struct Param {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ParamInput {
    Name(String),
    Full(ParamFields),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ParamFields {
    name: String,
    #[serde(default)]
    description: String,
}

impl From<ParamInput> for Param {
    fn from(input: ParamInput) -> Self {
        match input {
            ParamInput::Name(name) => Self {
                name,
                description: String::new(),
            },
            ParamInput::Full(ParamFields { name, description }) => Self { name, description },
        }
    }
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
        if self.description.trim().is_empty() || self.description.chars().count() > MAX_DESCRIPTION
        {
            bail!("description must be one short sentence (at most {MAX_DESCRIPTION} characters)");
        }
        if self.params.len() > MAX_PARAMS
            || self.params.iter().any(|p| !identifier(&p.name, 32, false))
            || (1..self.params.len()).any(|i| self.declares(&self.params[i].name, i))
        {
            bail!("params must be up to {MAX_PARAMS} distinct names of letters, digits or '_'");
        }
        if self
            .params
            .iter()
            .any(|p| p.description.chars().count() > MAX_DESCRIPTION)
        {
            bail!("a param description must be short (at most {MAX_DESCRIPTION} characters)");
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

    /// Whether one of the first `before` params is named `name`.
    fn declares(&self, name: &str, before: usize) -> bool {
        self.params[..before].iter().any(|p| p.name == name)
    }

    /// The steps with `{{param}}` replaced by `args` inside string values only. Keys,
    /// tool names and the step list never change, so arguments cannot add or pick tools.
    ///
    /// # Errors
    /// A missing or undeclared argument, or a value that is not a short string.
    pub fn bind(&self, args: &Map<String, Value>) -> Result<Vec<Step>> {
        let mut values = BTreeMap::new();
        for (key, value) in args {
            if !self.declares(key, self.params.len()) {
                bail!("unknown argument {key}");
            }
            match value.as_str() {
                Some(text) if text.chars().count() <= MAX_VALUE => {
                    values.insert(key.as_str(), text)
                }
                _ => bail!("argument {key} must be a string of up to {MAX_VALUE} characters"),
            };
        }
        if let Some(missing) = self
            .params
            .iter()
            .find(|p| !values.contains_key(p.name.as_str()))
        {
            bail!("missing argument {}", missing.name);
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

    #[test]
    fn description_limit_counts_characters_like_the_schema() {
        let mut s = script(json!([{"tool":"get_time"}]), json!([]));
        s.description = "\u{b7}".repeat(MAX_DESCRIPTION);
        s.validate(20).unwrap();
        s.description.push('x');
        let error = s.validate(20).unwrap_err().to_string();
        assert!(error.contains("1000 characters"), "{error}");
    }

    #[test]
    fn params_take_names_or_described_objects() {
        let s = script(
            json!([{"tool":"get_time"}]),
            json!(["a", {"name":"b","description":"The time zone"}]),
        );
        s.validate(20).unwrap();
        assert_eq!(s.params[0].description, "");
        assert_eq!(s.params[1].description, "The time zone");
        assert!(s
            .bind(json!({"a":"1","b":"2"}).as_object().unwrap())
            .is_ok());
        let mut long = s.clone();
        long.params[1].description = "x".repeat(MAX_DESCRIPTION + 1);
        assert!(long.validate(20).is_err());
        assert!(serde_json::from_value::<Param>(json!({"name":"a","x":1})).is_err());
        let duplicate = script(json!([{"tool":"get_time"}]), json!(["a", {"name":"a"}]));
        assert!(duplicate.validate(20).is_err());
    }
}
