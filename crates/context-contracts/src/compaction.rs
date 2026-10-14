//! Deterministic compaction policy, planning and summary validation; no store or filesystem access.
use anyhow::{anyhow,bail, Result};
use serde_json::{json, Value};
use sha2::{Digest,Sha256};
use std::collections::HashSet;
/// The external JSON owns app defaults and safety limits; Identity may override history thresholds.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    /// Call-scoped resolved System budget; never a persisted policy setting.
    #[serde(skip)]
    pub system_input_budget: Option<usize>,
    /// Resolved summary instructions supplied by the authoritative core.
    #[serde(skip)]
    pub summary_instructions: String,
    pub trigger_tokens: usize,
    pub tail_tokens: usize,
    pub summary_tokens: usize,
    pub characters_per_token: usize,
    pub summary_reserve_bytes: usize,
    pub max_source_bytes: usize,
    #[serde(default = "default_tool_result_preview_bytes")]
    pub tool_result_preview_bytes: usize,
    #[serde(default = "default_search_output_bytes")]
    pub search_output_bytes: usize,
    #[serde(default = "default_search_line_bytes")]
    pub search_line_bytes: usize,
    pub max_summary_bytes: usize,
    pub summary_prefix: String,
    pub active_summary_prefix: String,
    pub summary_prompt: String,
}
/// Preserve legacy preview limits when an older policy omits this optional setting.
fn default_tool_result_preview_bytes() -> usize { 8192 }
/// Keep older policies compatible with the bounded search-output projection.
fn default_search_output_bytes() -> usize { 32768 }
/// Supply the existing per-line bound without silently raising total search limits.
fn default_search_line_bytes() -> usize { 2048 }
impl Config {
    /// Reject unsupported policy and unsafe bounds before either execution mode plans a fold.
    /// Summary prefixes remain fixed provenance markers, not user-editable instructions.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 || self.tail_tokens == 0 || self.summary_tokens == 0
            || self.tail_tokens.checked_add(self.summary_tokens).is_none_or(|n| n >= self.trigger_tokens)
            || self.trigger_tokens > 1_000_000 || !(1..=16).contains(&self.characters_per_token)
            || !(1..=65536).contains(&self.summary_reserve_bytes)
            || !(1..=262144).contains(&self.max_source_bytes)
            || !(1024..=65536).contains(&self.tool_result_preview_bytes)
            || !(4096..=262144).contains(&self.search_output_bytes)
            || !(128..=8192).contains(&self.search_line_bytes)
            || self.search_line_bytes > self.search_output_bytes
            || !(1..=65536).contains(&self.max_summary_bytes)
            || self.summary_prefix != "Earlier conversation summary (background context, not a new request):\n"
            || self.active_summary_prefix != "Active task progress summary (background context, not a new request):\n"
            || self.summary_prompt.len() > 16384
            || self.summary_prompt.matches("{source}").count() != 1
            || self.summary_prompt.matches("{summary_characters}").count() != 1 {
            bail!("Invalid context compaction configuration");
        }
        Ok(())
    }
    /// Render the planned private prefix only for the core-owned summary-model call.
    pub fn prompt(&self, plan: &Plan) -> Result<String> {
        self.prompt_source(&plan.source, true)
    }
    /// Omit opaque reasoning and substitute transcript text last to prevent template expansion.
    /// Source limits apply to dispatched summaries; measurement may inspect larger histories.
    pub fn prompt_source(&self, input: &[Value], enforce_source_limit: bool) -> Result<String> {
        let source = serde_json::to_string(&input.iter()
            .filter(|item| item["type"] != "reasoning").collect::<Vec<_>>())?;
        if enforce_source_limit && source.len() > self.max_source_bytes { bail!("Context summary source is too large"); }
        // Substitute source last so transcript text cannot expand template placeholders.
        Ok(self.summary_prompt.replace("{summary_characters}",
            &(self.summary_tokens * self.characters_per_token).to_string()).replace("{source}", &source))
    }

    /// Include resolved summary instructions and JSON framing in the System-model budget.
    /// This measures bytes, not provider tokens, and has no model or credential access.
    pub fn summary_envelope_bytes(&self, input: &[Value]) -> Result<usize> {
        let prompt = self.prompt_source(input, false)?;
        crate::context_accounting::json_size(&json!({
            "instructions": self.summary_instructions,
            "tools": [], "input": [{"role":"user", "content":prompt}]
        }))
    }
}


