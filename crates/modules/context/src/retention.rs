//! Run-local metadata for projecting old tool outputs from model context.
//!
//! Raw outputs remain in durable run input. This module stores only bounded
//! references and returns a projected copy for one model request.
use crate::ports::AgentMetadata;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
};
use webagent_access::storage::RecordCodec;

pub const CONTROL_NAME: &str = "manage_tool_context";
const STATE_KEY: &str = "tool_context_state";
const POLICY_KEY: &str = "tool_context_policy";
const MAX_ENTRIES: usize = 512;
const MAX_TOOLS: usize = 128;
const MAX_REASON_CHARS: usize = 256;
const MAX_PLACEHOLDER_CHARS: usize = 2048;
const MAX_LIFETIME_CALLS: u64 = 1_000;
const MAX_OUTPUT_CHARS: usize = 262_144;
const MAX_POLICY_BYTES: usize = 64 * 1024;
/// Live policy file, relative to the checkout.
pub const LIVE_POLICY: &str = "crates/modules/context/tool-output-retention.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    CodeIndex,
    Selected,
    AllEligible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub enabled: bool,
    pub scope: Scope,
    pub tools: Vec<String>,
    pub lifetime_calls: u64,
    pub ordinary_chars: usize,
    pub promoted_chars: usize,
    pub refresh_on_recall: bool,
    pub promote_on_edit: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: false,
            scope: Scope::CodeIndex,
            tools: Vec::new(),
            lifetime_calls: 6,
            ordinary_chars: 24_000,
            promoted_chars: 24_000,
            refresh_on_recall: true,
            promote_on_edit: true,
        }
    }
}

impl Policy {
    /// Validate original file bytes before JSON object normalization can discard duplicate fields.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_POLICY_BYTES {
            bail!("Tool output retention policy exceeds limit");
        }
        let policy: Self = serde_json::from_slice(bytes)?;
        policy.validate()?;
        Ok(policy)
    }
    /// Read the live policy file (`live-files.json`) on every use.
    pub fn live() -> Result<Self> {
        Self::read_in(&webagent_configuration::live::roots())
    }
    /// Read the policy from the first of `roots` that has it; tests pass their own root.
    pub fn read_in(roots: &[std::path::PathBuf]) -> Result<Self> {
        webagent_configuration::live::parsed_from(
            roots,
            LIVE_POLICY,
            MAX_POLICY_BYTES as u64,
            Self::parse,
        )
    }

    pub fn validate(&self) -> Result<()> {
        if self.lifetime_calls == 0 || self.lifetime_calls > MAX_LIFETIME_CALLS {
            bail!("Invalid tool output lifetime");
        }
        if !(1_024..=MAX_OUTPUT_CHARS).contains(&self.ordinary_chars)
            || !(1_024..=MAX_OUTPUT_CHARS).contains(&self.promoted_chars)
        {
            bail!("Invalid tool output character budget");
        }
        if self.tools.len() > MAX_TOOLS {
            bail!("Too many selected tool names");
        }
        let mut seen = HashSet::new();
        for name in &self.tools {
            if name.is_empty()
                || name.len() > 128
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                || !eligible_tools().contains(&name.as_str())
                || !seen.insert(name)
            {
                bail!("Invalid or duplicate selected tool name");
            }
        }
        if self.scope == Scope::Selected && self.tools.is_empty() {
            bail!("Selected scope requires at least one tool");
        }
        Ok(())
    }

    pub fn eligible(&self, name: &str) -> bool {
        if !self.enabled || !perishable_read(name) {
            return false;
        }
        match self.scope {
            Scope::CodeIndex => matches!(
                name,
                "code_index_lookup"
                    | "code_index_coverage"
                    | "code_index_trace"
                    | "code_index_progress"
            ),
            Scope::Selected => self.tools.iter().any(|tool| tool == name),
            Scope::AllEligible => true,
        }
    }
}

/// Load an agent override from encrypted agent metadata, without returning it
/// to any model-facing output. The override is a complete effective policy.
pub async fn agent_override(
    store: &impl crate::ports::AgentMetadata,
    agent_id: &str,
) -> Result<Option<Value>> {
    let view = store.for_agent(agent_id).await?;
    let blob: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
        .bind(agent_id)
        .fetch_one(view.agent_pool())
        .await?;
    let metadata = view.unseal(&blob)?;
    let Some(value) = metadata.get("tool_output_policy") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let effective = value.get("effective").unwrap_or(value).clone();
    Ok(Some(effective))
}

/// Resolve a fresh policy at each model-call boundary.
pub async fn effective(store: &impl crate::ports::AgentMetadata, agent_id: &str) -> Result<Policy> {
    let mut policy = Policy::live()?;
    if let Some(override_value) = agent_override(store, agent_id).await? {
        policy = serde_json::from_value(override_value)?;
    }
    policy.validate()?;
    // Lifetime/budget eviction remains experimental and is not active. Preserve saved settings.
    policy.enabled = false;
    Ok(policy)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompts {
    pub guidance: String,
    pub placeholder: String,
}

fn embedded_prompt_defaults(template: &Value) -> Result<(String, String)> {
    let fields = template
        .get("tool_output_retention")
        .and_then(Value::as_object)
        .context("Embedded tool output retention prompt section is missing or invalid")?;
    let field = |name: &str| -> Result<String> {
        let text = fields.get(name).and_then(Value::as_str).with_context(|| {
            format!("Embedded tool output retention {name} prompt is missing or invalid")
        })?;
        if text.trim().is_empty() || text.len() > 8192 {
            bail!("Embedded tool output retention prompt is invalid");
        }
        Ok(text.to_owned())
    };
    Ok((field("guidance")?, field("placeholder")?))
}

impl Prompts {
    /// Read live prompts. Missing section/keys fall back to validated embedded JSON;
    /// malformed present values fail closed.
    pub fn read(root: &Path) -> Result<Self> {
        Self::read_at(root, crate::defaults::published_root().as_deref())
    }
    pub fn read_at(root: &Path, source: Option<&Path>) -> Result<Self> {
        let template: Value = serde_json::from_slice(&crate::defaults::prompt_defaults_at(source)?)
            .context("Embedded prompt configuration is malformed")?;
        let (guidance_default, placeholder_default) = embedded_prompt_defaults(&template)?;
        let live = crate::defaults::resolved_prompts_at(root, source)?;
        let section = live.get("tool_output_retention");
        if section.is_some_and(|value| !value.is_object()) {
            bail!("Invalid tool output retention prompt section");
        }
        let field = |name: &str, fallback: &str| -> Result<String> {
            let Some(value) = section.and_then(|section| section.get(name)) else {
                return Ok(fallback.to_owned());
            };
            let Some(text) = value.as_str() else {
                bail!("Invalid tool output retention prompt");
            };
            if text.trim().is_empty() || text.len() > 8192 {
                bail!("Tool output retention prompt is invalid");
            }
            Ok(text.to_owned())
        };
        Ok(Self {
            guidance: field("guidance", &guidance_default)?,
            placeholder: field("placeholder", &placeholder_default)?,
        })
    }
}

/// Explicit perishable-read list exposed to the settings API.
pub fn eligible(name: &str) -> bool {
    perishable_read(name)
}

pub fn eligible_tools() -> &'static [&'static str] {
    &[
        "code_index_lookup",
        "code_index_coverage",
        "code_index_trace",
        "code_index_progress",
        "file_list",
        "file_read",
        "file_search",
        "state_inspect",
        "web_search",
        "get_weather",
        "maps_geocode",
        "read_attachment",
        "search_this_session",
        "session_search",
        "similar_sessions",
        "terminal_read",
        "terminal_wait",
        "terminal_list",
        "v3_source_list",
        "v3_source_read",
        "dev_browser_url",
        "get_tunnel_url",
    ]
}

