//! Session scripts: a saved, named sequence of tool calls for one conversation tree.
//!
//! Pure rules only: the shape a script may take, which tools a step may name, and
//! `{{param}}` substitution, and the proposal rules for saved scripts, which last across
//! conversations and need the user's approval. Storage, tree resolution, live policy and
//! dispatch are in the
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

/// Most saved-script proposals one user and agent may have awaiting a decision.
pub const MAX_PENDING: usize = 50;

/// An empty saved-scripts document: approved `scripts` by name and pending `proposals` by
/// ID. `revision` advances on every change so the page cannot act on a stale view.
#[must_use]
pub fn saved_empty() -> Value {
    serde_json::json!({"revision":0,"scripts":{},"proposals":{}})
}

fn bump(doc: &mut Value) {
    doc["revision"] = (doc["revision"].as_u64().unwrap_or(0) + 1).into();
}

fn saved_revision(doc: &Value, name: &str) -> u64 {
    doc["scripts"][name]["revision"].as_u64().unwrap_or(0)
}

/// Records `script` as a proposal awaiting the user's decision. A newer proposal for the
/// same name replaces an older pending one.
///
/// # Errors
/// The saved-script or pending-proposal limit is reached.
pub fn propose(
    doc: &mut Value,
    script: &Script,
    id: &str,
    origin: &str,
    now: u64,
    max_saved: usize,
) -> Result<()> {
    let saved = doc["scripts"].as_object().map_or(0, Map::len);
    if doc["scripts"].get(&script.name).is_none() && saved >= max_saved {
        bail!(
            "This agent already has {max_saved} saved scripts; the user can delete one on the \
             Artifacts page"
        );
    }
    let base_revision = saved_revision(doc, &script.name);
    let proposals = doc["proposals"]
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Invalid saved-scripts document"))?;
    proposals.retain(|_, p| p["script"]["name"] != script.name.as_str());
    if proposals.len() >= MAX_PENDING {
        bail!("{MAX_PENDING} saved-script proposals already await the user's decision");
    }
    proposals.insert(
        id.to_owned(),
        serde_json::json!({"id":id,"script":script,"origin_session_id":origin,"created_at":now,
            "base_revision":base_revision}),
    );
    bump(doc);
    Ok(())
}

/// Approves or rejects a pending proposal and removes it. Approval saves the exact proposed
/// script, unless the saved script changed after it was proposed.
///
/// # Errors
/// No such proposal, a stale proposal, an invalid script or the saved-script limit.
pub fn decide(
    doc: &mut Value,
    id: &str,
    approve: bool,
    now: u64,
    max_saved: usize,
    max_steps: usize,
) -> Result<Value> {
    let Some(proposal) = doc["proposals"].get(id).cloned() else {
        bail!("Proposal is not pending");
    };
    if approve {
        let script: Script = serde_json::from_value(proposal["script"].clone())?;
        script.validate(max_steps)?;
        let current = saved_revision(doc, &script.name);
        if current != proposal["base_revision"].as_u64().unwrap_or(0) {
            bail!(
                "The saved script changed after this proposal; reject it and ask for a fresh one"
            );
        }
        let saved = doc["scripts"].as_object().map_or(0, Map::len);
        if current == 0 && saved >= max_saved {
            bail!("This agent already has {max_saved} saved scripts; delete one first");
        }
        doc["scripts"][&script.name] = serde_json::json!({"script":script,
            "revision":current + 1,"saved_at":now,
            "origin_session_id":proposal["origin_session_id"]});
    }
    if let Some(proposals) = doc["proposals"].as_object_mut() {
        proposals.remove(id);
    }
    bump(doc);
    Ok(serde_json::json!({"status":if approve {"approved"} else {"rejected"}}))
}

/// Deletes a saved script.
///
/// # Errors
/// No saved script has that name.
pub fn delete_saved(doc: &mut Value, name: &str) -> Result<()> {
    if doc["scripts"]
        .as_object_mut()
        .and_then(|scripts| scripts.remove(name))
        .is_none()
    {
        bail!("No saved script named {name}");
    }
    bump(doc);
    Ok(())
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
    fn saved_scripts_need_a_decision_and_reject_stale_proposals() {
        let mut doc = saved_empty();
        let one = script(json!([{"tool":"get_time"}]), json!([]));
        propose(&mut doc, &one, "p1", "s", 1, 2).unwrap();
        assert_eq!(doc["scripts"], json!({}), "a proposal saves nothing");
        // A second proposal for the same name replaces the pending one.
        propose(&mut doc, &one, "p2", "s", 2, 2).unwrap();
        assert!(doc["proposals"].get("p1").is_none());
        let stale = doc.clone();
        decide(&mut doc, "p2", true, 3, 2, 20).unwrap();
        assert_eq!(doc["scripts"]["capture"]["revision"], 1);
        assert_eq!(doc["proposals"], json!({}));
        assert!(decide(&mut doc, "p2", true, 4, 2, 20).is_err());
        // A proposal made before the approved edit is stale.
        propose(&mut doc, &one, "p3", "s", 5, 2).unwrap();
        let mut late = stale;
        late["scripts"] = doc["scripts"].clone();
        assert!(decide(&mut late, "p2", true, 6, 2, 20).is_err());
        decide(&mut doc, "p3", false, 7, 2, 20).unwrap();
        assert_eq!(
            doc["scripts"]["capture"]["revision"], 1,
            "rejecting keeps the saved one"
        );
        delete_saved(&mut doc, "capture").unwrap();
        assert!(delete_saved(&mut doc, "capture").is_err());
        assert_eq!(doc["revision"], 6);
    }

    #[test]
    fn saved_script_limits() {
        let mut doc = saved_empty();
        let mut a = script(json!([{"tool":"get_time"}]), json!([]));
        propose(&mut doc, &a, "p1", "s", 1, 1).unwrap();
        decide(&mut doc, "p1", true, 1, 1, 20).unwrap();
        a.name = "other".into();
        assert!(propose(&mut doc, &a, "p2", "s", 1, 1).is_err());
        // Replacing the saved one stays within the limit.
        a.name = "capture".into();
        propose(&mut doc, &a, "p3", "s", 1, 1).unwrap();
        for i in 0..MAX_PENDING {
            a.name = format!("n{i}");
            let _ = propose(&mut doc, &a, &format!("q{i}"), "s", 1, 100);
        }
        assert_eq!(doc["proposals"].as_object().unwrap().len(), MAX_PENDING);
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