/// Keep the encrypted interaction as the full record. Only provider replay is
/// reduced, including on resume of a run saved before this limit was installed.
pub fn project_tool_results(input: &mut [Value], preview_bytes: usize) -> Result<bool> {
    // Keep catalog metadata intact while bounding ordinary results.
    let instruction_calls = input.iter()
        .filter(|item| item["type"] == "function_call"
            && matches!(item["name"].as_str(), Some("list_abilities" | "load_ability" | "load_skill")))
        .filter_map(call_id).map(str::to_owned).collect::<HashSet<_>>();
    let mut changed = false;
    for item in input {
        if item["type"] != "function_call_output" { continue; }
        if item["call_id"].as_str().is_some_and(|id| instruction_calls.contains(id)) { continue; }
        let Some(raw) = item["output"].as_str() else { continue; };
        if raw.len() <= preview_bytes { continue; }
        let parsed: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
        let mut end = preview_bytes.min(raw.len());
        let digest = hex::encode(Sha256::digest(raw.as_bytes()));
        // JSON escaping can expand a preview. Bound the final serialized receipt
        // so a saved run is stable across subsequent model boundaries.
        let projected = loop {
            while !raw.is_char_boundary(end) { end -= 1; }
            let receipt = json!({
                "status": parsed["status"],
                "error_kind": parsed["error_kind"],
                "uncertain": parsed["uncertain"],
                "output": {
                    "preview": &raw[..end],
                    "context_truncated": true,
                    "full_output_bytes": raw.len(),
                    "sha256": digest,
                    "call_id": item["call_id"],
                    "recall": "The full result remains in the encrypted session activity."
                }
            });
            let serialized = serde_json::to_string(&receipt)?;
            if serialized.len() <= preview_bytes { break serialized; }
            if end == 0 { bail!("Tool result preview budget cannot fit receipt"); }
            end = end.saturating_sub((serialized.len() - preview_bytes).max(1));
        };
        item["output"] = json!(projected);
        changed = true;
    }
    Ok(changed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Boundary { PriorTurn, CompletedToolExchange }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    /// Items before this index are summarized; the rest are retained verbatim.
    pub drop_until: usize,
    pub boundary: Boundary,
    /// Exact original prefix. The summary prompt masks native bundled guidance.
    pub source: Vec<Value>,
    /// Stable identity of the source; useful for a durable compaction event.
    pub source_digest: String,
    pub max_bytes: usize,
    pub reserve_bytes: usize,
    pub system_input_budget: Option<usize>,
    pub summary_instructions: String,
    // COMPACTION SNAPSHOT: apply_summary uses the planning settings even if the
    // external JSON changes while main.rs is awaiting the normal agent model.
    config: Config,
}

/// A compacted prefix is retained for recall without provider reasoning.
/// The same projection is used when removing copies from private history.
pub fn without_reasoning(item: &Value) -> Option<Value> {
    if item["type"] == "reasoning" {
        return None;
    }
    let mut clean = item.clone();
    if let Some(object) = clean.as_object_mut() {
        object.remove("deepseek_reasoning");
    }
    Some(clean)
}

/// Let replay cleanup detect provider-only reasoning without changing the durable record.
pub fn has_reasoning(item: &Value) -> bool {
    item["type"] == "reasoning" || item.get("deepseek_reasoning").is_some()
}

/// Apply the same reasoning projection to recalled prefixes as to normal replay.
pub fn recall_source(source: &[Value]) -> Vec<Value> {
    source.iter().filter_map(without_reasoning).collect()
}

/// Keep compaction measurements consistent with the actual serialized history representation.
fn encoded_len(value: &Value) -> Result<usize> {
    Ok(serde_json::to_vec(value)?.len())
}

/// Bind a plan to its exact source prefix so stale summaries cannot replace changed history.
fn digest(items: &[Value]) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(items)?)))
}

