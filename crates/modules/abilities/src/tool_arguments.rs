//! The shared reader Rust tools use for their arguments, and the registry that proves each
//! registered argument type accepts what its `<tool>.tool.json` allows.
//!
//! The engine checks every call against the tool's live schema before the tool runs, so
//! limits, allowed values and patterns live only in tool.json. A tool reads the checked
//! arguments into its type with [`read`] and keeps only the rules a schema cannot express.
//! Each implementing crate registers `(tool, type)` with [`argument_type!`]; its test runs
//! [`registry_problems`], which decodes calls built from the live schemas.
use crate::tool_definitions;
use anyhow::{anyhow, Result};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{Map, Value};

/// Reads checked arguments into `T`, naming the tool in a failure.
pub fn read<T: DeserializeOwned>(tool: &str, args: &Value) -> Result<T> {
    T::deserialize(args).map_err(|error| anyhow!("{tool}: invalid arguments: {error}"))
}

/// One tool's argument type, registered where the tool is implemented.
pub struct ArgumentType {
    pub tool: &'static str,
    pub decode: fn(&Value) -> Result<()>,
}

/// Decodes `args` into `T` and drops it; the registry's view of a type.
pub fn decode<T: DeserializeOwned>(args: &Value) -> Result<()> {
    T::deserialize(args)
        .map(drop)
        .map_err(|error| anyhow!("{error}"))
}

/// `argument_type!("calculate", Arithmetic)` registers `Arithmetic` as calculate's arguments.
#[macro_export]
macro_rules! argument_type {
    ($tool:literal, $type:ty) => {
        $crate::tool_arguments::ArgumentType {
            tool: $tool,
            decode: $crate::tool_arguments::decode::<$type>,
        }
    };
}

/// Every way a registered type disagrees with its tool's current definition: no definition,
/// a registered twice, a sample call it rejects, or an unknown argument it accepts although
/// the schema is closed (a type must refuse what the schema refuses, so an argument added to
/// the schema alone fails here).
pub fn registry_problems<'a>(types: impl IntoIterator<Item = &'a ArgumentType>) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for registered in types {
        if !seen.insert(registered.tool) {
            problems.push(format!("{}: registered twice", registered.tool));
        }
        let Some(definition) = tool_definitions::get(registered.tool) else {
            problems.push(format!("{}: no tool definition", registered.tool));
            continue;
        };
        let schema = &definition.parameters;
        for (kind, sample) in [
            ("required-only", sample(schema, false)),
            ("full", sample(schema, true)),
        ] {
            if let Err(error) = (registered.decode)(&sample) {
                problems.push(format!(
                    "{}: {kind} sample {sample} rejected: {error}",
                    registered.tool
                ));
            }
        }
        if schema["additionalProperties"] == false {
            let mut probe = sample(schema, true);
            if let Some(object) = probe.as_object_mut() {
                object.insert("unregistered_probe".into(), Value::Bool(true));
            }
            if (registered.decode)(&probe).is_ok() {
                problems.push(format!(
                    "{}: accepts unknown arguments; add #[serde(deny_unknown_fields)]",
                    registered.tool
                ));
            }
        }
    }
    problems
}

/// A value `schema` allows: with `all`, every declared property and at least one array
/// item; otherwise only what is required. Uses the first allowed value or form, the
/// minimum length, count or bound, and a fixed UUID for `format: uuid`.
pub fn sample(schema: &Value, all: bool) -> Value {
    if let Some(value) = schema["enum"].as_array().and_then(|values| values.first()) {
        return value.clone();
    }
    if let Some(first) = schema["anyOf"]
        .as_array()
        .and_then(|options| options.first())
    {
        return sample(first, all);
    }
    let kind = match &schema["type"] {
        Value::String(kind) => kind.as_str(),
        Value::Array(kinds) => kinds
            .iter()
            .filter_map(Value::as_str)
            .find(|kind| *kind != "null")
            .unwrap_or("null"),
        _ if schema["properties"].is_object() => "object",
        _ => "null",
    };
    match kind {
        "object" => {
            let required: Vec<&str> = schema["required"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let mut object = Map::new();
            for (key, property) in schema["properties"].as_object().into_iter().flatten() {
                if all || required.contains(&key.as_str()) {
                    object.insert(key.clone(), sample(property, all));
                }
            }
            Value::Object(object)
        }
        "array" => {
            let least = schema["minItems"].as_u64().unwrap_or(0);
            let count = if all { least.max(1) } else { least };
            Value::Array((0..count).map(|_| sample(&schema["items"], all)).collect())
        }
        "string" if schema["format"] == "uuid" => "00000000-0000-4000-8000-000000000000".into(),
        "string" => "a"
            .repeat(schema["minLength"].as_u64().unwrap_or(1).max(1) as usize)
            .into(),
        "integer" => schema["minimum"]
            .as_f64()
            .map_or(1, |minimum| minimum.ceil() as i64)
            .into(),
        "number" => schema["minimum"].as_f64().unwrap_or(1.0).into(),
        "boolean" => true.into(),
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    #[serde(deny_unknown_fields)]
    struct Probe {
        name: String,
        count: Option<u32>,
    }

    #[test]
    fn samples_follow_the_schema() {
        let schema = json!({"type":"object","required":["mode","ids"],"properties":{
            "mode":{"type":"string","enum":["fast","slow"]},
            "ids":{"type":"array","minItems":2,"items":{"type":"string","format":"uuid"}},
            "limit":{"type":"integer","minimum":3},
            "label":{"type":["string","null"],"minLength":2},
            "pick":{"anyOf":[{"type":"boolean"},{"type":"string"}]}}});
        let uuid = "00000000-0000-4000-8000-000000000000";
        assert_eq!(
            sample(&schema, false),
            json!({"mode":"fast","ids":[uuid, uuid]})
        );
        assert_eq!(
            sample(&schema, true),
            json!({"mode":"fast","ids":[uuid, uuid],"limit":3,"label":"aa","pick":true})
        );
    }

    #[test]
    fn read_names_the_tool() {
        let error = read::<Probe>("t", &json!({"count":1}))
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("t: invalid arguments: missing field `name`"),
            "{error}"
        );
        assert!(decode::<Probe>(&json!({"name":"a","extra":1})).is_err());
    }
}
