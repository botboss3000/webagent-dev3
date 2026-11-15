//! Checks a tool call's arguments against the tool's `parameters` schema before anything
//! else sees them, so a bad call returns a precise error instead of reaching the tool or
//! asking the user to approve it. Covers the JSON-schema subset tool definitions use:
//! `type`, `properties`, `required`, `additionalProperties`, `items`, `enum`,
//! `minimum`/`maximum`, `minLength`/`maxLength` and `minItems`/`maxItems`.
use serde_json::Value;

/// Reserved arguments the runtime strips before validation and dispatch.
const RESERVED: [&str; 2] = ["_execution_mode", "_repeat_of"];

/// `Err` holds one message naming the tool, the argument and what was expected, plus the
/// tool's usage hint when it has one.
pub fn check(name: &str, schema: &Value, args: &Value, hint: Option<&str>) -> Result<(), String> {
    let Some(object) = args.as_object() else {
        return Err(finish(name, "arguments must be a JSON object", hint));
    };
    let mut args = object.clone();
    for key in RESERVED {
        args.remove(key);
    }
    value(schema, &Value::Object(args), "").map_err(|problem| finish(name, &problem, hint))
}

fn finish(name: &str, problem: &str, hint: Option<&str>) -> String {
    match hint {
        Some(hint) => format!("{name}: {problem}. {hint}"),
        None => format!("{name}: {problem}"),
    }
}

fn label(path: &str) -> String {
    if path.is_empty() {
        "arguments".to_owned()
    } else {
        format!("argument '{path}'")
    }
}

fn value(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    let kinds: Vec<&str> = match &schema["type"] {
        Value::String(kind) => vec![kind.as_str()],
        Value::Array(kinds) => kinds.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if !kinds.is_empty() && !kinds.iter().any(|kind| is_kind(kind, value)) {
        return Err(format!("{} must be {}", label(path), kinds.join(" or ")));
    }
    if let Some(options) = schema["enum"].as_array() {
        if !options.contains(value) {
            let allowed = options
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!("{} must be one of {allowed}", label(path)));
        }
    }
    if let Some(number) = value.as_f64() {
        if schema["minimum"].as_f64().is_some_and(|min| number < min) {
            return Err(format!(
                "{} must be at least {}",
                label(path),
                schema["minimum"]
            ));
        }
        if schema["maximum"].as_f64().is_some_and(|max| number > max) {
            return Err(format!(
                "{} must be at most {}",
                label(path),
                schema["maximum"]
            ));
        }
    }
    if let Some(text) = value.as_str() {
        let length = text.chars().count() as u64;
        if schema["minLength"].as_u64().is_some_and(|min| length < min) {
            return Err(format!(
                "{} is shorter than {}",
                label(path),
                schema["minLength"]
            ));
        }
        if schema["maxLength"].as_u64().is_some_and(|max| length > max) {
            return Err(format!(
                "{} is longer than {}",
                label(path),
                schema["maxLength"]
            ));
        }
    }
    if let Some(items) = value.as_array() {
        let count = items.len() as u64;
        if schema["minItems"].as_u64().is_some_and(|min| count < min) {
            return Err(format!(
                "{} needs at least {} items",
                label(path),
                schema["minItems"]
            ));
        }
        if schema["maxItems"].as_u64().is_some_and(|max| count > max) {
            return Err(format!(
                "{} allows at most {} items",
                label(path),
                schema["maxItems"]
            ));
        }
        if schema["items"].is_object() {
            for (index, item) in items.iter().enumerate() {
                self::value(&schema["items"], item, &format!("{path}[{index}]"))?;
            }
        }
    }
    if let Some(object) = value.as_object() {
        let properties = schema["properties"].as_object();
        for key in schema["required"].as_array().into_iter().flatten() {
            if let Some(key) = key.as_str() {
                if !object.contains_key(key) {
                    return Err(format!("missing required {}", label(&join(path, key))));
                }
            }
        }
        let required = |key: &str| {
            schema["required"]
                .as_array()
                .is_some_and(|required| required.iter().any(|item| item == key))
        };
        for (key, item) in object {
            match properties.and_then(|properties| properties.get(key)) {
                // Models often send null for an optional argument; it means "not given".
                Some(_) if item.is_null() && !required(key) => {}
                Some(property) => self::value(property, item, &join(path, key))?,
                None if schema["additionalProperties"] == false => {
                    let expected = properties
                        .map(|properties| properties.keys().cloned().collect::<Vec<_>>().join(", "))
                        .unwrap_or_default();
                    return Err(if expected.is_empty() {
                        format!(
                            "unknown {}; this tool takes no arguments",
                            label(&join(path, key))
                        )
                    } else {
                        format!(
                            "unknown {}; expected one of {expected}",
                            label(&join(path, key))
                        )
                    });
                }
                None => {}
            }
        }
    }
    Ok(())
}

fn is_kind(kind: &str, value: &Value) -> bool {
    match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({"type":"object","additionalProperties":false,"required":["path"],
            "properties":{"path":{"type":"string","minLength":1},
                "limit":{"type":"integer","minimum":1,"maximum":20},
                "mode":{"type":"string","enum":["a","b"]},
                "tags":{"type":"array","items":{"type":"string"},"maxItems":2}}})
    }

    #[test]
    fn valid_arguments_and_reserved_fields_pass() {
        let args = json!({"path":"x","limit":3,"mode":null,"_repeat_of":"c1","_execution_mode":"foreground"});
        assert_eq!(check("file_read", &schema(), &args, None), Ok(()));
    }

    #[test]
    fn errors_name_the_argument_and_expectation() {
        let error = |args| check("file_read", &schema(), &args, None).unwrap_err();
        assert_eq!(
            error(json!({})),
            "file_read: missing required argument 'path'"
        );
        assert_eq!(
            error(json!({"path":"x","paths":"y"})),
            "file_read: unknown argument 'paths'; expected one of limit, mode, path, tags"
        );
        assert_eq!(
            error(json!({"path":"x","limit":"3"})),
            "file_read: argument 'limit' must be integer"
        );
        assert_eq!(
            error(json!({"path":"x","limit":30})),
            "file_read: argument 'limit' must be at most 20"
        );
        assert_eq!(
            error(json!({"path":"x","mode":"c"})),
            "file_read: argument 'mode' must be one of \"a\", \"b\""
        );
        assert_eq!(
            error(json!({"path":"x","tags":["a",1]})),
            "file_read: argument 'tags[1]' must be string"
        );
        assert_eq!(
            error(json!({"path":null})),
            "file_read: argument 'path' must be string"
        );
        assert_eq!(
            error(json!("not an object")),
            "file_read: arguments must be a JSON object"
        );
    }

    #[test]
    fn usage_hint_is_appended() {
        assert_eq!(
            check(
                "file_read",
                &schema(),
                &json!({}),
                Some("Pass a workspace-relative path.")
            ),
            Err(
                "file_read: missing required argument 'path'. Pass a workspace-relative path."
                    .to_owned()
            )
        );
    }
}