/// Instruction, policy, approval, write, and operational receipt results are absent.
fn perishable_read(name: &str) -> bool {
    eligible_tools().contains(&name)
}

fn state_mut(run: &mut Value) -> &mut Value {
    if !run.get(STATE_KEY).is_some_and(Value::is_object) {
        run[STATE_KEY] = json!({"clock":0,"entries":[]});
    }
    let state = &mut run[STATE_KEY];
    if !state["clock"].is_u64() {
        state["clock"] = json!(0);
    }
    if !state["entries"].is_array() {
        state["entries"] = json!([]);
    }
    state
}

fn state_ref(run: &Value) -> (&[Value], u64) {
    let entries = run
        .get(STATE_KEY)
        .and_then(|v| v.get("entries"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let clock = run
        .get(STATE_KEY)
        .and_then(|v| v.get("clock"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    (entries, clock)
}

fn call_id(item: &Value) -> Option<&str> {
    item.get("call_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 128)
}

fn rid(run: &Value) -> &str {
    run.get("id")
        .or_else(|| run.get("run_id"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
}

fn output_text(output: &Value) -> String {
    output
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| serde_json::to_string(output).unwrap_or_default())
}

fn successful_output(output: &Value) -> bool {
    let text = output_text(output);
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    matches!(
        value.get("status").and_then(Value::as_str),
        Some("success" | "ok")
    )
}

fn call_arguments(call: &Value) -> Value {
    match call.get("arguments") {
        Some(Value::Object(_)) => call["arguments"].clone(),
        Some(Value::String(text)) => serde_json::from_str(text).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

fn source_location(name: &str, args: &Value) -> (String, String) {
    let default_root = if name.starts_with("code_index_") {
        "webagent-v3"
    } else {
        ""
    };
    let root = args
        .get("root")
        .and_then(Value::as_str)
        .unwrap_or(default_root)
        .to_owned();
    let path = ["file_path", "path", "scope"]
        .iter()
        .find_map(|key| args.get(*key).and_then(Value::as_str))
        .unwrap_or("")
        .to_owned();
    (root, path)
}

/// Canonical workspace-relative identity for exact read/edit correlation.
/// Reject traversal and absolute forms instead of trying to repair them.
fn workspace_relative(root: &str, path: &str) -> Option<String> {
    if path.is_empty()
        || root.starts_with('/')
        || path.starts_with('/')
        || root.contains('\\')
        || path.contains('\\')
    {
        return None;
    }
    let mut components = Vec::new();
    for value in [root, path] {
        if value.is_empty() {
            continue;
        }
        for component in value.split('/') {
            if component.is_empty()
                || component == "."
                || component == ".."
                || component.contains(':')
            {
                return None;
            }
            components.push(component);
        }
    }
    (!components.is_empty()).then(|| components.join("/"))
}

fn hash_output(output: &Value) -> String {
    hex::encode(Sha256::digest(output_text(output).as_bytes()))
}

/// Recognize only the provider-compaction receipt shape. Its digest must match
/// an existing ledger entry before it can stand in for the original output.
fn compacted_output_matches(output: &Value, call_id: &str, expected_sha256: &str) -> bool {
    let Ok(receipt) = serde_json::from_str::<Value>(&output_text(output)) else {
        return false;
    };
    receipt["output"]["context_truncated"] == true
        && receipt["output"]["call_id"].as_str() == Some(call_id)
        && receipt["output"]["sha256"].as_str() == Some(expected_sha256)
        && expected_sha256.len() == 64
        && expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_compacted_preview(output: &Value, call_id: &str) -> bool {
    let Ok(receipt) = serde_json::from_str::<Value>(&output_text(output)) else {
        return false;
    };
    receipt["output"]["context_truncated"] == true
        && receipt["output"]["call_id"].as_str() == Some(call_id)
        && receipt["output"]["sha256"].as_str().is_some_and(|digest| {
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

fn entry_key(entry: &Value, run_id: &str, id: &str) -> bool {
    entry["origin_run_id"].as_str() == Some(run_id) && entry["call_id"].as_str() == Some(id)
}

/// Register a paired successful read result; only a digest and bounded metadata are stored.
pub fn register(run: &mut Value, call: &Value, output: &Value, policy: &Policy) {
    let Some(id) = call_id(call).map(str::to_owned) else {
        return;
    };
    let Some(name) = call.get("name").and_then(Value::as_str) else {
        return;
    };
    if !policy.eligible(name) || !successful_output(output) {
        return;
    }
    let run_id = rid(run).to_owned();
    let (root, path) = source_location(name, &call_arguments(call));
    let hash = hash_output(output);
    let state = state_mut(run);
    let clock = state["clock"].as_u64().unwrap_or(0);
    // `state_mut` always initializes the entries array.
    let Some(entries) = state["entries"].as_array_mut() else {
        return;
    };
    if entries.iter().any(|entry| entry_key(entry, &run_id, &id)) {
        return;
    }
    if entries.len() >= MAX_ENTRIES && !entries.iter().any(|entry| entry["promoted"] != true) {
        return;
    }
    entries.push(json!({
        "call_id":id, "origin_run_id":run_id, "tool_name":name,
        "output_sha256":hash, "output_chars":output_text(output).chars().count(),
        "root":root, "path":path,
        "first_seen":clock, "last_used":clock,
        "promoted":false, "reason":""
    }));
    while entries.len() > MAX_ENTRIES {
        let victim = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry["promoted"] != true)
            .min_by_key(|(_, entry)| {
                (
                    entry["last_used"].as_u64().unwrap_or(0),
                    entry["first_seen"].as_u64().unwrap_or(0),
                )
            })
            .map(|(index, _)| index);
        let Some(victim) = victim else {
            entries.pop();
            break;
        };
        entries.remove(victim);
    }
}

/// Record a successful current tool result while its pending call is still present.
pub fn record(run: &mut Value, result: &Value) -> Result<usize> {
    let policy = run
        .get(POLICY_KEY)
        .and_then(|value| serde_json::from_value::<Policy>(value.clone()).ok())
        .unwrap_or_default();
    let pending = run.get("pending").cloned().unwrap_or(Value::Null);
    let call = json!({
        "type":"function_call",
        "call_id":pending["call_id"],
        "name":pending["name"],
        "arguments":pending["arguments"]
    });
    record_dedup_origin(run, &call, result)?;
    let before = state_ref(run).0.len();
    register(run, &call, result, &policy);
    let name = pending["name"].as_str().unwrap_or("");
    let args = pending.get("arguments").cloned().unwrap_or(Value::Null);
    let promoted = if policy.enabled {
        promote_exact_edit(
            run,
            &policy,
            name,
            &args,
            successful_output(result),
            "related source was edited successfully",
        )
    } else {
        0
    };
    Ok(state_ref(run).0.len().saturating_sub(before) + promoted)
}

/// Reconcile existing input pairs. Repeated calls are idempotent.
pub fn synchronize(run: &mut Value, policy: &Policy) {
    let input = run
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let calls: HashMap<String, Value> = input
        .iter()
        .filter(|item| item["type"] == "function_call")
        .filter_map(|item| call_id(item).map(|id| (id.to_owned(), item.clone())))
        .collect();
    for output in input
        .iter()
        .filter(|item| item["type"] == "function_call_output")
    {
        let Some(id) = call_id(output) else {
            continue;
        };
        if let Some(call) = calls.get(id) {
            let tool_name = call["name"].as_str().unwrap_or("");
            let existing_origin = state_ref(run)
                .0
                .iter()
                .any(|entry| entry["call_id"] == id && entry["tool_name"] == tool_name);
            let compacted = is_compacted_preview(&output["output"], id);
            let output_hash = hash_output(&output["output"]);
            let same_digest = state_ref(run).0.iter().any(|entry| {
                entry["call_id"] == id
                    && entry["tool_name"] == tool_name
                    && entry["output_sha256"] == output_hash
            });
            if !same_digest && !compacted && !existing_origin {
                register(run, call, &output["output"], policy);
            }
        }
    }
}

/// Advance once immediately before a real model dispatch.
pub fn advance(run: &mut Value) -> Result<()> {
    let state = state_mut(run);
    let next = state["clock"]
        .as_u64()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("Tool context clock overflow"))?;
    state["clock"] = json!(next);
    Ok(())
}

/// Project history without mutating the durable run input. Newest successful
/// outputs consume separate ordinary and promoted budgets first.
pub fn project(run: &Value, policy: &Policy, placeholder: &str) -> Result<Option<Vec<Value>>> {
    policy.validate()?;
    if !policy.enabled {
        return Ok(None);
    }
    let Some(input) = run.get("input").and_then(Value::as_array) else {
        return Ok(None);
    };
    let (entries, clock) = state_ref(run);
    let mut calls: HashMap<&str, Vec<&Value>> = HashMap::new();
    let mut output_counts: HashMap<&str, usize> = HashMap::new();
    for call in input.iter().filter(|item| item["type"] == "function_call") {
        if let Some(id) = call_id(call) {
            calls.entry(id).or_default().push(call);
        }
    }
    for output in input
        .iter()
        .filter(|item| item["type"] == "function_call_output")
    {
        if let Some(id) = call_id(output) {
            *output_counts.entry(id).or_default() += 1;
        }
    }
    let mut candidates = Vec::new();
    for (index, item) in input.iter().enumerate() {
        if item["type"] != "function_call_output" {
            continue;
        }
        let Some(id) = call_id(item) else {
            continue;
        };
        let Some(paired_calls) = calls.get(id) else {
            continue;
        };
        if paired_calls.len() != 1 || output_counts.get(id) != Some(&1) {
            continue;
        }
        let call = paired_calls[0];
        let Some(name) = call["name"].as_str() else {
            continue;
        };
        if !policy.eligible(name) || !successful_output(&item["output"]) {
            continue;
        }
        let digest = hash_output(&item["output"]);
        let mut matches: Vec<_> = entries
            .iter()
            .filter(|entry| {
                entry["call_id"] == id
                    && entry["tool_name"] == name
                    && (entry["output_sha256"] == digest
                        || entry["output_sha256"].as_str().is_some_and(|expected| {
                            compacted_output_matches(&item["output"], id, expected)
                        }))
            })
            .collect();
        if matches.len() == 1 {
            candidates.push((index, item, matches.remove(0)));
        }
    }
    candidates.sort_by_key(|(_, _, entry)| {
        let used = if policy.refresh_on_recall {
            entry["last_used"].as_u64().unwrap_or(0)
        } else {
            entry["first_seen"].as_u64().unwrap_or(0)
        };
        (
            std::cmp::Reverse(used),
            std::cmp::Reverse(entry["first_seen"].as_u64().unwrap_or(0)),
        )
    });
    let mut ordinary_used = 0usize;
    let mut promoted_used = 0usize;
    let mut keep = HashSet::new();
    for (index, item, entry) in &candidates {
        let age_origin = if policy.refresh_on_recall {
            entry["last_used"].as_u64().unwrap_or(0)
        } else {
            entry["first_seen"].as_u64().unwrap_or(0)
        };
        if entry["promoted"] != true && clock.saturating_sub(age_origin) >= policy.lifetime_calls {
            continue;
        }
        let chars = entry["output_chars"]
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or_else(|| output_text(&item["output"]).chars().count());
        let (used, cap) = if entry["promoted"] == true {
            (&mut promoted_used, policy.promoted_chars)
        } else {
            (&mut ordinary_used, policy.ordinary_chars)
        };
        if chars <= cap.saturating_sub(*used) {
            *used += chars;
            keep.insert(*index);
        }
    }
    if candidates.is_empty() {
        return Ok(None);
    }
    let mut projected = input.to_vec();
    let mut changed = false;
    for (index, item, entry) in candidates {
        if keep.contains(&index) {
            if entry["requires_reread"] == true || entry["source_changed"] == true {
                let warning = json!({
                    "status":"source_changed",
                    "output":item["output"],
                    "warning":"This retained result describes source before a successful edit. Reread the current source before relying on it.",
                    "requires_reread":true,
                    "result_id":entry["call_id"],
                    "tool":entry["tool_name"],
                    "location":entry["path"]
                }).to_string();
                projected[index]["output"] = json!(warning);
                changed = true;
            }
            continue;
        }
        let call = entry["call_id"].as_str().unwrap_or("");
        let tool = entry["tool_name"].as_str().unwrap_or("");
        let origin = entry["origin_run_id"].as_str().unwrap_or("");
        let digest = entry["output_sha256"].as_str().unwrap_or("");
        let location = if entry["root"].as_str().unwrap_or("").is_empty() {
            entry["path"].as_str().unwrap_or("").to_owned()
        } else {
            format!(
                "{}/{}",
                entry["root"].as_str().unwrap_or(""),
                entry["path"].as_str().unwrap_or("")
            )
        };
        let rendered = placeholder
            .replace("{result_id}", call)
            .replace("{tool}", tool)
            .replace("{location}", &location)
            .replace("{{call_id}}", call)
            .replace("{{tool_name}}", tool)
            .replace("{{origin_run_id}}", origin)
            .replace("{{source_sha256}}", digest);
        let rendered: String = rendered.chars().take(MAX_PLACEHOLDER_CHARS).collect();
        projected[index]["output"] = json!(json!({
            "status":"retained",
            "output":rendered,
            "result_id":call,
            "tool":tool,
            "location":location,
            "source_sha256":digest,
            "requires_reread":entry["requires_reread"] == true || entry["source_changed"] == true,
            "warning":if entry["requires_reread"] == true || entry["source_changed"] == true {
                "The source changed after this result. Reread it before relying on prior details."
            } else { "" },
            "recall_reference":{"tool":"recall_compacted","tool_call_id":call}
        })
        .to_string());
        changed = true;
    }
    Ok(changed.then_some(projected))
}

/// Resolve a retained output's origin. An active pair must match the ledger
/// name and digest; after compaction removes both items, a unique eligible ledger
/// entry remains the only permitted source of origin identity.
pub fn origin_run_id<'a>(run: &'a Value, id: &str) -> Option<&'a str> {
    if let Some(origin) = run["tool_result_projection"]["origins"].get(id) {
        return origin["run_id"].as_str();
    }
    let input = run.get("input")?.as_array()?;
    let calls: Vec<_> = input
        .iter()
        .filter(|item| item["type"] == "function_call" && call_id(item) == Some(id))
        .collect();
    let outputs: Vec<_> = input
        .iter()
        .filter(|item| item["type"] == "function_call_output" && call_id(item) == Some(id))
        .collect();
    let entries: Vec<_> = state_ref(run)
        .0
        .iter()
        .filter(|entry| entry["call_id"] == id)
        .collect();
    if entries.len() != 1 {
        return None;
    }
    let entry = entries[0];
    let name = entry["tool_name"].as_str()?;
    let origin = entry["origin_run_id"].as_str()?;
    if !perishable_read(name) || origin.is_empty() {
        return None;
    }
    if calls.is_empty() && outputs.is_empty() {
        return Some(origin);
    }
    if calls.len() != 1
        || outputs.len() != 1
        || calls[0]["name"].as_str() != Some(name)
        || !successful_output(&outputs[0]["output"])
    {
        return None;
    }
    let digest = hash_output(&outputs[0]["output"]);
    let matches = entry["output_sha256"] == digest
        || entry["output_sha256"]
            .as_str()
            .is_some_and(|expected| compacted_output_matches(&outputs[0]["output"], id, expected));
    matches.then_some(origin)
}

/// Verify an archived result against the unique source digest retained in the
/// current run's metadata ledger before exposing any page of that result.
pub fn verify_origin_output(run: &Value, id: &str, name: &str, raw: &str) -> bool {
    if let Some(origin) = run["tool_result_projection"]["origins"].get(id) {
        return origin["tool"] == name
            && origin["sha256"] == webagent_context_contracts::tool_results::fingerprint(raw);
    }
    if !perishable_read(name) {
        return false;
    }
    let entries: Vec<_> = state_ref(run)
        .0
        .iter()
        .filter(|entry| entry["call_id"] == id)
        .collect();
    if entries.len() != 1 {
        return false;
    }
    let digest = hex::encode(Sha256::digest(raw.as_bytes()));
    entries[0]["tool_name"].as_str() == Some(name)
        && entries[0]["output_sha256"].as_str() == Some(digest.as_str())
}

/// Touch a retained result after a confirmed recall from its origin run.
pub fn touch(run: &mut Value, origin_run_id: &str, id: &str) -> Result<()> {
    let state = state_mut(run);
    let clock = state["clock"].as_u64().unwrap_or(0);
    let entries = state["entries"]
        .as_array_mut()
        .ok_or_else(|| anyhow!("Tool output retention state invalid"))?;
    let Some(entry) = entries
        .iter_mut()
        .find(|entry| entry_key(entry, origin_run_id, id))
    else {
        bail!("Unknown retained tool output");
    };
    entry["last_used"] = json!(clock);
    Ok(())
}

/// Promote only for an explicitly successful edit with exactly matching root and path.
pub fn promote_exact_edit(
    run: &mut Value,
    policy: &Policy,
    name: &str,
    args: &Value,
    success: bool,
    reason: &str,
) -> usize {
    if !policy.enabled
        || !policy.promote_on_edit
        || !success
        || !matches!(name, "file_write" | "file_edit")
        || reason.is_empty()
        || reason.chars().count() > MAX_REASON_CHARS
    {
        return 0;
    }
    let root = args.get("root").and_then(Value::as_str).unwrap_or("");
    let Some(path) = ["file_path", "path"]
        .iter()
        .find_map(|key| args.get(*key).and_then(Value::as_str))
    else {
        return 0;
    };
    let Some(edit_identity) = workspace_relative(root, path) else {
        return 0;
    };
    let state = state_mut(run);
    let Some(entries) = state["entries"].as_array_mut() else {
        return 0;
    };
    let mut count = 0;
    for entry in entries {
        let source_identity = workspace_relative(
            entry["root"].as_str().unwrap_or(""),
            entry["path"].as_str().unwrap_or(""),
        );
        if source_identity.as_deref() == Some(edit_identity.as_str()) {
            entry["promoted"] = json!(true);
            entry["reason"] = json!(reason);
            entry["source_changed"] = json!(true);
            entry["requires_reread"] = json!(true);
            count += 1;
        }
    }
    count
}

/// Native control changes only retention metadata; it never authorizes a tool.
pub fn control(run: &mut Value, call: &Value, enabled: bool) -> Value {
    if !enabled {
        return json!({"status":"denied","output":"Tool context control is disabled"});
    }
    let args = call_arguments(call);
    let action = args["action"].as_str().unwrap_or("");
    let ids = args["call_ids"].as_array().cloned().unwrap_or_default();
    let reason = args["reason"].as_str().unwrap_or("");
    if !matches!(action, "promote" | "release" | "touch")
        || ids.is_empty()
        || ids.len() > 16
        || reason.chars().count() > MAX_REASON_CHARS
        || (action == "promote" && reason.is_empty())
    {
        return json!({"status":"error","output":"Invalid tool context request"});
    }
    let mut names = HashSet::new();
    let mut parsed = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(id) = id.as_str().filter(|s| !s.is_empty() && s.len() <= 128) else {
            return json!({"status":"error","output":"Invalid tool context call ID"});
        };
        if !names.insert(id.to_owned()) {
            return json!({"status":"error","output":"Duplicate tool context call ID"});
        }
        parsed.push(id.to_owned());
    }
    let state = state_mut(run);
    let clock = state["clock"].as_u64().unwrap_or(0);
    let Some(entries) = state["entries"].as_array_mut() else {
        return json!({"status":"error","output":"Tool context ledger unavailable"});
    };
    if parsed.iter().any(|id| {
        entries
            .iter()
            .filter(|entry| entry["call_id"] == *id)
            .count()
            != 1
    }) {
        return json!({"status":"error","output":"Unknown or ambiguous tool context call ID"});
    }
    for id in &parsed {
        if let Some(entry) = entries.iter_mut().find(|entry| entry["call_id"] == *id) {
            match action {
                "promote" => {
                    entry["promoted"] = json!(true);
                    entry["reason"] = json!(reason);
                    entry["last_used"] = json!(clock);
                }
                "release" => {
                    entry["promoted"] = json!(false);
                    entry["reason"] = json!("");
                }
                "touch" => entry["last_used"] = json!(clock),
                _ => unreachable!(),
            }
        }
    }
    json!({"status":"ok","output":format!("Updated {} retained tool output(s)",parsed.len())})
}

pub fn schema() -> Value {
    json!({
        "type":"function",
        "name":"manage_tool_context",
        "description":"Promote, release, or refresh selected retained tool outputs for this session. This changes context retention only.",
        "parameters":{
            "type":"object",
            "properties":{
                "action":{"type":"string","enum":["promote","release","touch"]},
                "call_ids":{"type":"array","items":{"type":"string","maxLength":128},"minItems":1,"maxItems":16},
                "reason":{"type":"string","maxLength":256}
            },
            "required":["action","call_ids"],
            "additionalProperties":false
        }
    })
}

// Exact duplicate suppression is independent of the disabled experimental lifetime policy.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct DedupPolicy {
    pub enabled: bool,
}
impl Default for DedupPolicy {
    fn default() -> Self {
        Self { enabled: true }
    }
}

pub const DEDUP_DEFAULT_TOOLS: &[&str] = &[
    "file_read",
    "file_list",
    "file_search",
    "v3_source_read",
    "v3_source_list",
    "code_index_lookup",
    "code_index_coverage",
    "code_index_trace",
    "code_index_progress",
];

pub fn dedup_included(settings: &Value, name: &str) -> bool {
    settings["tool_result_deduplication"]
        .get(name)
        .and_then(Value::as_bool)
        .unwrap_or_else(|| DEDUP_DEFAULT_TOOLS.contains(&name))
}

pub async fn dedup_policy<S: crate::ports::AgentMetadata>(
    store: &S,
    agent: &str,
) -> Result<DedupPolicy> {
    let scoped = store.for_agent(agent).await?;
    let raw: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
        .bind(agent)
        .fetch_one(crate::ports::AgentMetadata::agent_pool(&scoped))
        .await?;
    let metadata = scoped.unseal(&raw)?;
    match metadata.get("tool_result_deduplication") {
        Some(value) => {
            // Older releases mixed per-tool overrides into this policy object.
            // The request DTO remains strict; persisted policy reads select only its enabled field.
            let fields = value
                .as_object()
                .context("Invalid duplicate-result agent policy")?;
            Ok(serde_json::from_value(
                json!({"enabled":fields.get("enabled").cloned().unwrap_or(json!(true))}),
            )?)
        }
        None => Ok(DedupPolicy::default()),
    }
}

/// Read the live placeholder file (`prompts/tool_result_deduplication/placeholder.md`); it has no default.
pub fn dedup_template(root: &Path) -> Result<String> {
    let prompts = crate::defaults::resolved_prompts(root)?;
    let text = prompts["tool_result_deduplication"]["placeholder"]
        .as_str()
        .context("prompts: tool_result_deduplication/placeholder.md is required")?;
    webagent_context_contracts::tool_results::validate_template(text)?;
    Ok(text.to_owned())
}

fn dedup_scope(run: &Value) -> Value {
    json!({"session":run["session_id"],"agent":run["agent_id"],
        "requester":run["requester_user_id"],"workspace":run["workspace_id"]})
}

/// Capture immutable provenance before native tool completion clears its pending identity.
fn record_dedup_origin(run: &mut Value, call: &Value, output: &Value) -> Result<()> {
    let Some(id) = call_id(call) else {
        return Ok(());
    };
    let id = id.to_owned();
    // Native controls retain only call_id in pending state. Resolve their name
    // from the host-recorded model call before freezing provenance.
    let name = call["name"]
        .as_str()
        .filter(|name| !name.is_empty())
        .or_else(|| {
            run["input"]
                .as_array()
                .and_then(|input| {
                    input.iter().find(|item| {
                        item["type"] == "function_call" && call_id(item) == Some(id.as_str())
                    })
                })
                .and_then(|item| item["name"].as_str())
                .filter(|name| !name.is_empty())
        });
    let Some(name) = name else {
        return Ok(());
    };
    let origin = json!({"run_id":rid(run),"tool":name,"scope":dedup_scope(run),"sha256":webagent_context_contracts::tool_results::fingerprint(&output.to_string()),"previewed":false});
    if !run["tool_result_projection"].is_object() {
        run["tool_result_projection"] = json!({});
    }
    if !run["tool_result_projection"]["origins"].is_object() {
        run["tool_result_projection"]["origins"] = json!({});
    }
    let origins = run["tool_result_projection"]["origins"]
        .as_object_mut()
        .context("Projection origins missing")?;
    if let Some(existing) = origins.get(&id) {
        if existing != &origin {
            bail!("Tool-result provenance changed");
        }
    } else {
        origins.insert(id, origin);
    }
    Ok(())
}

/// Build model-only replacements from journal originals. Every reconstructed origin stays session scoped.

/// Preserve component-withdrawal recovery while keeping configuration/reconstruction errors actionable.

/// Expand model-only references for summarization; callers retain the original verified folding plan.

pub async fn dedup_prepare(
    run: &mut Value,
    preview_bytes: usize,
    root: &Path,
    policy: DedupPolicy,
    template: Option<String>,
    settings: &impl crate::ports::DedupSettings,
    journal: &impl crate::ports::Journal,
    planner: &impl crate::ports::Planner,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(Vec<Value>, bool)> {
    use webagent_context_contracts::tool_results as contract;
    let sid = run["session_id"]
        .as_str()
        .context("Projection session missing")?
        .to_owned();
    let agent = run["agent_id"]
        .as_str()
        .unwrap_or("shared_default")
        .to_owned();
    if !run["tool_result_projection"].is_object() {
        let session = journal.document(&format!("session:{sid}")).await?;
        run["tool_result_projection"] = session
            .as_ref()
            .and_then(|s| s.get("tool_result_projection"))
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
    }
    let before = run["tool_result_projection"].clone();
    let mut document = before.clone();
    if !document["origins"].is_object() {
        document["origins"] = json!({});
    }
    let seed = document["initialized"] != true;
    let state: contract::State = match document.get("planner") {
        Some(v) => serde_json::from_value(v.clone())?,
        None => contract::State::default(),
    };
    let rebase = document["rebase"] == true;
    let settings = settings.dedup_settings(&sid).await?;
    let input = run["input"]
        .as_array()
        .context("Projection input missing")?
        .clone();
    let calls: HashMap<_, _> = input
        .iter()
        .filter(|item| item["type"] == "function_call")
        .filter_map(|item| call_id(item).map(|id| (id.to_owned(), item)))
        .collect();
    if calls.len()
        != input
            .iter()
            .filter(|item| item["type"] == "function_call")
            .count()
    {
        bail!("Ambiguous tool-result call identity");
    }
    let mut observations = Vec::new();
    let mut scopes: HashMap<String, Value> = HashMap::new();
    let mut originals = HashMap::new();
    for item in input
        .iter()
        .filter(|item| item["type"] == "function_call_output")
    {
        let Some(id) = call_id(item) else {
            continue;
        };
        let Some(call) = calls.get(id) else {
            continue;
        };
        let Some(name) = call["name"].as_str() else {
            continue;
        };
        let origin = document["origins"].get(id).cloned();
        let (source_run, raw) = if let Some(origin) = &origin {
            let source = origin["run_id"]
                .as_str()
                .context("Projection origin run missing")?;
            if origin["sha256"] == hash_output(&item["output"]) {
                (source.to_owned(), output_text(&item["output"]))
            } else {
                let result = journal
                    .tool_result(&sid, source, id)
                    .await?
                    .context("Original tool output unavailable")?;
                (
                    source.to_owned(),
                    result["output"]
                        .as_str()
                        .context("Original result text missing")?
                        .to_owned(),
                )
            }
        } else if let Some((source, result)) = journal.tool_result_origin(&sid, id).await? {
            (
                source,
                result["output"]
                    .as_str()
                    .context("Original result text missing")?
                    .to_owned(),
            )
        } else {
            if is_compacted_preview(&item["output"], id) {
                bail!("Original tool output unavailable for preview reconstruction");
            }
            (rid(run).to_owned(), output_text(&item["output"]))
        };
        let source_scope = if source_run == rid(run) {
            dedup_scope(run)
        } else if let Some(scope) = scopes.get(&source_run) {
            scope.clone()
        } else {
            let source = journal.run(&source_run).await?;
            if source["session_id"] != sid
                || source["agent_id"].as_str().unwrap_or("shared_default") != agent
            {
                bail!("Tool-result origin is outside this conversation");
            }
            let scope = dedup_scope(&source);
            scopes.insert(source_run.clone(), scope.clone());
            scope
        };
        let sha = contract::fingerprint(&raw);
        let scope = origin
            .as_ref()
            .map(|value| value["scope"].clone())
            .unwrap_or(source_scope);
        if let Some(origin) = &origin {
            if origin["sha256"] != sha {
                bail!("Original tool output failed provenance verification");
            }
            if origin["tool"] != name {
                // Recover previously saved native-control origins only from the
                // authorized immutable call journal; mismatched names stay fatal.
                let recorded = if origin["tool"].is_null() {
                    journal.tool_call_record(&sid, &source_run, id).await?
                } else {
                    None
                };
                if !recorded.is_some_and(|call| call["call_id"] == id && call["name"] == name) {
                    bail!("Original tool output failed provenance verification");
                }
            }
        }
        let old_preview = origin
            .as_ref()
            .is_some_and(|value| value["previewed"] == true)
            || (seed && output_text(&item["output"]) != raw);
        document["origins"][id] = json!({"run_id":source_run,"tool":name,"scope":scope,"sha256":sha,"previewed":old_preview});
        observations.push(contract::Observation {
            call_id: id.to_owned(),
            tool: name.to_owned(),
            inputs: call_arguments(call),
            scope,
            success: successful_output(&json!(raw)),
            output: raw.clone(),
            included: dedup_included(&settings, name) && !old_preview,
        });
        originals.insert(id.to_owned(), raw);
    }
    // Output completion may be asynchronous; canonical model-call order owns first/full selection.
    let call_order: HashMap<_, _> = input
        .iter()
        .enumerate()
        .filter(|(_, item)| item["type"] == "function_call")
        .filter_map(|(position, item)| call_id(item).map(|id| (id.to_owned(), position)))
        .collect();
    observations.sort_by_key(|item| call_order[&item.call_id]);
    let request = contract::Request {
        version: contract::VERSION,
        observations,
        state,
        enabled: policy.enabled && !seed,
        template: template.clone(),
        rebase,
    };
    let plan = planner
        .tool_results_plan(root, run, &request, cancel)
        .await?;
    contract::validate_plan(&request, &plan)?;
    let mut projected = input;
    for item in projected
        .iter_mut()
        .filter(|item| item["type"] == "function_call_output")
    {
        let Some(id) = call_id(item).map(str::to_owned) else {
            continue;
        };
        if let Some(text) = plan.replacements.get(&id) {
            item["output"] = json!(text);
            continue;
        }
        let controlled = plan
            .state
            .assignments
            .get(&id)
            .is_some_and(|entry| entry.template.is_some());
        if controlled {
            item["output"] = json!(originals
                .get(&id)
                .context("Full primary output unavailable")?);
        } else {
            crate::compaction::project_tool_results(std::slice::from_mut(item), preview_bytes)?;
        }
    }
    document["config_fingerprint"] = json!(crate::digest(&json!([
        policy,
        template,
        settings["tool_result_deduplication"]
    ])));
    document["planner"] = serde_json::to_value(&plan.state)?;
    document["initialized"] = json!(true);
    document["rebase"] = json!(false);
    if rebase {
        let present: HashSet<_> = request
            .observations
            .iter()
            .map(|item| item.call_id.as_str())
            .collect();
        document["origins"]
            .as_object_mut()
            .context("Projection origins missing")?
            .retain(|id, _| present.contains(id.as_str()));
    }
    let changed = document != before;
    if changed {
        document["revision"] = json!(before["revision"]
            .as_u64()
            .unwrap_or(0)
            .checked_add(1)
            .context("Projection revision overflow")?);
        run["tool_result_projection"] = document;
    }
    Ok((projected, changed))
}
pub async fn dedup_summary_source(
    journal: &impl crate::ports::Journal,
    run: &Value,
    source: &[Value],
) -> Result<Vec<Value>> {
    let sid = run["session_id"]
        .as_str()
        .context("Summary session missing")?;
    let mut full = source.to_vec();
    for item in &mut full {
        if item["type"] != "function_call_output" {
            continue;
        }
        let Some(id) = call_id(item) else {
            continue;
        };
        if let Some(origin) = run["tool_result_projection"]["origins"].get(id) {
            let origin = origin["run_id"]
                .as_str()
                .context("Summary origin missing")?;
            let result = journal
                .tool_result(sid, origin, id)
                .await?
                .context("Summary original unavailable")?;
            item["output"] = result["output"].clone();
        }
    }
    Ok(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy {
            enabled: true,
            scope: Scope::AllEligible,
            tools: vec![],
            lifetime_calls: 2,
            ordinary_chars: 1_024,
            promoted_chars: 1_024,
            refresh_on_recall: true,
            promote_on_edit: true,
        }
    }

    fn call(id: &str, name: &str, args: Value) -> Value {
        json!({"type":"function_call","call_id":id,"name":name,
            "arguments":args.to_string()})
    }

    fn output(id: &str, status: &str, value: &str) -> Value {
        json!({"type":"function_call_output","call_id":id,
            "output":json!({"status":status,"output":value}).to_string()})
    }

    fn run() -> Value {
        json!({"id":"run-1","input":[]})
    }

    #[test]
    fn policy_defaults_deny_and_reject_unknown_fields_and_bad_bounds() {
        assert!(!Policy::default().enabled);
        assert_eq!(Policy::default().lifetime_calls, 6);
        assert_eq!(Policy::default().ordinary_chars, 24_000);
        assert_eq!(CONTROL_NAME, "manage_tool_context");
        assert!(eligible_tools().contains(&"file_read"));
        assert!(!eligible_tools().contains(&"v3_worker_job"));
        assert!(Policy::default().refresh_on_recall);
        assert!(Policy::default().promote_on_edit);
        assert!(serde_json::from_value::<Policy>(json!({"enabled":true,"surprise":1})).is_err());
        let mut invalid = policy();
        invalid.lifetime_calls = MAX_LIFETIME_CALLS + 1;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn only_successful_eligible_reads_enter_bounded_metadata_ledger() {
        let p = policy();
        let mut r = run();
        register(
            &mut r,
            &call("c1", "file_read", json!({"path":"src/a.rs"})),
            &json!(json!({"status":"success","output":"result"}).to_string()),
            &p,
        );
        register(
            &mut r,
            &call("c2", "file_edit", json!({"path":"src/a.rs"})),
            &json!(json!({"status":"success","output":"result"}).to_string()),
            &p,
        );
        register(
            &mut r,
            &call("c3", "file_read", json!({})),
            &json!(json!({"status":"uncertain","output":"result"}).to_string()),
            &p,
        );
        assert_eq!(r[STATE_KEY]["entries"].as_array().unwrap().len(), 1);
        assert_eq!(
            r[STATE_KEY]["entries"][0]["output_sha256"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        assert!(r[STATE_KEY]["entries"][0].get("output").is_none());
    }

    #[test]
    fn projection_expires_at_exact_boundary_and_keeps_raw_history_unchanged() {
        let p = policy();
        let mut r = run();
        let c = call("read-1", "file_read", json!({"path":"src/a.rs"}));
        let o = output("read-1", "success", "é🙂");
        r["input"] = json!([c.clone(), o.clone()]);
        register(&mut r, &c, &o["output"], &p);
        let raw = r["input"].clone();
        assert!(project(&r, &p, "retained {{call_id}}").unwrap().is_none());
        advance(&mut r).unwrap();
        assert!(project(&r, &p, "retained {{call_id}}").unwrap().is_none());
        advance(&mut r).unwrap();
        let projected = project(&r, &p, "retained {{call_id}}").unwrap().unwrap();
        let replacement: Value =
            serde_json::from_str(projected[1]["output"].as_str().unwrap()).unwrap();
        assert_eq!(replacement["status"], "retained");
        assert_eq!(r["input"], raw);
        assert!(project(&r, &Policy::default(), "hidden").unwrap().is_none());
    }

    #[test]
    fn archived_origin_resolves_only_from_one_eligible_ledger_entry() {
        let raw = "archived output";
        let digest = hex::encode(Sha256::digest(raw.as_bytes()));
        let mut r = json!({"id":"later-run","input":[],"tool_context_state":{"entries":[{
            "call_id":"archived-1","origin_run_id":"source-run","tool_name":"file_read",
            "output_sha256":digest,"promoted":false
        }]}});
        assert_eq!(origin_run_id(&r, "archived-1"), Some("source-run"));
        assert!(verify_origin_output(&r, "archived-1", "file_read", raw));
        assert!(!verify_origin_output(
            &r,
            "archived-1",
            "file_read",
            "tampered"
        ));
        assert!(!verify_origin_output(&r, "archived-1", "web_search", raw));
        r[STATE_KEY]["entries"].as_array_mut().unwrap().push(json!({
            "call_id":"archived-1","origin_run_id":"other-run","tool_name":"file_read",
            "output_sha256":digest,"promoted":false
        }));
        assert_eq!(origin_run_id(&r, "archived-1"), None);
        assert!(!verify_origin_output(&r, "archived-1", "file_read", raw));
    }

    #[test]
    fn newest_first_budgets_and_promoted_budget_are_separate() {
        let mut p = policy();
        p.lifetime_calls = 20;
        p.ordinary_chars = 1_024;
        p.promoted_chars = 1_024;
        let mut r = run();
        let body = "x".repeat(700);
        let c1 = call("old", "file_read", json!({"path":"a"}));
        let o1 = output("old", "success", &body);
        let c2 = call("new", "file_read", json!({"path":"b"}));
        let o2 = output("new", "success", &body);
        r["input"] = json!([c1.clone(), o1.clone(), c2.clone(), o2.clone()]);
        register(&mut r, &c1, &o1["output"], &p);
        advance(&mut r).unwrap();
        register(&mut r, &c2, &o2["output"], &p);
        let projected = project(&r, &p, "retained {{call_id}}").unwrap().unwrap();
        let old_replacement: Value =
            serde_json::from_str(projected[1]["output"].as_str().unwrap()).unwrap();
        assert_eq!(old_replacement["status"], "retained");
        assert_eq!(projected[3]["output"], o2["output"]);
        let entries = r[STATE_KEY]["entries"].as_array_mut().unwrap();
        entries[0]["promoted"] = json!(true);
        entries[0]["source_changed"] = json!(true);
        entries[0]["requires_reread"] = json!(true);
        p.promoted_chars = 1_024;
        let promoted = project(&r, &p, "retained").unwrap().unwrap();
        let stale: Value = serde_json::from_str(promoted[1]["output"].as_str().unwrap()).unwrap();
        assert_eq!(stale["status"], "source_changed");
        assert_eq!(stale["requires_reread"], true);
        assert_eq!(promoted[3]["output"], o2["output"]);
    }

    #[test]
    fn nested_compaction_preview_uses_only_matching_digest_and_preserves_ledger() {
        let p = policy();
        let mut r = run();
        let c = call("large", "file_read", json!({"path":"src/a.rs"}));
        let full = output("large", "success", &"large-result".repeat(160));
        let full_text = full["output"].as_str().unwrap();
        let digest = hash_output(&full["output"]);
        let preview = json!({"status":"success","output":{
            "preview":"bounded prefix","context_truncated":true,
            "full_output_bytes":full_text.len(),"sha256":digest,
            "call_id":"large","recall":"full output retained"}})
        .to_string();
        let compacted = json!({"type":"function_call_output","call_id":"large","output":preview});
        r["input"] = json!([c.clone(), compacted.clone()]);
        register(&mut r, &c, &full["output"], &p);
        let entry_before = r[STATE_KEY]["entries"][0].clone();
        synchronize(&mut r, &p);
        assert_eq!(r[STATE_KEY]["entries"].as_array().unwrap().len(), 1);
        assert_eq!(r[STATE_KEY]["entries"][0], entry_before);
        assert_eq!(origin_run_id(&r, "large"), Some("run-1"));
        advance(&mut r).unwrap();
        advance(&mut r).unwrap();
        let raw = r["input"].clone();
        let projected = project(&r, &p, "retained {result_id}").unwrap().unwrap();
        let receipt: Value =
            serde_json::from_str(projected[1]["output"].as_str().unwrap()).unwrap();
        assert_eq!(receipt["status"], "retained");
        assert_eq!(r["input"], raw);
        assert_eq!(r[STATE_KEY]["entries"][0]["output_sha256"], digest);

        let mut tampered = r.clone();
        let mut bad: Value =
            serde_json::from_str(tampered["input"][1]["output"].as_str().unwrap()).unwrap();
        bad["output"]["sha256"] = json!("0".repeat(64));
        tampered["input"][1]["output"] = json!(bad.to_string());
        assert!(project(&tampered, &p, "retained").unwrap().is_none());
        assert_eq!(origin_run_id(&tampered, "large"), None);

        let mut ambiguous = r.clone();
        let pair = ambiguous["input"].as_array().unwrap().clone();
        ambiguous["input"] = json!([pair[0], pair[1], pair[0], pair[1]]);
        assert!(project(&ambiguous, &p, "retained").unwrap().is_none());
        assert_eq!(origin_run_id(&ambiguous, "large"), None);
    }

    #[test]
    fn unicode_budgets_count_characters_not_bytes() {
        let mut p = policy();
        p.lifetime_calls = 20;
        p.ordinary_chars = 1_024;
        let mut r = run();
        let c = call("unicode", "file_read", json!({"path":"x"}));
        let o = output("unicode", "success", &"é🙂".repeat(300));
        r["input"] = json!([c.clone(), o.clone()]);
        register(&mut r, &c, &o["output"], &p);
        assert!(project(&r, &p, "retained").unwrap().is_none());
    }

    #[test]
    fn touch_and_control_validate_all_ids_before_mutating() {
        let p = policy();
        let mut r = run();
        let c = call(
            "c1",
            "file_read",
            json!({"root":"repo","file_path":"src/a.rs"}),
        );
        let o = output("c1", "success", "small");
        r["input"] = json!([c.clone(), o.clone()]);
        register(&mut r, &c, &o["output"], &p);
        advance(&mut r).unwrap();
        let before = r[STATE_KEY]["entries"].clone();
        let bad = control(
            &mut r,
            &json!({"arguments":json!({"action":"promote","call_ids":["c1","missing"],"reason":"needed"}).to_string()}),
            true,
        );
        assert_eq!(bad["status"], "error");
        assert_eq!(r[STATE_KEY]["entries"], before);
        assert!(touch(&mut r, "run-2", "c1").is_err());
        assert!(touch(&mut r, "run-1", "c1").is_ok());
        let good = control(
            &mut r,
            &json!({"arguments":json!({"action":"promote","call_ids":["c1"],"reason":"keep"}).to_string()}),
            true,
        );
        assert_eq!(good["status"], "ok");
        assert_eq!(r[STATE_KEY]["entries"][0]["reason"], "keep");
    }

    #[test]
    fn automatic_promotion_requires_exact_root_and_path_and_success() {
        let p = policy();
        let mut r = run();
        let c = call(
            "c1",
            "code_index_lookup",
            json!({"root":"webagent-v3","file_path":"src/lib.rs"}),
        );
        let o = output("c1", "success", "small");
        register(&mut r, &c, &o["output"], &p);
        let args = json!({"root":"webagent-v3","path":"src/lib.rs"});
        assert_eq!(
            promote_exact_edit(&mut r, &p, "file_edit", &args, false, "edited"),
            0
        );
        assert_eq!(
            promote_exact_edit(
                &mut r,
                &p,
                "file_edit",
                &json!({"root":"other","path":"src/lib.rs"}),
                true,
                "edited"
            ),
            0
        );
        assert_eq!(
            promote_exact_edit(&mut r, &p, "file_edit", &args, true, "edited"),
            1
        );
        assert!(r[STATE_KEY]["entries"][0]["promoted"].as_bool().unwrap());

        let mut indexed = run();
        let indexed_call = call(
            "indexed",
            "code_index_lookup",
            json!({
            "root":"webagent-v3","file_path":"src/a.rs"}),
        );
        register(&mut indexed, &indexed_call, &o["output"], &p);
        let actual_file_write = json!({"file_path":"webagent-v3/src/a.rs"});
        assert_eq!(
            promote_exact_edit(
                &mut indexed,
                &p,
                "file_write",
                &actual_file_write,
                true,
                "edited"
            ),
            1
        );
        assert_eq!(indexed[STATE_KEY]["entries"][0]["requires_reread"], true);

        let mut wrong_root = run();
        register(&mut wrong_root, &indexed_call, &o["output"], &p);
        assert_eq!(
            promote_exact_edit(
                &mut wrong_root,
                &p,
                "file_write",
                &json!({"file_path":"other/src/a.rs"}),
                true,
                "edited"
            ),
            0
        );
        assert_eq!(
            promote_exact_edit(
                &mut wrong_root,
                &p,
                "file_write",
                &json!({"file_path":"src/a.rs"}),
                true,
                "edited"
            ),
            0
        );
        assert_eq!(
            promote_exact_edit(
                &mut wrong_root,
                &p,
                "file_write",
                &json!({"file_path":"webagent-v3/../other/src/a.rs"}),
                true,
                "edited"
            ),
            0
        );
    }

    #[test]
    fn raw_policy_parser_and_live_reader_share_field_bounds_and_byte_limits() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(LIVE_POLICY);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let roots = [root.path().to_owned()];
        let valid = br#"{"enabled":false}"#;
        assert_eq!(Policy::parse(valid).unwrap(), Policy::default());
        fs::write(&path, valid).unwrap();
        assert_eq!(
            Policy::read_in(&roots).unwrap(),
            Policy::parse(valid).unwrap()
        );
        let mut at_limit = vec![b' '; MAX_POLICY_BYTES - 2];
        at_limit.extend_from_slice(b"{}");
        assert!(Policy::parse(&at_limit).is_ok());
        let mut oversized = at_limit;
        oversized.push(b' ');
        assert!(serde_json::from_slice::<Value>(&oversized).is_ok());
        for bytes in [
            br#"{"enabled":false,"enabled":true}"#.to_vec(),
            br#"{"unexpected":"PRIVATE_POLICY_MARKER"}"#.to_vec(),
            br#"{"lifetime_calls":0}"#.to_vec(),
            br#"{"enabled":"false"}"#.to_vec(),
            oversized,
        ] {
            assert!(Policy::parse(&bytes).is_err());
            fs::write(&path, &bytes).unwrap();
            // The live reader serves the last good bytes once a file has validated.
            assert_eq!(
                Policy::read_in(&roots).unwrap(),
                Policy::parse(valid).unwrap()
            );
        }
    }

    #[test]
    fn synchronize_pairs_by_call_id_and_policy_read_missing_uses_disabled_defaults() {
        let p = policy();
        let mut r = run();
        let c = call("c1", "file_read", json!({}));
        r["input"] = json!([
            c,
            output("c1", "success", "ok"),
            output("orphan", "success", "x")
        ]);
        synchronize(&mut r, &p);
        assert_eq!(r[STATE_KEY]["entries"].as_array().unwrap().len(), 1);
        // The shipped live policy keeps retention disabled.
        assert_eq!(Policy::live().unwrap(), Policy::default());
    }

    #[tokio::test]
    async fn policy_reads_live_file_and_agent_override_is_complete() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("x").join(LIVE_POLICY);
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(
            &config,
            json!({"enabled":true,"scope":"selected","tools":["file_read"]}).to_string(),
        )
        .unwrap();
        let loaded = Policy::read_in(&[dir.path().join("x")]).unwrap();
        assert!(loaded.eligible("file_read"));
        assert!(!loaded.eligible("file_edit"));
    }

    #[test]
    fn prompts_fall_back_for_absent_section_and_reject_invalid_present_values() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = Prompts::read(dir.path()).unwrap();
        assert!(!prompts.guidance.is_empty());
        assert!(!prompts.placeholder.is_empty());
        assert_eq!(
            embedded_prompt_defaults(&json!({"tool_output_retention":{
                "guidance":"Configured guidance", "placeholder":"Configured placeholder"
            }}))
            .unwrap(),
            (
                "Configured guidance".into(),
                "Configured placeholder".into()
            )
        );
        assert!(embedded_prompt_defaults(&json!({})).is_err());
        assert!(
            embedded_prompt_defaults(&json!({"tool_output_retention":{"guidance":false}})).is_err()
        );
        // A candidate source whose guidance file is blank is rejected.
        let folder = dir
            .path()
            .join(webagent_configuration::prompts::PROMPTS)
            .join("tool_output_retention");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("guidance.md"), "  \n").unwrap();
        assert!(Prompts::read_at(dir.path(), Some(dir.path())).is_err());
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn fixture_record_dedup_origin(run: &mut Value, call: &Value, output: &Value) -> Result<()> {
    record_dedup_origin(run, call, output)
}
