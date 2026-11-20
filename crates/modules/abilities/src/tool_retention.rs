//! What a tool's stored calls keep once the conversation has moved past them: behind the
//! compaction fold, or in a session idle for the storage policy's `trim_idle_days`.
//! `<tool>.tool.json` sets it with `retention`; a tool without one keeps everything.
//! The model context ahead of the fold is never changed here.
use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::{json, Map, Value};

/// Fields a trimmed output always keeps: they say how the call ended.
const OUTCOME_FIELDS: [&str; 5] = [
    "status",
    "error_kind",
    "error_code",
    "exit_code",
    "truncated",
];
/// A log output keeps any other field at most this long, as JSON.
const SHORT_FIELD_BYTES: usize = 200;
/// An argument shorter than this is kept even when its field is listed to drop.
const SHORT_ARGUMENT_BYTES: usize = 200;
const MAX_FIELDS: usize = 32;
const MAX_TAIL_LINES: u32 = 1000;
/// A kept tail line averages at most this many bytes, so one huge line cannot keep everything.
const TAIL_LINE_BYTES: usize = 400;
/// Marks a trimmed output so a later pass leaves it alone.
const MARKER: &str = "retention";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OutputLevel {
    /// Re-runnable lookups (reads, listings, searches): only the outcome is kept.
    Disposable,
    /// Command and build logs: the outcome, short fields, `keep_fields`, and long text
    /// cut to its last `tail_lines` lines.
    Log,
    /// Evidence (messages sent, screenshots, changes made): kept whole.
    #[default]
    Record,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Retention {
    #[serde(default)]
    pub output: OutputLevel,
    /// Log level: result fields kept whole whatever their size.
    #[serde(default)]
    pub keep_fields: Vec<String>,
    /// Log level: long text fields keep their last lines; absent drops them.
    #[serde(default)]
    pub tail_lines: Option<u32>,
    /// Argument fields replaced by their size once trimmed, such as a written file's content.
    #[serde(default)]
    pub drop_input_fields: Vec<String>,
}

impl Retention {
    pub fn validate(&self, parameters: &Value, folder: &str) -> Result<()> {
        if self.output != OutputLevel::Log
            && (!self.keep_fields.is_empty() || self.tail_lines.is_some())
        {
            bail!("retention keep_fields and tail_lines need output \"log\": {folder}");
        }
        if self
            .tail_lines
            .is_some_and(|lines| lines == 0 || lines > MAX_TAIL_LINES)
        {
            bail!("retention tail_lines must be 1..{MAX_TAIL_LINES}: {folder}");
        }
        let properties = parameters["properties"].as_object();
        if self.keep_fields.len() > MAX_FIELDS
            || self.drop_input_fields.len() > MAX_FIELDS
            || self
                .keep_fields
                .iter()
                .any(|field| field.is_empty() || field.len() > 64)
            || self
                .drop_input_fields
                .iter()
                .any(|field| !properties.is_some_and(|known| known.contains_key(field)))
        {
            bail!("retention fields must be short names and drop_input_fields tool parameters: {folder}");
        }
        Ok(())
    }

    pub fn keeps_everything(&self) -> bool {
        self.output == OutputLevel::Record && self.drop_input_fields.is_empty()
    }
}

/// The stored output trimmed to `level`, or `None` when it stays as it is (already trimmed,
/// a record, or nothing to remove). Outputs are JSON text, usually an object.
pub fn trimmed_output(retention: &Retention, level: OutputLevel, output: &Value) -> Option<Value> {
    if level == OutputLevel::Record {
        return None;
    }
    let text = output.as_str()?;
    let trimmed = match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(fields)) => {
            if fields.contains_key(MARKER) {
                return None;
            }
            Value::Object(trimmed_fields(retention, level, fields))
        }
        // Plain text: a log keeps its tail, anything else only the marker.
        _ => {
            let mut kept = Map::new();
            if let (OutputLevel::Log, Some(lines)) = (level, retention.tail_lines) {
                kept.insert("output".into(), json!(tail(text, lines)));
            }
            kept.insert(MARKER.into(), json!(note(level)));
            Value::Object(kept)
        }
    };
    let trimmed = serde_json::to_string(&trimmed).ok()?;
    (trimmed.len() < text.len()).then(|| json!(trimmed))
}

fn trimmed_fields(
    retention: &Retention,
    level: OutputLevel,
    fields: Map<String, Value>,
) -> Map<String, Value> {
    let mut kept = Map::new();
    for (name, value) in fields {
        let keep = OUTCOME_FIELDS.contains(&name.as_str())
            || (level == OutputLevel::Log
                && (retention.keep_fields.contains(&name)
                    || serde_json::to_string(&value)
                        .is_ok_and(|json| json.len() <= SHORT_FIELD_BYTES)));
        if keep {
            kept.insert(name, value);
        } else if let (OutputLevel::Log, Some(lines), Value::String(text)) =
            (level, retention.tail_lines, &value)
        {
            kept.insert(name, json!(tail(text, lines)));
        }
    }
    kept.insert(MARKER.into(), json!(note(level)));
    kept
}

fn note(level: OutputLevel) -> &'static str {
    match level {
        OutputLevel::Log => "Long text cut to its end after the conversation moved on; run the tool again for the rest.",
        _ => "Output removed after the conversation moved on; run the tool again if it is needed.",
    }
}

