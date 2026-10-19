//! Version-one duplicate-result contracts and independent host validation. No persistence or authority.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use sha2::{Digest, Sha256};

pub const VERSION: u32 = 1;
pub const MAX_OBSERVATIONS: usize = 4096;
pub const MAX_TEMPLATE_BYTES: usize = 8192;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub call_id: String,
    pub tool: String,
    pub inputs: Value,
    pub scope: Value,
    /// Exact original result text, before previews. Never interaction-row metadata.
    pub output: String,
    pub success: bool,
    pub included: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct State {
    pub assignments: BTreeMap<String, Assignment>,
    pub last: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Assignment {
    pub key: String,
    pub output_sha256: String,
    /// Sequence identity survives removal of the original primary and intervening reset records.
    pub sequence: String,
    pub primary: String,
    pub placeholder: Option<String>,
    /// Captured configuration, not a code fallback; permits disabled-policy compaction repair.
    pub template: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub observations: Vec<Observation>,
    pub state: State,
    pub enabled: bool,
    pub template: Option<String>,
    /// Only the host may request rebasing after committed compaction/history restoration.
    pub rebase: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub version: u32,
    pub state: State,
    /// Replacements keyed by execution ID; omission means use its full original result.
    pub replacements: BTreeMap<String, String>,
}

/// Templates are mandatory when enabled; malformed braces and unknown variables are rejected.
pub fn validate_template(template: &str) -> Result<()> {
    if template.trim().is_empty() || template.len() > MAX_TEMPLATE_BYTES {
        bail!("prompts.json: tool_result_deduplication.placeholder must be nonempty and at most 8192 bytes");
    }
    let mut rest = template;
    while let Some(index) = rest.find(['{', '}']) {
        rest = &rest[index..];
        let token = ["{current_call_id}", "{reference_call_id}", "{tool}"]
            .into_iter().find(|token| rest.starts_with(token))
            .context("prompts.json: unsupported or malformed duplicate-result placeholder variable")?;
        rest = &rest[token.len()..];
    }
    Ok(())
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}

/// Sort object keys only for tool-input identity. Result text is never parsed or normalized.
pub fn canonical(value: &Value) -> Result<String> {
    Ok(match value {
        Value::Object(map) => {
            let sorted: BTreeMap<_, _> = map.iter().collect();
            let fields = sorted.into_iter().map(|(key, value)| {
                Ok(format!("{}:{}", serde_json::to_string(key)?, canonical(value)?))
            }).collect::<Result<Vec<_>>>()?;
            format!("{{{}}}", fields.join(","))
        }
        Value::Array(items) => format!("[{}]", items.iter().map(canonical)
            .collect::<Result<Vec<_>>>()?.join(",")),
        _ => serde_json::to_string(value)?,
    })
}

pub fn key(item: &Observation) -> Result<String> {
    Ok(fingerprint(&canonical(&json!([item.tool, item.inputs, item.scope]))?))
}

pub fn replacement(item: &Observation, primary: &str, template: &str) -> Result<Option<String>> {
    validate_template(template)?;
    // Single-pass substitution never interprets variable-like text inside an ID or tool name.
    let mut rendered = String::new();
    let mut rest = template;
    while let Some(index) = rest.find('{') {
        rendered.push_str(&rest[..index]);
        rest = &rest[index..];
        let (token, value) = if rest.starts_with("{current_call_id}") {
            ("{current_call_id}", item.call_id.as_str())
        } else if rest.starts_with("{reference_call_id}") {
            ("{reference_call_id}", primary)
        } else { ("{tool}", item.tool.as_str()) };
        rendered.push_str(value);
        rest = &rest[token.len()..];
    }
    rendered.push_str(rest);
    let receipt = json!({"status":"unchanged", "output":rendered,
        "result_id":item.call_id, "reference_call_id":primary,
        "recall_reference":{"tool":"recall_compacted", "tool_call_id":primary}}).to_string();
    Ok((serde_json::to_vec(&receipt)?.len() < serde_json::to_vec(&item.output)?.len()).then_some(receipt))
}


pub fn fingerprint(value: &str) -> String { hex::encode(Sha256::digest(value.as_bytes())) }

/// Validate exact host-owned correspondence; the service may neither invent content nor rewrite old assignments.
pub fn validate_plan(request: &Request, plan: &Plan) -> Result<()> {
    if request.version != VERSION || plan.version != VERSION || request.observations.len() > MAX_OBSERVATIONS {
        bail!("Invalid duplicate projection version or bounds");
    }
    let items: BTreeMap<_,_> = request.observations.iter().map(|item| (item.call_id.as_str(), item)).collect();
    if items.len()!=request.observations.len() {bail!("Ambiguous duplicate projection IDs");}
    if plan.state.assignments.len()>MAX_OBSERVATIONS*2 || plan.state.last.len()>MAX_OBSERVATIONS*2 {bail!("Projection state exceeds contract bounds");}
    for (id,entry) in &plan.state.assignments {
        if !items.contains_key(id.as_str()) && (request.rebase || request.state.assignments.get(id)!=Some(entry)) {
            bail!("Projection invented or changed an inactive assignment");
        }
    }
    if !request.rebase && request.state.assignments.keys().any(|id|!plan.state.assignments.contains_key(id)) {
        bail!("Projection removed an existing assignment");
    }
    let latest: BTreeMap<_,_>=request.observations.iter().filter(|item|item.success)
        .filter_map(|item|plan.state.assignments.get(&item.call_id).map(|entry|(entry.sequence.as_str(),item.call_id.as_str()))).collect();
    let mut expected_replacements=BTreeMap::new();
    let mut last=request.state.last.clone();
    for item in &request.observations {
        let entry=plan.state.assignments.get(&item.call_id).context("Missing projection assignment")?;
        if entry.key!=key(item)? || entry.output_sha256!=fingerprint(&item.output) {bail!("Projection original identity mismatch");}
        if let Some(old)=request.state.assignments.get(&item.call_id) {
            if (!request.rebase && old!=entry) || old.key!=entry.key || old.output_sha256!=entry.output_sha256 || old.sequence!=entry.sequence {
                bail!("Projection rewrote an existing sequence");
            }
        } else {
            let previous=last.get(&entry.key).and_then(|id|items.get(id.as_str()));
            let duplicate=request.enabled && item.included && item.success && previous.is_some_and(|old|
                old.success && old.included && old.tool==item.tool && old.inputs==item.inputs && old.scope==item.scope && old.output==item.output
                    && plan.state.assignments.get(&old.call_id).is_some_and(|entry|entry.template.is_some()));
            let previous_entry=previous.and_then(|old|plan.state.assignments.get(&old.call_id));
            let sequence=if duplicate {previous_entry.map(|e|e.sequence.as_str()).unwrap_or(&item.call_id)} else {&item.call_id};
            if entry.sequence!=sequence {bail!("Projection crossed a result-change boundary");}
            let expected_template=if request.enabled && item.included {request.template.as_ref()} else {None};
            if &entry.template!=&expected_template.cloned() {bail!("Projection changed configured template");}
            last.insert(entry.key.clone(),item.call_id.clone());
        }
        if request.rebase && item.success && latest.get(entry.sequence.as_str()).copied()!=Some(entry.primary.as_str()) {
            bail!("Projection did not promote the latest surviving occurrence");
        }
        if let Some(text)=&entry.placeholder {
            let primary=items.get(entry.primary.as_str()).context("Projection full reference missing")?;
            let primary_entry=plan.state.assignments.get(&entry.primary).context("Projection primary assignment missing")?;
            if !item.success || !primary.success || item.tool!=primary.tool || item.inputs!=primary.inputs || item.scope!=primary.scope
                || item.output!=primary.output || entry.sequence!=primary_entry.sequence || primary_entry.placeholder.is_some() || item.call_id==entry.primary {
                bail!("Projection referenced unequal or hidden output");
            }
            let template=if request.rebase {request.template.as_deref().or(entry.template.as_deref())} else {entry.template.as_deref()}
                .context("Projection template missing")?;
            if replacement(item,&entry.primary,template)?.as_ref()!=Some(text) {bail!("Projection placeholder differs from configuration");}
            expected_replacements.insert(item.call_id.clone(),text.clone());
        }
    }
    if request.rebase {
        let present_keys:BTreeSet<_>=request.observations.iter().map(key).collect::<Result<_>>()?;
        last.retain(|key,_|present_keys.contains(key));
        for id in last.values_mut() {
            if !items.contains_key(id.as_str()) {
                if let Some(previous)=request.state.assignments.get(id) {
                    if let Some(primary)=latest.get(previous.sequence.as_str()) { *id=(*primary).to_owned(); }
                }
            }
        }
    }
    if plan.replacements!=expected_replacements || plan.state.last!=last {bail!("Projection replacement coverage mismatch");}
    Ok(())
}