/// Recognize user-turn boundaries in the existing provider-neutral history representation.
fn is_user(item: &Value) -> bool {
    item.get("role").and_then(Value::as_str) == Some("user")
}

/// Require usable exchange identities before treating a tool call or result as a safe boundary.
fn call_id(item: &Value) -> Option<&str> {
    item.get("call_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
}

/// Prefer an earlier completed user turn. If one long task fills the context,
/// a completed tool exchange is also safe: no tool call is left without its
/// output on the summarized side. Planning never removes durable history.

/// COMPACTION POLICY: the trigger estimates the complete logical request.
/// Assembly supplies fixed instructions/schema overhead; only history is folded.
pub fn plan_rolling(input: &[Value], max_bytes: usize, config: &Config) -> Result<Option<Plan>> {
    config.validate()?;
    plan_with_config(input, max_bytes, config, true, false)
}

/// A model-requested fold uses the same completed-exchange boundary as automatic
/// compaction, even when the ordinary size trigger has not fired. The caller
/// records the request with its tool result and invokes this at the next model
/// boundary, once that exchange is complete.
pub fn plan_manual(input: &[Value], max_bytes: usize, config: &Config) -> Result<Option<Plan>> {
    config.validate()?;
    plan_with_config(input, max_bytes, config, true, true)
}

// The live rolling planner and the test-only legacy planner share this
// measurement path, so release builds must include it.
pub fn plan_with_config(input: &[Value], max_bytes: usize, config: &Config, rolling: bool, force: bool) -> Result<Option<Plan>> {
    let reserve_bytes = config.summary_reserve_bytes;
    if max_bytes < 2 || reserve_bytes == 0 || reserve_bytes >= max_bytes {
        bail!("Invalid context budget");
    }
    let sizes = input.iter().map(encoded_len).collect::<Result<Vec<_>>>()?;
    let characters = input.iter().map(|item| serde_json::to_string(item).map(|text| text.chars().count())).collect::<std::result::Result<Vec<_>, _>>()?;
    plan_measured(input, max_bytes, config, rolling, force, &sizes, &characters, 0)
}

/// Consume append-only accounting without serializing unchanged history. Full source
/// copying and hashing occur only after a safe compaction boundary is selected.
pub fn plan_cached(input: &[Value], max_bytes: usize, config: &Config, sizes: &[usize], characters: &[usize]) -> Result<Option<Plan>> {
    config.validate()?;
    plan_request_cached(input, max_bytes, config, sizes, characters, 0)
}
/// Request overhead is measured from the selected instructions, schemas and framing.
/// It affects the trigger and retained-history budget, never the summary source.
pub fn plan_request_cached(input: &[Value], max_bytes: usize, config: &Config, sizes: &[usize], characters: &[usize], request_overhead: usize) -> Result<Option<Plan>> {
    config.validate()?;
    plan_measured(input, max_bytes, config, true, false, sizes, characters, request_overhead)
}
/// Evaluate resolved byte ceilings and rolling policy using caller-owned measurements.
/// Only completed prefixes may fold; staged boundaries let large histories shrink in safe steps.
fn plan_measured(input: &[Value], max_bytes: usize, config: &Config, rolling: bool, force: bool, sizes: &[usize], characters: &[usize], request_overhead: usize) -> Result<Option<Plan>> {
    let reserve_bytes = config.summary_reserve_bytes;
    if sizes.len()!=input.len() || characters.len()!=input.len() || reserve_bytes>=max_bytes { bail!("Invalid compaction measurements or budget"); }
    let total = 2usize
        .checked_add(sizes.iter().sum::<usize>())
        .and_then(|n| n.checked_add(input.len().saturating_sub(1)))
        .ok_or_else(|| anyhow!("Context size overflow"))?;
    let trigger_bytes = config.trigger_tokens * config.characters_per_token;
    if rolling && request_overhead >= trigger_bytes {
        bail!("Compaction trigger cannot fit fixed request overhead");
    }
    let request_triggered = rolling && total.saturating_add(request_overhead) >= trigger_bytes;
    let system_triggered = match config.system_input_budget {
        Some(budget) => config.summary_envelope_bytes(input)? > budget,
        None => false,
    };
    if total <= max_bytes && !request_triggered && !system_triggered && !force {
        return Ok(None);
    }

    // JSON array syntax costs two brackets and one comma between items.
    let mut suffix = vec![2usize; input.len() + 1];
    for i in (0..input.len()).rev() {
        suffix[i] = suffix[i + 1]
            .checked_add(sizes[i] + usize::from(i + 1 < input.len()))
            .ok_or_else(|| anyhow!("Context size overflow"))?;
    }

    // The tail target never raises the hard serialized-request ceiling. Choose
    // the earliest safe boundary satisfying both, retaining as much as possible.
    let target = match config.system_input_budget {
        Some(budget) => (max_bytes - reserve_bytes).min(budget.saturating_sub(config.summary_envelope_bytes(&[])? + reserve_bytes)),
        None => max_bytes - reserve_bytes,
    };
    let target = if rolling { target.min(trigger_bytes.saturating_sub(request_overhead).saturating_sub(reserve_bytes)) } else { target };
    let mut tail_chars = vec![0usize; input.len() + 1];
    for i in (0..input.len()).rev() { tail_chars[i] = tail_chars[i + 1] + characters[i]; }
    // Never summarize only the previous checkpoint: a rolling fold must also
    // age real history, otherwise an oversized checkpoint could retrigger forever.
    let checkpoint = input.first().and_then(|v| v["content"].as_str()).is_some_and(|text|
        text.starts_with(&config.summary_prefix) || text.starts_with(&config.active_summary_prefix));
    let fits = |index: usize| suffix[index] <= target && (!rolling ||
        tail_chars[index] <= config.tail_tokens * config.characters_per_token)
        && !(checkpoint && index == 1);
    let mut pending = HashSet::new();
    let mut seen = HashSet::new();
    let mut valid = true;
    let mut active_boundary = None;
    // Large histories may need several small folds before the final tail fits.
    let mut staged_boundary = None;
    let mut prefix = vec![2usize; input.len() + 1];
    for i in 0..input.len() {
        prefix[i + 1] = prefix[i].checked_add(sizes[i] + usize::from(i > 0))
            .ok_or_else(|| anyhow!("Context size overflow"))?;
    }
    let source_fits = |index: usize| -> Result<bool> {
        if prefix[index] > config.max_source_bytes { return Ok(false); }
        match config.system_input_budget {
            Some(budget) => Ok(config.summary_envelope_bytes(&input[..index])? <= budget),
            None => Ok(true),
        }
    };
    for (i, item) in input.iter().enumerate() {
        // Evaluate a boundary before consuming its user turn. This prevents
        // splitting a function call from its corresponding output.
        if i > 0 && is_user(item) && valid && pending.is_empty() && source_fits(i)? {
            if fits(i) {
                let source = input[..i].to_vec();
                return Ok(Some(Plan {
                    drop_until: i, boundary: Boundary::PriorTurn,
                    source_digest: digest(&source)?, source, max_bytes,
                    reserve_bytes, system_input_budget:config.system_input_budget, summary_instructions:config.summary_instructions.clone(), config: config.clone(),
                }));
            }
            staged_boundary = Some((i, Boundary::PriorTurn));
        }
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => match call_id(item) {
                Some(id) if seen.insert(id.to_owned()) => {
                    pending.insert(id.to_owned());
                }
                _ => valid = false,
            },
            Some("function_call_output") => match call_id(item) {
                Some(id) if pending.remove(id) => {
                    // Keep later exchanges verbatim while summarizing completed work.
                    if valid && pending.is_empty() && source_fits(i + 1)? {
                        if fits(i + 1) && active_boundary.is_none() {
                            active_boundary = Some(i + 1);
                        } else if !fits(i + 1) {
                            staged_boundary = Some((i + 1, Boundary::CompletedToolExchange));
                        }
                    }
                }
                _ => valid = false,
            },
            _ => {}
        }
    }
    if valid && pending.is_empty() {
        if let Some((drop_until, boundary)) = active_boundary
            .map(|index| (index, Boundary::CompletedToolExchange)).or(staged_boundary) {
            let source = input[..drop_until].to_vec();
            return Ok(Some(Plan {
                drop_until, boundary, source_digest: digest(&source)?,
                source, max_bytes, reserve_bytes, system_input_budget:config.system_input_budget, summary_instructions:config.summary_instructions.clone(), config: config.clone(),
            }));
        }
    }
    bail!("Context exceeds budget without a completed turn or tool exchange to summarize")
}

