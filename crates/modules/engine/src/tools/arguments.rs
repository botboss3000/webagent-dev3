//! Checks a tool call's arguments against the tool's `parameters` schema before anything
//! else sees them, so a bad call returns a precise error instead of reaching the tool or
//! asking the user to approve it. Covers the JSON-schema subset tool definitions use:
//! `type`, `properties`, `required`, `additionalProperties`, `items`, `enum`,
//! `minimum`/`maximum`, `minLength`/`maxLength`, `minItems`/`maxItems`, `pattern`,
//! `format: uuid`, `anyOf` and a schema-valued `additionalProperties`. [`schema_problems`] refuses any other keyword at release,
//! so a schema never relies on a rule this check would skip; [`contract_changes`] names
//! the edits a tool's code would have to follow.
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

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
    if let Some(options) = schema["anyOf"].as_array() {
        let mut problems = Vec::new();
        for option in options {
            match self::value(option, value, path) {
                Ok(()) => {
                    problems.clear();
                    break;
                }
                Err(problem) => problems.push(problem),
            }
        }
        if !problems.is_empty() {
            return Err(format!(
                "{} matches none of the allowed forms ({})",
                label(path),
                problems.join("; ")
            ));
        }
    }
    if let Some(text) = value.as_str() {
        if let Some(pattern) = schema["pattern"].as_str() {
            if !matches_pattern(pattern, text) {
                return Err(format!("{} must match {pattern}", label(path)));
            }
        }
        if schema["format"] == "uuid" && !is_uuid(text) {
            return Err(format!("{} must be a UUID", label(path)));
        }
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
                None if schema["additionalProperties"].is_object() => {
                    self::value(&schema["additionalProperties"], item, &join(path, key))?
                }
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

/// Compiled schema patterns. Tool definitions hold a few dozen distinct patterns, so the
/// cache stays small; an invalid one never matches (the release check refuses it).
static PATTERNS: LazyLock<Mutex<HashMap<String, Option<Regex>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn matches_pattern(pattern: &str, text: &str) -> bool {
    let mut cache = PATTERNS.lock().unwrap_or_else(|e| e.into_inner());
    cache
        .entry(pattern.to_owned())
        .or_insert_with(|| Regex::new(pattern).ok())
        .as_ref()
        .is_some_and(|regex| regex.is_match(text))
}

/// The hyphenated form only: `8-4-4-4-12` hex digits.
fn is_uuid(text: &str) -> bool {
    text.len() == 36
        && text.char_indices().all(|(index, c)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

/// Keywords [`check`] enforces, plus annotations it may ignore.
const KEYWORDS: [&str; 18] = [
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "minItems",
    "maxItems",
    "pattern",
    "format",
    "anyOf",
    "description",
    "title",
    "default",
];

/// Release-time problems with one tool's `parameters` schema: a keyword [`check`] would
/// skip, an invalid pattern, a lower limit above its upper one (no call could pass), an
/// unknown format, or an `additionalProperties` that is neither a boolean nor a schema.
pub fn schema_problems(name: &str, schema: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    schema_walk(schema, "", &mut |path, schema| {
        let Some(object) = schema.as_object() else {
            problems.push(format!("{name}: {} is not a schema object", label(path)));
            return;
        };
        for key in object.keys() {
            if !KEYWORDS.contains(&key.as_str()) {
                problems.push(format!(
                    "{name}: {} uses unsupported keyword '{key}'",
                    label(path)
                ));
            }
        }
        if let Some(pattern) = object.get("pattern") {
            if pattern
                .as_str()
                .is_none_or(|pattern| Regex::new(pattern).is_err())
            {
                problems.push(format!("{name}: {} has an invalid pattern", label(path)));
            }
        }
        for (low, high) in [
            ("minLength", "maxLength"),
            ("minimum", "maximum"),
            ("minItems", "maxItems"),
        ] {
            let bound = |key: &str| object.get(key).and_then(Value::as_f64);
            if let (Some(low_value), Some(high_value)) = (bound(low), bound(high)) {
                if low_value > high_value {
                    problems.push(format!("{name}: {} has {low} above {high}", label(path)));
                }
            }
        }
        if object.get("format").is_some_and(|format| format != "uuid") {
            problems.push(format!("{name}: {} has an unsupported format", label(path)));
        }
        if object
            .get("additionalProperties")
            .is_some_and(|value| !value.is_boolean() && !value.is_object())
        {
            problems.push(format!(
                "{name}: {} additionalProperties must be true, false or a schema",
                label(path)
            ));
        }
    });
    problems
}

/// Visits every subschema with its argument path.
fn schema_walk(schema: &Value, path: &str, visit: &mut impl FnMut(&str, &Value)) {
    visit(path, schema);
    if let Some(properties) = schema["properties"].as_object() {
        for (key, property) in properties {
            schema_walk(property, &join(path, key), visit);
        }
    }
    if !schema["items"].is_null() {
        schema_walk(&schema["items"], &format!("{path}[]"), visit);
    }
    if schema["additionalProperties"].is_object() {
        schema_walk(&schema["additionalProperties"], &join(path, "*"), visit);
    }
    for option in schema["anyOf"].as_array().into_iter().flatten() {
        schema_walk(option, path, visit);
    }
}

/// Edits from `old` to `new` that a tool's code would have to follow: an argument added,
/// removed or renamed, a type change, an allowed value added, a required argument made
/// optional, an `additionalProperties` or `anyOf` change. Descriptions, limits, patterns
/// and removed allowed values are left to [`check`], which enforces them on every call.
pub fn contract_changes(name: &str, old: &Value, new: &Value) -> Vec<String> {
    let mut changes = Vec::new();
    contract_walk(old, new, "", &mut changes);
    changes
        .into_iter()
        .map(|change| format!("{name}: {change}"))
        .collect()
}

fn contract_walk(old: &Value, new: &Value, path: &str, changes: &mut Vec<String>) {
    let at = label(path);
    if old["type"] != new["type"] {
        changes.push(format!("{at} changed type"));
    }
    match (&old["additionalProperties"], &new["additionalProperties"]) {
        (Value::Object(_), Value::Object(_)) => contract_walk(
            &old["additionalProperties"],
            &new["additionalProperties"],
            &join(path, "*"),
            changes,
        ),
        (old, new) if old != new => changes.push(format!("{at} changed additionalProperties")),
        _ => {}
    }
    match (old["enum"].as_array(), new["enum"].as_array()) {
        (Some(_), None) => changes.push(format!("{at} dropped its allowed values")),
        (Some(old), Some(new)) => {
            for value in new.iter().filter(|value| !old.contains(value)) {
                changes.push(format!("{at} added allowed value {value}"));
            }
        }
        _ => {}
    }
    let required = |schema: &Value| -> Vec<String> {
        schema["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    let now_required = required(new);
    for key in required(old) {
        if !now_required.contains(&key) {
            changes.push(format!(
                "{} is no longer required",
                label(&join(path, &key))
            ));
        }
    }
    let empty = Map::new();
    let old_properties = old["properties"].as_object().unwrap_or(&empty);
    let new_properties = new["properties"].as_object().unwrap_or(&empty);
    for (key, property) in old_properties {
        match new_properties.get(key) {
            Some(next) => contract_walk(property, next, &join(path, key), changes),
            None => changes.push(format!(
                "{} was removed or renamed",
                label(&join(path, key))
            )),
        }
    }
    for key in new_properties.keys() {
        if !old_properties.contains_key(key) {
            changes.push(format!("{} was added", label(&join(path, key))));
        }
    }
    if old["items"].is_null() != new["items"].is_null() {
        changes.push(format!("{at} changed its item schema"));
    } else if !old["items"].is_null() {
        contract_walk(&old["items"], &new["items"], &format!("{path}[]"), changes);
    }
    match (old["anyOf"].as_array(), new["anyOf"].as_array()) {
        (None, None) => {}
        (Some(old), Some(new)) if old.len() == new.len() => {
            for (old, new) in old.iter().zip(new) {
                contract_walk(old, new, path, changes);
            }
        }
        _ => changes.push(format!("{at} changed its allowed forms")),
    }
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
    fn patterns_formats_and_any_of_are_enforced() {
        let schema = json!({"type":"object","properties":{
            "id":{"type":"string","pattern":"^[a-f0-9]{4}$"},
            "session":{"type":"string","format":"uuid"},
            "pick":{"anyOf":[{"type":"string","enum":["all"]},{"type":"integer","minimum":1}]}}});
        let check = |args| check("t", &schema, &args, None);
        assert_eq!(
            check(
                json!({"id":"ab12","session":"0f8fad5b-d9cb-469f-a165-70867728950e","pick":"all"})
            ),
            Ok(())
        );
        assert_eq!(check(json!({"pick":3})), Ok(()));
        let named = json!({"type":"object","properties":{"args":{"type":"object",
            "additionalProperties":{"type":"string","maxLength":2}}}});
        assert_eq!(
            super::check("t", &named, &json!({"args":{"a":"ok"}}), None),
            Ok(())
        );
        assert_eq!(
            super::check("t", &named, &json!({"args":{"a":"long"}}), None),
            Err("t: argument 'args.a' is longer than 2".to_owned())
        );
        assert_eq!(
            check(json!({"id":"AB12"})),
            Err("t: argument 'id' must match ^[a-f0-9]{4}$".to_owned())
        );
        assert_eq!(
            check(json!({"session":"0f8fad5bd9cb469fa16570867728950e"})),
            Err("t: argument 'session' must be a UUID".to_owned())
        );
        assert_eq!(
            check(json!({"pick":0})),
            Err("t: argument 'pick' matches none of the allowed forms (argument 'pick' must be string; argument 'pick' must be at least 1)".to_owned())
        );
    }

    #[test]
    fn release_refuses_keywords_the_check_would_skip() {
        let bad = json!({"type":"object","properties":{
            "format":{"type":"string","enum":["a"]},
            "x":{"type":"string","pattern":"("},
            "y":{"type":"string","format":"email"},
            "z":{"type":"object","additionalProperties":1,"oneOf":[]},
            "w":{"type":"object","additionalProperties":{"type":"string","maxLength":2}},
            "v":{"type":"string","minLength":3,"maxLength":2}}});
        assert_eq!(
            schema_problems("t", &bad),
            vec![
                "t: argument 'v' has minLength above maxLength",
                "t: argument 'x' has an invalid pattern",
                "t: argument 'y' has an unsupported format",
                "t: argument 'z' uses unsupported keyword 'oneOf'",
                "t: argument 'z' additionalProperties must be true, false or a schema",
            ]
        );
        assert!(schema_problems("file_read", &schema()).is_empty());
    }

    #[test]
    fn contract_changes_name_edits_the_code_must_follow() {
        let old = schema();
        let mut safe = old.clone();
        safe["properties"]["limit"]["maximum"] = json!(50);
        safe["properties"]["mode"]["enum"] = json!(["a"]);
        safe["properties"]["path"]["description"] = json!("Where");
        safe["required"] = json!(["path", "limit"]);
        assert!(contract_changes("t", &old, &safe).is_empty());
        let mut breaking = old.clone();
        breaking["properties"]["limit"]["type"] = json!("string");
        breaking["properties"]["mode"]["enum"] = json!(["a", "b", "c"]);
        let tags = breaking["properties"]["tags"].take();
        breaking["properties"]
            .as_object_mut()
            .unwrap()
            .remove("tags");
        breaking["properties"]["labels"] = tags;
        breaking["required"] = json!([]);
        assert_eq!(
            contract_changes("t", &old, &breaking),
            vec![
                "t: argument 'path' is no longer required",
                "t: argument 'limit' changed type",
                "t: argument 'mode' added allowed value \"c\"",
                "t: argument 'tags' was removed or renamed",
                "t: argument 'labels' was added",
            ]
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