/// The last `lines` lines of `text`, at most `lines * TAIL_LINE_BYTES` bytes of them.
fn tail(text: &str, lines: u32) -> String {
    let all: Vec<&str> = text.lines().collect();
    let keep = (lines as usize).min(all.len());
    let mut kept = all[all.len() - keep..].join("\n");
    let cap = lines as usize * TAIL_LINE_BYTES;
    if kept.len() > cap {
        let mut start = kept.len() - cap;
        while !kept.is_char_boundary(start) {
            start += 1;
        }
        kept = kept[start..].to_owned();
    }
    if keep == all.len() && kept.len() == text.trim_end_matches('\n').len() {
        return text.to_owned();
    }
    format!("[{} earlier lines removed]\n{kept}", all.len() - keep)
}

/// The call's JSON arguments with `drop_input_fields` replaced by their size, or `None`.
pub fn trimmed_arguments(retention: &Retention, arguments: &Value) -> Option<Value> {
    if retention.drop_input_fields.is_empty() {
        return None;
    }
    let text = arguments.as_str()?;
    let Ok(Value::Object(mut fields)) = serde_json::from_str::<Value>(text) else {
        return None;
    };
    let mut changed = false;
    for name in &retention.drop_input_fields {
        let Some(value) = fields.get_mut(name) else {
            continue;
        };
        let size = serde_json::to_string(value).map_or(0, |json| json.len());
        if size >= SHORT_ARGUMENT_BYTES {
            *value = json!(format!(
                "[removed after the conversation moved on: {size} bytes]"
            ));
            changed = true;
        }
    }
    changed.then(|| json!(serde_json::to_string(&fields).unwrap_or_default()))
}

/// The `path` argument of a call, used to find reads that a later write made stale.
pub fn call_path(arguments: &Value) -> Option<String> {
    let parsed: Value = serde_json::from_str(arguments.as_str()?).ok()?;
    parsed["path"].as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn retention(value: Value) -> Retention {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn disposable_keeps_only_the_outcome_and_is_trimmed_once() {
        let rule = retention(json!({"output":"disposable"}));
        let output = json!(json!({"status":"ok","output":"x".repeat(500),"offset":1}).to_string());
        let trimmed = trimmed_output(&rule, OutputLevel::Disposable, &output).unwrap();
        let fields: Value = serde_json::from_str(trimmed.as_str().unwrap()).unwrap();
        assert_eq!(fields["status"], "ok");
        assert!(fields.get("output").is_none() && fields.get("offset").is_none());
        assert!(fields[MARKER].is_string());
        assert_eq!(
            trimmed_output(&rule, OutputLevel::Disposable, &trimmed),
            None
        );
        assert_eq!(trimmed_output(&rule, OutputLevel::Record, &output), None);
    }

    #[test]
    fn log_keeps_short_and_named_fields_and_the_tail_of_long_text() {
        let rule = retention(json!({"output":"log","tail_lines":2,"keep_fields":["steps"]}));
        let lines: Vec<String> = (1..=100).map(|n| format!("line {n}")).collect();
        let output = json!(
            json!({"status":"failed","job_id":"j1","output":lines.join("\n"),
            "steps":["a".repeat(300)],"other":"y".repeat(300)})
            .to_string()
        );
        let trimmed = trimmed_output(&rule, OutputLevel::Log, &output).unwrap();
        let fields: Value = serde_json::from_str(trimmed.as_str().unwrap()).unwrap();
        assert_eq!(fields["job_id"], "j1");
        assert_eq!(fields["steps"][0].as_str().unwrap().len(), 300);
        assert_eq!(
            fields["output"],
            "[98 earlier lines removed]\nline 99\nline 100"
        );
        assert_eq!(fields["other"], "y".repeat(300));
        let wide = tail(&"w".repeat(2_000), 1);
        assert!(
            wide.starts_with("[0 earlier lines removed]\n") && wide.ends_with(&"w".repeat(400))
        );
    }

    #[test]
    fn plain_text_and_small_outputs() {
        let rule = retention(json!({"output":"log","tail_lines":1}));
        let output = json!(format!("{}last", "first\n".repeat(50)));
        let trimmed = trimmed_output(&rule, OutputLevel::Log, &output).unwrap();
        let fields: Value = serde_json::from_str(trimmed.as_str().unwrap()).unwrap();
        assert_eq!(fields["output"], "[50 earlier lines removed]\nlast");
        // Trimming that would not shrink the row leaves it alone.
        assert_eq!(
            trimmed_output(
                &rule,
                OutputLevel::Disposable,
                &json!("{\"status\":\"ok\"}")
            ),
            None
        );
    }

    #[test]
    fn listed_long_arguments_are_replaced_by_their_size() {
        let rule = retention(json!({"drop_input_fields":["content"]}));
        let arguments = json!(json!({"path":"a.txt","content":"z".repeat(400)}).to_string());
        let trimmed = trimmed_arguments(&rule, &arguments).unwrap();
        let fields: Value = serde_json::from_str(trimmed.as_str().unwrap()).unwrap();
        assert_eq!(fields["path"], "a.txt");
        assert_eq!(
            fields["content"],
            "[removed after the conversation moved on: 402 bytes]"
        );
        assert_eq!(trimmed_arguments(&rule, &trimmed), None);
        assert_eq!(call_path(&arguments).as_deref(), Some("a.txt"));
    }

    #[test]
    fn validation() {
        let parameters = json!({"properties":{"content":{}}});
        assert!(retention(json!({"output":"log","tail_lines":40}))
            .validate(&parameters, "t")
            .is_ok());
        assert!(retention(json!({"output":"disposable","tail_lines":40}))
            .validate(&parameters, "t")
            .is_err());
        assert!(retention(json!({"output":"log","tail_lines":0}))
            .validate(&parameters, "t")
            .is_err());
        assert!(retention(json!({"drop_input_fields":["missing"]}))
            .validate(&parameters, "t")
            .is_err());
        assert!(serde_json::from_value::<Retention>(json!({"output":"gone"})).is_err());
        assert!(Retention::default().keeps_everything());
    }
}