/// Replace a planned prefix only after a summary has been generated and validated.
/// The caller must persist this result before sending it to a provider.
pub fn apply_summary(input: &[Value], plan: &Plan, summary: &str) -> Result<Vec<Value>> {
    let valid_boundary = match plan.boundary {
        Boundary::PriorTurn => plan.drop_until < input.len() && is_user(&input[plan.drop_until]),
        Boundary::CompletedToolExchange => input.get(plan.drop_until.saturating_sub(1))
            .is_some_and(|item| item["type"] == "function_call_output"),
    };
    if plan.drop_until == 0
        || plan.drop_until > input.len()
        || !valid_boundary
        || input[..plan.drop_until] != plan.source
        || digest(&input[..plan.drop_until])? != plan.source_digest
    {
        bail!("Stale or invalid context compaction plan");
    }
    let summary = summary.trim();
    if summary.is_empty() || summary.len() > plan.config.max_summary_bytes || summary.len() > plan.reserve_bytes
    {
        bail!("Context summary has invalid length");
    }
    let mut result = Vec::with_capacity(input.len() - plan.drop_until + 1);
    let prefix = match plan.boundary {
        Boundary::PriorTurn => &plan.config.summary_prefix,
        Boundary::CompletedToolExchange => &plan.config.active_summary_prefix,
    };
    result.push(json!({"role":"user","content":format!("{prefix}{summary}")}));
    // COMPACTION CHECKPOINT: one summary replaces the entire aged prefix (including
    // its previous checkpoint); the retained suffix remains verbatim for main.rs to save.
    result.extend_from_slice(&input[plan.drop_until..]);
    let result_bytes = encoded_len(&json!(&result))?;
    if result_bytes > plan.max_bytes && result_bytes >= encoded_len(&json!(input))? {
        bail!("Compacted context did not shrink toward the budget");
    }
    Ok(result)
}

/// Validate an untrusted live plan against host-owned policy and exact current input.
/// Alternate safe boundaries are allowed; the service cannot choose looser safety limits.
pub fn validate_external_plan(input:&[Value],plan:&Plan,config:&Config,history_budget:usize)->Result<()> {
 config.validate()?;
 if plan.max_bytes!=history_budget || plan.reserve_bytes!=config.summary_reserve_bytes
  || plan.system_input_budget!=config.system_input_budget || plan.summary_instructions!=config.summary_instructions
  || serde_json::to_value(&plan.config)?!=serde_json::to_value(config)?
  || plan.drop_until==0 || plan.drop_until>input.len()
  || input[..plan.drop_until]!=plan.source || digest(&input[..plan.drop_until])?!=plan.source_digest
  || crate::context_accounting::json_size(&plan.source)?>config.max_source_bytes {
  bail!("Stale or invalid context compaction plan");
 }
 if config.system_input_budget.is_some_and(|budget|config.summary_envelope_bytes(&plan.source).map_or(true,|size|size>budget)) {bail!("Stale or invalid context compaction plan");}
 let valid_boundary=match plan.boundary {
  Boundary::PriorTurn=>input.get(plan.drop_until).is_some_and(is_user),
  Boundary::CompletedToolExchange=>input[plan.drop_until-1]["type"]=="function_call_output",
 };
 if !valid_boundary {bail!("Stale or invalid context compaction plan");}
 let mut seen=HashSet::new();let mut pending=HashSet::new();
 for item in &input[..plan.drop_until] {
  match item["type"].as_str() {
   Some("function_call")=>match call_id(item) {Some(id) if seen.insert(id)=>{pending.insert(id);},_=>bail!("Stale or invalid context compaction plan")},
   Some("function_call_output")=>match call_id(item) {Some(id) if pending.remove(id)=>{},_=>bail!("Stale or invalid context compaction plan")},
   _=>{},
  }
 }
 if !pending.is_empty(){bail!("Stale or invalid context compaction plan");}
 Ok(())
}
