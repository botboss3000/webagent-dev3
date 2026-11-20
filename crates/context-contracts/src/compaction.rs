//! Deterministic compaction policy, planning and summary validation; no store or filesystem access.
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
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
    /// Larger tool outputs skip duplicate detection, so they replay as previews.
    #[serde(default = "default_dedup_max_output_bytes")]
    pub dedup_max_output_bytes: usize,
    #[serde(default = "default_search_output_bytes")]
    pub search_output_bytes: usize,
    #[serde(default = "default_search_line_bytes")]
    pub search_line_bytes: usize,
    pub max_summary_bytes: usize,
    pub summary_prefix: String,
    pub active_summary_prefix: String,
    pub summary_prompt: String,
    /// Lean prompt for one incremental summary block; `summary_prompt` merges blocks.
    /// Absent (older policies) uses `summary_prompt` for every fold.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub block_prompt: String,
    /// Background summaries start at `trigger_tokens`; the run stops to fold only at the
    /// ceiling. Absent or below the trigger means the trigger is the ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ceiling_tokens: Option<usize>,
    /// Summary blocks larger than this together merge their oldest blocks into one.
    #[serde(default = "default_merge_tokens")]
    pub merge_tokens: usize,
    /// A requested fold (the agent's task fold or `/compact`) that would summarize less
    /// than this is refused: it would save little and cost a model call.
    #[serde(default = "default_min_fold_tokens")]
    pub min_fold_tokens: usize,
    /// Who writes summaries: the running model (cache-friendly fork) or the System role.
    #[serde(default)]
    pub summarizer: Summarizer,
    /// Failed bridge forks in a row before the next summary starts a new session with the
    /// selected model instead (it cannot fail on a history mismatch, but reuses no cache).
    #[serde(default = "default_fork_attempts")]
    pub fork_attempts: u64,
    /// Call-scoped measured serialized bytes per provider token (the model's calibrated
    /// ratio). Thresholds stay in provider tokens; absent uses `characters_per_token`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_per_token: Option<f64>,
}
fn default_fork_attempts() -> u64 {
    3
}
/// Who writes a summary. `SameModel` forks the running request so its cached prefix is
/// reused; providers that cannot fork fall back to the System role.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Summarizer {
    #[default]
    #[serde(rename = "system")]
    System,
    #[serde(rename = "self")]
    SameModel,
}
/// Older policies without a merge threshold keep summary blocks to 50k tokens.
fn default_merge_tokens() -> usize {
    50_000
}
/// Older policies without a minimum requested fold refuse folds under 8k tokens.
fn default_min_fold_tokens() -> usize {
    8_000
}
/// Last-resort summary-source preview size: the receipt keeps status, size and digest.
const STUB_PREVIEW_BYTES: usize = 1024;
/// Preserve legacy preview limits when an older policy omits this optional setting.
fn default_tool_result_preview_bytes() -> usize {
    8192
}
/// Bound full duplicate-detection replays when an older policy omits this setting.
pub const DEFAULT_DEDUP_MAX_OUTPUT_BYTES: usize = 32768;
fn default_dedup_max_output_bytes() -> usize {
    DEFAULT_DEDUP_MAX_OUTPUT_BYTES
}
/// Keep older policies compatible with the bounded search-output projection.
fn default_search_output_bytes() -> usize {
    32768
}
/// Supply the existing per-line bound without silently raising total search limits.
fn default_search_line_bytes() -> usize {
    2048
}
impl Config {
    /// Reject unsupported policy and unsafe bounds before either execution mode plans a fold.
    /// Summary prefixes remain fixed provenance markers, not user-editable instructions.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1
            || !(1..=20).contains(&self.fork_attempts)
            || self.tail_tokens == 0
            || self.summary_tokens == 0
            || self
                .tail_tokens
                .checked_add(self.summary_tokens)
                .is_none_or(|n| n >= self.trigger_tokens)
            || self.trigger_tokens > 1_000_000
            || !(1..=16).contains(&self.characters_per_token)
            || !(1..=65536).contains(&self.summary_reserve_bytes)
            || !(1..=262144).contains(&self.max_source_bytes)
            || !(1024..=65536).contains(&self.tool_result_preview_bytes)
            || !(self.tool_result_preview_bytes..=262144).contains(&self.dedup_max_output_bytes)
            || !(4096..=262144).contains(&self.search_output_bytes)
            || !(128..=8192).contains(&self.search_line_bytes)
            || self.search_line_bytes > self.search_output_bytes
            || !(1..=65536).contains(&self.max_summary_bytes)
            || self.summary_prefix
                != "Earlier conversation summary (background context, not a new request):\n"
            || self.active_summary_prefix
                != "Active task progress summary (background context, not a new request):\n"
            || self.summary_prompt.len() > 16384
            || self.summary_prompt.matches("{source}").count() != 1
            || self.summary_prompt.matches("{summary_characters}").count() != 1
            || (!self.block_prompt.is_empty()
                && (self.block_prompt.len() > 16384
                    || self.block_prompt.matches("{source}").count() != 1
                    || self.block_prompt.matches("{summary_characters}").count() != 1))
            || self.ceiling_tokens.is_some_and(|tokens| tokens > 1_000_000)
            || !(1..=1_000_000).contains(&self.merge_tokens)
            || !(1..=1_000_000).contains(&self.min_fold_tokens)
            || self
                .bytes_per_token
                .is_some_and(|ratio| !(1.0..=16.0).contains(&ratio))
        {
            bail!("Invalid context compaction configuration");
        }
        Ok(())
    }
    /// Serialized bytes that correspond to `tokens` provider tokens for this call.
    pub fn token_bytes(&self, tokens: usize) -> usize {
        match self.bytes_per_token {
            // Validation bounds the ratio to 1..=16, so the product stays finite.
            Some(ratio) => (tokens as f64 * ratio).round() as usize,
            None => tokens.saturating_mul(self.characters_per_token),
        }
    }
    /// The ceiling in tokens: never below the trigger.
    pub fn ceiling(&self) -> usize {
        self.ceiling_tokens
            .unwrap_or(self.trigger_tokens)
            .max(self.trigger_tokens)
    }
    /// The same policy folding only at the ceiling; the main call assembles with it while
    /// a background summary runs from the trigger.
    pub fn at_ceiling(&self) -> Self {
        let mut config = self.clone();
        config.trigger_tokens = self.ceiling();
        config
    }
    /// Render the planned private prefix only for the core-owned summary-model call.
    pub fn prompt(&self, plan: &Plan) -> Result<String> {
        self.prompt_for(plan, &plan.source)
    }
    /// The summary prompt for `plan` over `source` (the plan's source, or its shortened
    /// projection): a lean block prompt for a fold, the full checkpoint prompt for a merge.
    pub fn prompt_for(&self, plan: &Plan, source: &[Value]) -> Result<String> {
        let (template, tokens) = match plan.kind {
            PlanKind::Merge => (&self.summary_prompt, self.summary_tokens * 2),
            PlanKind::Fold if !self.block_prompt.is_empty() => {
                (&self.block_prompt, self.summary_tokens)
            }
            PlanKind::Fold => (&self.summary_prompt, self.summary_tokens),
        };
        self.render(template, tokens, plan.summarized(source), true)
    }
    /// Summary-model transcript JSON without opaque reasoning. When the full outputs exceed the
    /// source limit, large tool outputs become previews, then stubs, so that one oversized tool
    /// exchange cannot block every fold. The plan itself always keeps the exact history.
    pub fn summary_source(&self, input: &[Value]) -> Result<String> {
        let mut items: Vec<Value> = input
            .iter()
            .filter(|item| item["type"] != "reasoning")
            .cloned()
            .collect();
        omit_skill_bodies(&mut items);
        let mut source = serde_json::to_string(&items)?;
        // Each level shortens the original outputs, never an earlier preview: the planner
        // sizes cut points from stubs of the originals, and a stub of a preview can be
        // larger, which made the host refuse a plan the planner had fitted.
        for preview_bytes in [self.tool_result_preview_bytes, STUB_PREVIEW_BYTES] {
            if source.len() <= self.max_source_bytes {
                break;
            }
            let mut shortened = items.clone();
            project_tool_results(&mut shortened, preview_bytes)?;
            source = serde_json::to_string(&shortened)?;
        }
        Ok(source)
    }
    /// Substitute transcript text last to prevent template expansion.
    /// Source limits apply to dispatched summaries; measurement may inspect larger histories.
    pub fn prompt_source(&self, input: &[Value], enforce_source_limit: bool) -> Result<String> {
        self.render(
            &self.summary_prompt,
            self.summary_tokens,
            input,
            enforce_source_limit,
        )
    }
    fn render(
        &self,
        template: &str,
        tokens: usize,
        input: &[Value],
        enforce_source_limit: bool,
    ) -> Result<String> {
        let source = self.summary_source(input)?;
        if enforce_source_limit && source.len() > self.max_source_bytes {
            bail!("Context summary source is too large");
        }
        // Substitute source last so transcript text cannot expand template placeholders.
        Ok(template
            .replace(
                "{summary_characters}",
                &(tokens * self.characters_per_token).to_string(),
            )
            .replace("{source}", &source))
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

/// Drop the skill text from `load_skill` results, Main Notes and task list changes (the
/// server's `main_notes_read-…`/`task_list_read-…` rows and the agent's own writes; the
/// agent's own reads stay) and mode changes so a summary never keeps them: a fold keeps them ahead of
/// its new block (see `apply_summary`), and a merge leaves skills and notes to the prompt.
pub fn omit_skill_bodies(input: &mut [Value]) {
    let loads = input
        .iter()
        .filter(|item| item["type"] == "function_call" && item["name"] == "load_skill")
        .filter_map(call_id)
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    let notes = input
        .iter()
        .filter(|item| {
            item["type"] == "function_call"
                && (matches!(
                    item["name"].as_str(),
                    Some(
                        "main_notes_write"
                            | "task_list_write"
                            | "mode_guidance"
                            | "set_execution_mode"
                    )
                ) || item["call_id"].as_str().is_some_and(|id| {
                    id.starts_with("main_notes_read-") || id.starts_with("task_list_read-")
                }))
        })
        .filter_map(call_id)
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    for item in input.iter_mut() {
        if item["type"] == "function_call_output"
            && item["call_id"]
                .as_str()
                .is_some_and(|id| notes.contains(id))
        {
            item["output"] = json!("Omitted; kept ahead of this summary block or in the prompt.");
        }
    }
    for item in input {
        if item["type"] != "function_call_output"
            || !item["call_id"]
                .as_str()
                .is_some_and(|id| loads.contains(id))
        {
            continue;
        }
        let Some(mut result) = item["output"]
            .as_str()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        else {
            continue;
        };
        if let Some(output) = result["output"].as_object_mut() {
            if output.remove("text").is_some() {
                output.insert("text_omitted".into(), json!(true));
                item["output"] = json!(result.to_string());
            }
        }
    }
}

/// Keep the encrypted interaction as the full record. Only provider replay is
/// reduced, including on resume of a run saved before this limit was installed.
pub fn project_tool_results(input: &mut [Value], preview_bytes: usize) -> Result<bool> {
    // Keep catalog metadata intact while bounding ordinary results.
    let instruction_calls = input
        .iter()
        .filter(|item| item["type"] == "function_call" && item["name"] == "load_skill")
        .filter_map(call_id)
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    let mut changed = false;
    for item in input {
        if item["type"] != "function_call_output" {
            continue;
        }
        if item["call_id"]
            .as_str()
            .is_some_and(|id| instruction_calls.contains(id))
        {
            continue;
        }
        let Some(raw) = item["output"].as_str() else {
            continue;
        };
        if raw.len() <= preview_bytes {
            continue;
        }
        let parsed: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
        let mut end = preview_bytes.min(raw.len());
        let digest = hex::encode(Sha256::digest(raw.as_bytes()));
        // JSON escaping can expand a preview. Bound the final serialized receipt
        // so a saved run is stable across subsequent model boundaries.
        let projected = loop {
            while !raw.is_char_boundary(end) {
                end -= 1;
            }
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
            if serialized.len() <= preview_bytes {
                break serialized;
            }
            if end == 0 {
                bail!("Tool result preview budget cannot fit receipt");
            }
            end = end.saturating_sub((serialized.len() - preview_bytes).max(1));
        };
        item["output"] = json!(projected);
        changed = true;
    }
    Ok(changed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Boundary {
    PriorTurn,
    CompletedToolExchange,
}

/// A fold summarizes history after the existing summary blocks into one new block; a
/// merge folds the oldest blocks into one, keeping the newest two.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PlanKind {
    #[default]
    Fold,
    Merge,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    /// Items before this index are summarized; the rest are retained verbatim.
    pub drop_until: usize,
    /// Items before this index are kept unchanged ahead of the new summary block: the
    /// existing blocks for a fold, none for a merge.
    #[serde(default)]
    pub first: usize,
    #[serde(default)]
    pub kind: PlanKind,
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

impl Plan {
    /// The items a summary covers, from `source` (the plan source or its shortened
    /// projection, item for item): a fold leaves earlier blocks out of its summary.
    pub fn summarized<'a>(&self, source: &'a [Value]) -> &'a [Value] {
        source.get(self.first..).unwrap_or(source)
    }
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

/// Whether `item` is a summary block: a user item carrying a summary prefix.
fn is_summary(item: &Value, config: &Config) -> bool {
    is_user(item)
        && item["content"].as_str().is_some_and(|text| {
            text.starts_with(&config.summary_prefix)
                || text.starts_with(&config.active_summary_prefix)
        })
}

/// Tool calls a fold keeps ahead of its new block instead of summarizing: loaded skills,
/// Main Notes and task list changes and the execution mode.
fn is_retained_call(item: &Value) -> bool {
    item["type"] == "function_call"
        && matches!(
            item["name"].as_str(),
            Some(
                "load_skill"
                    | "set_execution_mode"
                    | "main_notes_read"
                    | "main_notes_write"
                    | "task_list_read"
                    | "task_list_write"
            )
        )
}

/// Indices of the summary blocks at the start of history. Each block may follow the
/// retained rows its fold kept ahead of it, so the start reads: rows, block, rows, block.
pub fn summary_blocks(input: &[Value], config: &Config) -> Vec<usize> {
    let mut retained = HashSet::new();
    let mut blocks = Vec::new();
    for (index, item) in input.iter().enumerate() {
        if is_summary(item, config) {
            blocks.push(index);
        } else if is_retained_call(item) {
            retained.extend(call_id(item).map(str::to_owned));
        } else if !(item["type"] == "function_call_output"
            && call_id(item).is_some_and(|id| retained.contains(id)))
        {
            break;
        }
    }
    blocks
}

/// Where the history after the summary blocks starts: everything before it (the blocks and
/// the rows kept ahead of each) stays byte-identical across folds, so a provider's cached
/// prefix stays valid.
pub fn leading_blocks(input: &[Value], config: &Config) -> usize {
    summary_blocks(input, config)
        .last()
        .map_or(0, |last| last + 1)
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
///
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
pub fn plan_with_config(
    input: &[Value],
    max_bytes: usize,
    config: &Config,
    rolling: bool,
    force: bool,
) -> Result<Option<Plan>> {
    let reserve_bytes = config.summary_reserve_bytes;
    if max_bytes < 2 || reserve_bytes == 0 || reserve_bytes >= max_bytes {
        bail!("Invalid context budget");
    }
    let sizes = input.iter().map(encoded_len).collect::<Result<Vec<_>>>()?;
    let characters = input
        .iter()
        .map(|item| serde_json::to_string(item).map(|text| text.chars().count()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    plan_measured(
        input,
        max_bytes,
        config,
        rolling,
        force,
        &sizes,
        &characters,
        0,
    )
}

/// Consume append-only accounting without serializing unchanged history. Full source
/// copying and hashing occur only after a safe compaction boundary is selected.
pub fn plan_cached(
    input: &[Value],
    max_bytes: usize,
    config: &Config,
    sizes: &[usize],
    characters: &[usize],
) -> Result<Option<Plan>> {
    config.validate()?;
    plan_request_cached(input, max_bytes, config, sizes, characters, 0)
}
/// Background trigger check: the same rolling plan as assembly, measured here, for a
/// summary that starts while the main call proceeds under `Config::at_ceiling`.
pub fn plan_trigger(
    input: &[Value],
    max_bytes: usize,
    config: &Config,
    request_overhead: usize,
) -> Result<Option<Plan>> {
    config.validate()?;
    if config.summary_reserve_bytes >= max_bytes {
        bail!("Invalid context budget");
    }
    let sizes = input.iter().map(encoded_len).collect::<Result<Vec<_>>>()?;
    let characters = input
        .iter()
        .map(|item| serde_json::to_string(item).map(|text| text.chars().count()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    plan_measured(
        input,
        max_bytes,
        config,
        true,
        false,
        &sizes,
        &characters,
        request_overhead,
    )
}
/// Stable identity of a user message in history: the digest of its content. The
/// agent's task fold names the message that starts a new task by this key, so the
/// fold still finds it after later items arrive.
pub fn turn_key(item: &Value) -> Option<String> {
    is_user(item).then(|| hex::encode(Sha256::digest(item["content"].to_string().as_bytes())))
}

/// Completed boundaries of `input` after its earlier summary blocks: before a user message,
/// or after a tool result that closes every open call. `None` for a malformed exchange (a
/// reused call ID, or a result without its call); a repeated result of an answered call is
/// skipped, and a call still open just ends the list.
fn requested_boundaries(input: &[Value], blocks: usize) -> Option<Vec<(usize, Boundary)>> {
    let mut pending = HashSet::new();
    let mut seen = HashSet::new();
    let mut boundaries = Vec::new();
    for (i, item) in input.iter().enumerate() {
        if i > blocks && is_user(item) && pending.is_empty() {
            boundaries.push((i, Boundary::PriorTurn));
        }
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                let id = call_id(item)?;
                if !seen.insert(id.to_owned()) {
                    return None;
                }
                pending.insert(id.to_owned());
            }
            Some("function_call_output") => {
                let id = call_id(item)?;
                if !pending.remove(id) {
                    // A result saved twice is skipped: the extra copy of a call already
                    // answered, or one saved ahead of its own call (a call with its ID
                    // follows). A result whose call never appears is still malformed.
                    let call_follows = input[i + 1..].iter().any(|later| {
                        later.get("type").and_then(Value::as_str) == Some("function_call")
                            && call_id(later) == Some(id)
                    });
                    if seen.contains(id) || call_follows {
                        continue;
                    }
                    return None;
                }
                if pending.is_empty() && i + 1 > blocks && i + 1 < input.len() {
                    boundaries.push((i + 1, Boundary::CompletedToolExchange));
                }
            }
            _ => {}
        }
    }
    Some(boundaries)
}

/// A fold of `input[first..drop_until]` the user or agent asked for, or `None` when it would
/// summarize less than `min_fold_tokens`. Its byte bound is the current history, so the
/// summary only has to shrink what it replaces.
fn requested_plan(
    input: &[Value],
    first: usize,
    drop_until: usize,
    boundary: Boundary,
    config: &Config,
) -> Result<Option<Plan>> {
    let folded = input[first..drop_until]
        .iter()
        .map(encoded_len)
        .sum::<Result<usize>>()?;
    if folded < config.token_bytes(config.min_fold_tokens) {
        return Ok(None);
    }
    if config.summary_source(&input[first..drop_until])?.len() > config.max_source_bytes {
        bail!("Earlier context exceeds the configured summary-source limit");
    }
    let source = input[..drop_until].to_vec();
    Ok(Some(Plan {
        drop_until,
        first,
        kind: PlanKind::Fold,
        boundary,
        source_digest: digest(&source)?,
        source,
        max_bytes: encoded_len(&json!(input))?,
        reserve_bytes: config.summary_reserve_bytes,
        system_input_budget: config.system_input_budget,
        summary_instructions: config.summary_instructions.clone(),
        config: config.clone(),
    }))
}

/// Fold everything after the earlier summary blocks up to the user message at `until`,
/// which starts a new task (the agent's task fold). Everything from it on stays
/// verbatim. `None` when there is too little to fold; an error when `until` is not a
/// completed user-turn boundary.
pub fn plan_until(input: &[Value], until: usize, config: &Config) -> Result<Option<Plan>> {
    config.validate()?;
    let blocks = leading_blocks(input, config);
    let boundaries = requested_boundaries(input, blocks)
        .ok_or_else(|| anyhow!("Context has an unfinished or malformed tool exchange"))?;
    if !boundaries
        .iter()
        .any(|&(index, kind)| index == until && kind == Boundary::PriorTurn)
    {
        bail!("The new task does not start at a completed turn boundary");
    }
    requested_plan(input, blocks, until, Boundary::PriorTurn, config)
}

/// The section marks the agent's `task_list_write` results carry after `from`, in history
/// order: `(index of the result, mark kind, section id)`, where the kind is `created`,
/// `started` or `closed`.
fn task_marks(input: &[Value], from: usize) -> Vec<(usize, &'static str, String)> {
    let writes = input
        .iter()
        .filter(|item| item["type"] == "function_call" && item["name"] == "task_list_write")
        .filter_map(call_id)
        .collect::<HashSet<_>>();
    let mut marks = Vec::new();
    for (index, item) in input.iter().enumerate().skip(from) {
        if item["type"] != "function_call_output"
            || !call_id(item).is_some_and(|id| writes.contains(id))
        {
            continue;
        }
        let Some(result) = item["output"]
            .as_str()
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
        else {
            continue;
        };
        for kind in ["created", "started", "closed"] {
            for id in result["output"][kind].as_array().into_iter().flatten() {
                if let Some(id) = id.as_str() {
                    marks.push((index, kind, id.to_owned()));
                }
            }
        }
    }
    marks
}

/// Task mode: once a task starts (its first step begins), fold the task `retained + 1`
/// behind it, with everything before. The cut is the first user message after that task's
/// section closed, but never past the turn that created (else started) the oldest task kept;
/// so a task being discussed or planned keeps its predecessor verbatim. `None` when fewer
/// tasks have started since the last block, the history has a malformed exchange, or the
/// fold would be smaller than `min_fold_tokens` or larger than one summary source.
pub fn plan_tasks(input: &[Value], retained: usize, config: &Config) -> Result<Option<Plan>> {
    config.validate()?;
    let blocks = leading_blocks(input, config);
    let Some(boundaries) = requested_boundaries(input, blocks) else {
        return Ok(None);
    };
    let turns = boundaries
        .iter()
        .filter(|&&(_, kind)| kind == Boundary::PriorTurn)
        .map(|&(index, _)| index)
        .collect::<Vec<_>>();
    let marks = task_marks(input, blocks);
    let mut starts: Vec<(usize, &str)> = Vec::new();
    for (index, kind, id) in &marks {
        if *kind == "started" && !starts.iter().any(|(_, seen)| seen == id) {
            starts.push((*index, id));
        }
    }
    let Some(folded) = starts.len().checked_sub(retained + 2) else {
        return Ok(None);
    };
    let (folded_start, folded_id) = starts[folded];
    let (kept_start, kept_id) = starts[folded + 1];
    // The kept task begins at the turn that created its section, else the turn it started.
    let kept_from = marks
        .iter()
        .find(|(_, kind, id)| *kind == "created" && id == kept_id)
        .map_or(kept_start, |(index, _, _)| *index);
    let Some(&limit) = turns.iter().rev().find(|&&turn| turn <= kept_from) else {
        return Ok(None);
    };
    let after_close = marks
        .iter()
        .find(|(index, kind, id)| *kind == "closed" && id == folded_id && *index > folded_start)
        .and_then(|(closed, _, _)| turns.iter().copied().find(|&turn| turn > *closed));
    let cut = after_close.map_or(limit, |turn| turn.min(limit));
    if cut <= blocks {
        return Ok(None);
    }
    if config.summary_source(&input[blocks..cut])?.len() > config.max_source_bytes {
        return Ok(None);
    }
    requested_plan(input, blocks, cut, Boundary::PriorTurn, config)
}

/// The agent's fold at a task start: the agent is starting `section`'s first step and says
/// the earlier work is unrelated, so everything after the summary blocks folds up to the
/// first user message after the last section that closed, but never past the turn that
/// created `section` (the turn before the call when this write creates it). Returns that
/// user message's index; `None` when it would fold nothing past the blocks or less than
/// `min_fold_tokens`, and an error for a malformed exchange or an oversized range.
pub fn task_fold_cut(input: &[Value], section: &str, config: &Config) -> Result<Option<usize>> {
    config.validate()?;
    let blocks = leading_blocks(input, config);
    let boundaries = requested_boundaries(input, blocks)
        .ok_or_else(|| anyhow!("Context has an unfinished or malformed tool exchange"))?;
    let turns = boundaries
        .iter()
        .filter(|&&(_, kind)| kind == Boundary::PriorTurn)
        .map(|&(index, _)| index)
        .collect::<Vec<_>>();
    let marks = task_marks(input, blocks);
    let kept_from = marks
        .iter()
        .find(|(_, kind, id)| *kind == "created" && id == section)
        .map_or(input.len(), |(index, _, _)| *index);
    let Some(&limit) = turns.iter().rev().find(|&&turn| turn <= kept_from) else {
        return Ok(None);
    };
    let after_close = marks
        .iter()
        .rev()
        .find(|(index, kind, id)| *kind == "closed" && id != section && *index < kept_from)
        .and_then(|(closed, _, _)| turns.iter().copied().find(|&turn| turn > *closed));
    let cut = after_close.map_or(limit, |turn| turn.min(limit));
    if cut <= blocks {
        return Ok(None);
    }
    Ok(requested_plan(input, blocks, cut, Boundary::PriorTurn, config)?.map(|_| cut))
}

/// `/compact`: fold the oldest history after the earlier blocks and keep the agent's tail
/// (`tail_tokens`) verbatim, at the earliest completed boundary whose tail fits it, else
/// at the last one. A source too large for one summary folds its oldest part first.
/// `None` when nothing worth folding lies outside the tail.
pub fn plan_command(input: &[Value], config: &Config) -> Result<Option<Plan>> {
    config.validate()?;
    let blocks = leading_blocks(input, config);
    let Some(boundaries) = requested_boundaries(input, blocks) else {
        bail!("Context has an unfinished or malformed tool exchange");
    };
    let mut tail = vec![0usize; input.len() + 1];
    for i in (0..input.len()).rev() {
        tail[i] = tail[i + 1] + serde_json::to_string(&input[i])?.chars().count();
    }
    let tail_bytes = config.token_bytes(config.tail_tokens);
    let fitting: Vec<(usize, Boundary)> = boundaries
        .into_iter()
        .filter(|&(index, _)| {
            config
                .summary_source(&input[blocks..index])
                .is_ok_and(|source| source.len() <= config.max_source_bytes)
        })
        .collect();
    let chosen = fitting
        .iter()
        .find(|&&(index, _)| tail[index] <= tail_bytes)
        .or(fitting.last());
    match chosen {
        Some(&(index, boundary)) => requested_plan(input, blocks, index, boundary, config),
        None => Ok(None),
    }
}

/// Request overhead is measured from the selected instructions, schemas and framing.
/// It affects the trigger and retained-history budget, never the summary source.
pub fn plan_request_cached(
    input: &[Value],
    max_bytes: usize,
    config: &Config,
    sizes: &[usize],
    characters: &[usize],
    request_overhead: usize,
) -> Result<Option<Plan>> {
    config.validate()?;
    plan_measured(
        input,
        max_bytes,
        config,
        true,
        false,
        sizes,
        characters,
        request_overhead,
    )
}
/// Evaluate resolved byte ceilings and rolling policy using caller-owned measurements.
/// Only completed prefixes may fold; staged boundaries let large histories shrink in safe steps.
fn plan_measured(
    input: &[Value],
    max_bytes: usize,
    config: &Config,
    rolling: bool,
    force: bool,
    sizes: &[usize],
    characters: &[usize],
    request_overhead: usize,
) -> Result<Option<Plan>> {
    let reserve_bytes = config.summary_reserve_bytes;
    if sizes.len() != input.len() || characters.len() != input.len() || reserve_bytes >= max_bytes {
        bail!("Invalid compaction measurements or budget");
    }
    let total = 2usize
        .checked_add(sizes.iter().sum::<usize>())
        .and_then(|n| n.checked_add(input.len().saturating_sub(1)))
        .ok_or_else(|| anyhow!("Context size overflow"))?;
    let trigger_bytes = config.token_bytes(config.trigger_tokens);
    if rolling && request_overhead >= trigger_bytes {
        bail!("Compaction trigger cannot fit fixed request overhead");
    }
    let request_triggered = rolling && total.saturating_add(request_overhead) >= trigger_bytes;
    let blocks = leading_blocks(input, config);
    // The System budget caps what one System summary may read. It folds early only when
    // the System model writes the summaries and the history since the last block would
    // outgrow it; a same-model summary reads the request itself, so only the trigger counts.
    let system_triggered = match config.system_input_budget {
        Some(budget) if config.summarizer == Summarizer::System => {
            config.summary_envelope_bytes(&input[blocks..])? > budget
        }
        _ => false,
    };
    if total <= max_bytes && !request_triggered && !system_triggered && !force {
        return Ok(None);
    }
    // Earlier summaries stay as separate blocks ahead of new history. Once together they
    // outgrow `merge_tokens`, the oldest fold into one block first; the newest two stay.
    let block_indices = summary_blocks(input, config);
    if let (false, [.., third_last, _, _]) = (force, block_indices.as_slice()) {
        // The newest two blocks and the rows kept ahead of them stay.
        let drop_until = third_last + 1;
        let block_bytes = block_indices.iter().map(|&i| sizes[i]).sum::<usize>();
        if block_bytes > config.token_bytes(config.merge_tokens)
            && config.summary_source(&input[..drop_until])?.len() <= config.max_source_bytes
        {
            let source = input[..drop_until].to_vec();
            return Ok(Some(Plan {
                drop_until,
                first: 0,
                kind: PlanKind::Merge,
                boundary: Boundary::PriorTurn,
                source_digest: digest(&source)?,
                source,
                max_bytes,
                reserve_bytes,
                system_input_budget: config.system_input_budget,
                summary_instructions: config.summary_instructions.clone(),
                config: config.clone(),
            }));
        }
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
        Some(budget) => (max_bytes - reserve_bytes)
            .min(budget.saturating_sub(config.summary_envelope_bytes(&[])? + reserve_bytes)),
        None => max_bytes - reserve_bytes,
    };
    let target = if rolling {
        target.min(
            trigger_bytes
                .saturating_sub(request_overhead)
                .saturating_sub(reserve_bytes),
        )
    } else {
        target
    };
    let mut tail_chars = vec![0usize; input.len() + 1];
    for i in (0..input.len()).rev() {
        tail_chars[i] = tail_chars[i + 1] + characters[i];
    }
    // A fold never summarizes only earlier blocks: it must also age real history,
    // otherwise oversized blocks could retrigger forever.
    let fits = |index: usize| {
        suffix[index] <= target
            && (!rolling || tail_chars[index] <= config.token_bytes(config.tail_tokens))
            && index > blocks
    };
    let mut pending = HashSet::new();
    let mut seen = HashSet::new();
    let mut valid = true;
    let mut active_boundary = None;
    // Large histories may need several small folds before the final tail fits.
    let mut staged_boundary = None;
    // Size cut points by the smallest summary source (stubbed outputs, no reasoning);
    // `summary_source` picks the fullest version that fits for the actual prompt.
    let mut stubbed = input.to_vec();
    project_tool_results(&mut stubbed, STUB_PREVIEW_BYTES)?;
    let mut prefix = vec![2usize; input.len() + 1];
    for i in 0..input.len() {
        let size = if stubbed[i]["type"] == "reasoning" {
            0
        } else if stubbed[i] == input[i] {
            sizes[i]
        } else {
            encoded_len(&stubbed[i])?
        };
        prefix[i + 1] = prefix[i]
            .checked_add(size + usize::from(i > 0))
            .ok_or_else(|| anyhow!("Context size overflow"))?;
    }
    // A fold's summary covers only the items after the existing blocks.
    let source_fits = |index: usize| -> Result<bool> {
        // Brackets, and no comma before the first summarized item.
        let bytes = prefix[index] - prefix[blocks] + 2 - usize::from(blocks > 0);
        if bytes > config.max_source_bytes {
            return Ok(false);
        }
        match config.system_input_budget {
            Some(budget) => Ok(config.summary_envelope_bytes(&input[blocks..index])? <= budget),
            None => Ok(true),
        }
    };
    for (i, item) in input.iter().enumerate() {
        // Evaluate a boundary before consuming its user turn. This prevents
        // splitting a function call from its corresponding output.
        if i > blocks && is_user(item) && valid && pending.is_empty() && source_fits(i)? {
            if fits(i) {
                let source = input[..i].to_vec();
                return Ok(Some(Plan {
                    drop_until: i,
                    first: blocks,
                    kind: PlanKind::Fold,
                    boundary: Boundary::PriorTurn,
                    source_digest: digest(&source)?,
                    source,
                    max_bytes,
                    reserve_bytes,
                    system_input_budget: config.system_input_budget,
                    summary_instructions: config.summary_instructions.clone(),
                    config: config.clone(),
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
            .map(|index| (index, Boundary::CompletedToolExchange))
            .or(staged_boundary)
        {
            let source = input[..drop_until].to_vec();
            return Ok(Some(Plan {
                drop_until,
                first: blocks,
                kind: PlanKind::Fold,
                boundary,
                source_digest: digest(&source)?,
                source,
                max_bytes,
                reserve_bytes,
                system_input_budget: config.system_input_budget,
                summary_instructions: config.summary_instructions.clone(),
                config: config.clone(),
            }));
        }
    }
    bail!("Context exceeds budget without a completed turn or tool exchange to summarize")
}

/// Whether a plan cuts the history at a safe point: before a user message for a prior turn
/// (a merge may also stop before the rows kept ahead of the next block), or right after a
/// tool output for a completed exchange.
fn safe_cut(input: &[Value], plan: &Plan) -> bool {
    match plan.boundary {
        Boundary::PriorTurn => input.get(plan.drop_until).is_some_and(|item| {
            is_user(item) || plan.kind == PlanKind::Merge && is_retained_call(item)
        }),
        Boundary::CompletedToolExchange => plan
            .drop_until
            .checked_sub(1)
            .and_then(|index| input.get(index))
            .is_some_and(|item| item["type"] == "function_call_output"),
    }
}

/// Replace a planned prefix only after a summary has been generated and validated.
/// The caller must persist this result before sending it to a provider.
pub fn apply_summary(input: &[Value], plan: &Plan, summary: &str) -> Result<Vec<Value>> {
    let valid_boundary = safe_cut(input, plan);
    if plan.drop_until <= plan.first
        || plan.drop_until > input.len()
        || !valid_boundary
        || input[..plan.drop_until] != plan.source
        || digest(&input[..plan.drop_until])? != plan.source_digest
    {
        bail!("Stale or invalid context compaction plan");
    }
    let summary = summary.trim();
    if summary.is_empty()
        || summary.len() > plan.config.max_summary_bytes
        || summary.len() > plan.reserve_bytes
    {
        bail!("Context summary has invalid length");
    }
    let mut result = Vec::with_capacity(plan.first + input.len() - plan.drop_until + 1);
    let prefix = match (plan.kind, plan.boundary) {
        (PlanKind::Merge, _) | (_, Boundary::PriorTurn) => &plan.config.summary_prefix,
        (_, Boundary::CompletedToolExchange) => &plan.config.active_summary_prefix,
    };
    // COMPACTION BLOCKS: everything before the new block stays byte-identical. A fold keeps
    // the summarized range's retained rows just ahead of its block and the rest of the tail
    // verbatim for the loop to save; a merge leaves skills and notes to the prompt.
    result.extend_from_slice(&input[..plan.first]);
    let block = json!({"role":"user","content":format!("{prefix}{summary}")});
    match plan.kind {
        PlanKind::Fold => {
            let (kept, tail) = retained_rows(
                &input[plan.first..plan.drop_until],
                &input[plan.drop_until..],
            );
            result.extend(kept);
            result.push(block);
            result.extend(tail);
        }
        PlanKind::Merge => {
            result.push(block);
            result.extend_from_slice(&input[plan.drop_until..]);
        }
    }
    let result_bytes = encoded_len(&json!(&result))?;
    if result_bytes > plan.max_bytes && result_bytes >= encoded_len(&json!(input))? {
        bail!("Compacted context did not shrink toward the budget");
    }
    Ok(result)
}

/// Completed call/output exchanges in `items`, in call order.
fn exchanges(items: &[Value]) -> Vec<(&Value, &Value)> {
    let outputs = items
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .filter_map(|item| Some((call_id(item)?, item)))
        .collect::<std::collections::HashMap<_, _>>();
    items
        .iter()
        .filter(|item| item["type"] == "function_call")
        .filter_map(|call| Some((call, *outputs.get(call_id(call)?)?)))
        .collect()
}

/// Whether a `load_skill` output still carries the skill text the model reads.
fn skill_loaded(output: &Value) -> bool {
    output["output"]
        .as_str()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .is_some_and(|result| {
            result["output"]["status"] == "loaded" && result["output"]["text"].is_string()
        })
}

/// A Main Notes change: the server's `main_notes_read-…` rows (another session's writes)
/// and this session's own `main_notes_write` results.
fn is_notes_change(call: &Value) -> bool {
    call["name"] == "main_notes_write"
        || call["name"] == "main_notes_read"
            && call_id(call).is_some_and(|id| id.starts_with("main_notes_read-"))
}

/// A task list change: the server's `task_list_read-…` rows (changes made outside the
/// agent's own calls) and the agent's own `task_list_write` results.
fn is_tasks_change(call: &Value) -> bool {
    call["name"] == "task_list_write"
        || call["name"] == "task_list_read"
            && call_id(call).is_some_and(|id| id.starts_with("task_list_read-"))
}

/// Every change of one kind from a fold's range and tail combined into one `name` read
/// exchange under `heading`, oldest first; `own` names the agent's own write tool.
fn combined_changes(
    changes: &[&(&Value, &Value)],
    name: &str,
    own: &str,
    heading: &str,
) -> Option<[Value; 2]> {
    if changes.is_empty() {
        return None;
    }
    let text = changes
        .iter()
        .map(|(call, output)| {
            let body = output["output"]
                .as_str()
                .map_or_else(|| output["output"].to_string(), str::to_owned);
            if call["name"] == own {
                format!("This session wrote: {body}")
            } else {
                body
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let text = format!("{heading}\n\n{text}");
    let id = format!("{name}-{}", &hex::encode(Sha256::digest(&text))[..16]);
    let arguments = json!({"changed_by":"combined at compaction"}).to_string();
    Some([
        json!({"type":"function_call","call_id":id,"name":name,"arguments":arguments}),
        json!({"type":"function_call_output","call_id":id,"output":text}),
    ])
}

/// The rows a fold keeps ahead of its new block, and the tail without the ones it moved:
/// each `load_skill` exchange still carrying its text (so the skill stays out of the
/// prompt), the latest mode change unless the tail shows a newer one, and every Main Notes
/// change and every task list change from the range and the tail, each kind combined into
/// one read exchange.
fn retained_rows(range: &[Value], tail: &[Value]) -> (Vec<Value>, Vec<Value>) {
    let in_range = exchanges(range);
    let mut kept = Vec::new();
    for (call, output) in &in_range {
        if call["name"] == "load_skill" && skill_loaded(output) {
            kept.extend([(*call).clone(), (*output).clone()]);
        }
    }
    let in_tail = exchanges(tail);
    if !in_tail
        .iter()
        .any(|(call, _)| call["name"] == "set_execution_mode")
    {
        if let Some((call, output)) = in_range
            .iter()
            .rev()
            .find(|(call, _)| call["name"] == "set_execution_mode")
        {
            kept.extend([(*call).clone(), (*output).clone()]);
        }
    }
    let changes = in_range
        .iter()
        .chain(&in_tail)
        .filter(|(call, _)| is_notes_change(call))
        .collect::<Vec<_>>();
    let task_changes = in_range
        .iter()
        .chain(&in_tail)
        .filter(|(call, _)| is_tasks_change(call))
        .collect::<Vec<_>>();
    let moved = in_tail
        .iter()
        .filter(|(call, _)| is_notes_change(call) || is_tasks_change(call))
        .filter_map(|(call, _)| call_id(call))
        .collect::<HashSet<_>>();
    let tail = tail
        .iter()
        .filter(|item| !call_id(item).is_some_and(|id| moved.contains(id)))
        .cloned()
        .collect();
    for exchange in [
        combined_changes(
            &changes,
            "main_notes_read",
            "main_notes_write",
            "Main Notes changes after the notes in the instructions, oldest first:",
        ),
        combined_changes(
            &task_changes,
            "task_list_read",
            "task_list_write",
            "Task list changes after the list in the instructions, oldest first:",
        ),
    ]
    .into_iter()
    .flatten()
    {
        kept.extend(exchange);
    }
    (kept, tail)
}

/// Major reorder, once the provider cache is gone (idle past the cache timeout, or changed
/// tool definitions): the retained rows kept ahead of each summary block leave the history,
/// so their skills join the instructions and the Main Notes snapshot is rebuilt, and so do
/// the server's Main Notes change rows, which the rebuilt snapshot already holds. Blocks
/// keep their order and the rest of the history stays word for word; a mode no longer shown
/// is added again at the end by the host.
pub fn reorder(input: &[Value], config: &Config) -> Vec<Value> {
    let start = leading_blocks(input, config);
    let server_notes = input[start..]
        .iter()
        .filter(|item| {
            item["type"] == "function_call" && (is_notes_change(item) || is_tasks_change(item))
        })
        .filter_map(call_id)
        .filter(|id| id.starts_with("main_notes_read-") || id.starts_with("task_list_read-"))
        .collect::<HashSet<_>>();
    input[..start]
        .iter()
        .filter(|item| is_summary(item, config))
        .chain(
            input[start..]
                .iter()
                .filter(|item| !call_id(item).is_some_and(|id| server_notes.contains(id))),
        )
        .cloned()
        .collect()
}

/// Validate an untrusted live plan against host-owned policy and exact current input.
/// Alternate safe boundaries are allowed; the service cannot choose looser safety limits.
pub fn validate_external_plan(
    input: &[Value],
    plan: &Plan,
    config: &Config,
    history_budget: usize,
) -> Result<()> {
    config.validate()?;
    // Each check names itself, so a refused plan shows which rule failed.
    let stale = |check: &str| anyhow!("Stale or invalid context compaction plan ({check})");
    if plan.max_bytes != history_budget {
        return Err(stale("history budget"));
    }
    if plan.reserve_bytes != config.summary_reserve_bytes
        || plan.system_input_budget != config.system_input_budget
        || plan.summary_instructions != config.summary_instructions
    {
        return Err(stale("summary limits"));
    }
    if serde_json::to_value(&plan.config)? != serde_json::to_value(config)? {
        return Err(stale("policy config"));
    }
    if plan.drop_until == 0
        || plan.drop_until > input.len()
        || input[..plan.drop_until] != plan.source
        || digest(&input[..plan.drop_until])? != plan.source_digest
    {
        return Err(stale("source prefix"));
    }
    let blocks = leading_blocks(input, config);
    let range = match plan.kind {
        PlanKind::Fold => plan.first == blocks && plan.drop_until > blocks,
        PlanKind::Merge => {
            let indices = summary_blocks(input, config);
            plan.first == 0
                && indices.len() >= 3
                && plan.drop_until == indices[indices.len() - 3] + 1
        }
    };
    if !range {
        return Err(stale("summary blocks"));
    }
    if config.summary_source(plan.summarized(&plan.source))?.len() > config.max_source_bytes {
        return Err(stale("source size"));
    }
    if config.system_input_budget.is_some_and(|budget| {
        config
            .summary_envelope_bytes(plan.summarized(&plan.source))
            .map_or(true, |size| size > budget)
    }) {
        return Err(stale("summary model budget"));
    }
    if !safe_cut(input, plan) {
        return Err(stale("boundary"));
    }
    let mut seen = HashSet::new();
    let mut pending = HashSet::new();
    for item in &input[..plan.drop_until] {
        match item["type"].as_str() {
            Some("function_call") => match call_id(item) {
                Some(id) if seen.insert(id) => {
                    pending.insert(id);
                }
                _ => return Err(stale("call pairs")),
            },
            Some("function_call_output") => match call_id(item) {
                Some(id) if pending.remove(id) => {}
                _ => return Err(stale("call pairs")),
            },
            _ => {}
        }
    }
    if !pending.is_empty() {
        return Err(stale("call pairs"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    /// Plan validation compares the config the context service echoes back, so a calibrated
    /// `bytes_per_token` must survive JSON exactly; this value does not without `float_roundtrip`.
    #[test]
    fn calibrated_ratios_round_trip_exactly() {
        let ratio = 1.500_096_120_345_751_5_f64;
        let text = serde_json::to_string(&json!({ "bytes_per_token": ratio })).unwrap();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["bytes_per_token"].as_f64(), Some(ratio));
    }

    /// The fullest shortening level is exactly the planner's measure (stubs of the original
    /// outputs), so a plan the planner fitted under the source limit always validates.
    #[test]
    fn stubbed_summary_source_matches_the_planner_measure() {
        let config: super::Config = serde_json::from_value(json!({
            "schema_version": 1, "trigger_tokens": 10000, "tail_tokens": 40, "summary_tokens": 20,
            "characters_per_token": 4, "summary_reserve_bytes": 300, "max_source_bytes": 9000,
            "max_summary_bytes": 300,
            "summary_prefix": "Earlier conversation summary (background context, not a new request):\n",
            "active_summary_prefix": "Active task progress summary (background context, not a new request):\n",
            "summary_prompt": "Summarize under {summary_characters} characters. Conversation JSON: {source}"
        }))
        .unwrap();
        let mut input = vec![json!({"role": "user", "content": "go"})];
        for (n, output) in [
            "\\\"".repeat(9000),
            "é\n\"".repeat(6000),
            "x".repeat(50_000),
        ]
        .iter()
        .enumerate()
        {
            let id = format!("call-{n}");
            input.push(
                json!({"type": "function_call", "call_id": id, "name": "file_read",
                "arguments": "{}"}),
            );
            input.push(json!({"type": "function_call_output", "call_id": id, "output": output}));
        }
        let mut stubbed = input.clone();
        super::project_tool_results(&mut stubbed, super::STUB_PREVIEW_BYTES).unwrap();
        let source = config.summary_source(&input).unwrap();
        let expected = serde_json::to_string(&stubbed).unwrap();
        assert_eq!(source, expected);
    }

    /// One parallel exchange larger than the summary-source limit once stalled every fold
    /// (`no_safe_boundary`); its outputs now enter the summary as previews or stubs.
    #[test]
    fn oversized_tool_exchange_folds_with_shortened_outputs() {
        let config: super::Config = serde_json::from_value(json!({
            "schema_version": 1, "trigger_tokens": 10000, "tail_tokens": 40, "summary_tokens": 20,
            "characters_per_token": 4, "summary_reserve_bytes": 300, "max_source_bytes": 262144,
            "max_summary_bytes": 300,
            "summary_prefix": "Earlier conversation summary (background context, not a new request):\n",
            "active_summary_prefix": "Active task progress summary (background context, not a new request):\n",
            "summary_prompt": "Summarize under {summary_characters} characters. Conversation JSON: {source}"
        }))
        .unwrap();
        let mut input = vec![json!({"role": "user", "content": "read the receipts"})];
        for id in ["a", "b", "c"] {
            input.push(
                json!({"type": "function_call", "call_id": id, "name": "file_read",
                "arguments": "{}"}),
            );
        }
        for id in ["a", "b", "c"] {
            input.push(json!({"type": "function_call_output", "call_id": id,
                "output": "x".repeat(170_000)}));
        }
        input.push(json!({"role": "user", "content": "next"}));
        let plan = super::plan_rolling(&input, 4 * 1024 * 1024, &config)
            .unwrap()
            .unwrap();
        assert_eq!(plan.drop_until, 7);
        let prompt = config.prompt(&plan).unwrap();
        assert!(prompt.contains("context_truncated") && prompt.len() < 262144);
        assert_eq!(
            super::apply_summary(&input, &plan, "done").unwrap().len(),
            2
        );
        // A source that already fits is summarized in full.
        let small = &input[..2];
        assert_eq!(
            config.summary_source(small).unwrap(),
            serde_json::to_string(small).unwrap()
        );
    }

    fn requested_config() -> super::Config {
        serde_json::from_value(json!({
            "schema_version": 1, "trigger_tokens": 10000, "tail_tokens": 40, "summary_tokens": 20,
            "characters_per_token": 4, "summary_reserve_bytes": 300, "max_source_bytes": 9000,
            "max_summary_bytes": 300, "min_fold_tokens": 50,
            "summary_prefix": "Earlier conversation summary (background context, not a new request):\n",
            "active_summary_prefix": "Active task progress summary (background context, not a new request):\n",
            "summary_prompt": "Summarize under {summary_characters} characters. Conversation JSON: {source}"
        }))
        .unwrap()
    }

    fn turn(text: &str) -> Value {
        json!({"role": "user", "content": text})
    }

    /// The panel folds exactly up to the quoted message, keeps earlier blocks byte-identical,
    /// and refuses a boundary inside an open tool exchange or a fold too small to pay off.
    #[test]
    fn plan_until_folds_up_to_the_new_task() {
        let config = requested_config();
        let block = turn(&format!("{}old work", config.summary_prefix));
        let input = vec![
            block.clone(),
            turn(&"earlier task ".repeat(30)),
            json!({"type": "function_call", "call_id": "a", "name": "file_read", "arguments": "{}"}),
            json!({"type": "function_call_output", "call_id": "a", "output": "x".repeat(200)}),
            turn("new task: build the report"),
            json!({"type": "function_call", "call_id": "b", "name": "file_read", "arguments": "{}"}),
        ];
        let plan = super::plan_until(&input, 4, &config).unwrap().unwrap();
        assert_eq!((plan.first, plan.drop_until), (1, 4));
        let applied = super::apply_summary(&input, &plan, "earlier task done").unwrap();
        assert_eq!(applied[0], block);
        assert_eq!(applied[2], input[4]);
        assert!(super::plan_until(&input, 3, &config).is_err());
        let mut small = config.clone();
        small.min_fold_tokens = 1000;
        assert!(super::plan_until(&input, 4, &small).unwrap().is_none());
    }

    /// A fold keeps loaded skills, the latest mode and one combined Main Notes change ahead
    /// of its block; the next fold leaves all of that and the first block byte-identical.
    #[test]
    fn folds_keep_retained_rows_ahead_of_each_block() {
        let config = requested_config();
        let skill = json!({"status":"success","output":{"status":"loaded","text":"SKILL"}});
        let pair = |id: &str, name: &str, output: String| {
            [
                json!({"type":"function_call","call_id":id,"name":name,"arguments":"{}"}),
                json!({"type":"function_call_output","call_id":id,"output":output}),
            ]
        };
        let mut input = vec![turn(&"first task ".repeat(30))];
        input.extend(pair("s", "load_skill", skill.to_string()));
        input.extend(pair("m1", "set_execution_mode", "old".into()));
        input.extend(pair("m2", "set_execution_mode", "new".into()));
        input.extend(pair(
            "main_notes_read-x",
            "main_notes_read",
            "other: +a".into(),
        ));
        input.extend(pair("r", "file_read", "x".repeat(200)));
        input.push(turn("second task"));
        input.extend(pair("w", "main_notes_write", "own: +b".into()));
        let plan = super::plan_until(&input, 11, &config).unwrap().unwrap();
        assert_eq!((plan.first, plan.drop_until), (0, 11));
        let once = super::apply_summary(&input, &plan, "first task done").unwrap();
        let names: Vec<_> = once.iter().map(|item| item["name"].clone()).collect();
        assert_eq!(&once[..2], &input[1..3]);
        assert_eq!(once[3]["output"], "new");
        assert!(once[5]["output"]
            .as_str()
            .unwrap()
            .contains("other: +a\n\nThis session wrote: own: +b"));
        assert!(super::is_summary(&once[6], &config) && once[7] == input[11]);
        assert_eq!(names.iter().filter(|n| *n == "main_notes_write").count(), 0);
        assert_eq!(super::leading_blocks(&once, &config), 7);
        let mut more = once.clone();
        more.push(turn(&"third task ".repeat(30)));
        more.extend(pair("s2", "load_skill", skill.to_string()));
        more.push(turn("fourth task"));
        let plan = super::plan_until(&more, 11, &config).unwrap().unwrap();
        assert_eq!(plan.first, 7);
        let twice = super::apply_summary(&more, &plan, "third task done").unwrap();
        assert_eq!(&twice[..7], &once[..7]);
        assert_eq!(twice[8]["call_id"], "s2");
        assert_eq!(super::summary_blocks(&twice, &config), vec![6, 9]);
    }

    /// Task mode folds a finished task once the task after it starts, cutting at the first
    /// user message after it closed; a task only being discussed keeps its predecessor, and
    /// `retained` keeps that many finished tasks behind the current one.
    #[test]
    fn task_mode_folds_finished_tasks_once_the_next_one_starts() {
        let config = requested_config();
        let write = |id: &str, marks: Value| {
            let output = json!({"status":"success","output":marks}).to_string();
            [
                json!({"type":"function_call","call_id":id,"name":"task_list_write",
                    "arguments":"{}"}),
                json!({"type":"function_call_output","call_id":id,"output":output}),
            ]
        };
        let work = |id: &str| {
            [
                json!({"type":"function_call","call_id":id,"name":"file_read","arguments":"{}"}),
                json!({"type":"function_call_output","call_id":id,"output":"x".repeat(300)}),
            ]
        };
        let mut input = vec![turn("first task")];
        input.extend(write("a1", json!({"created":["s1"],"started":["s1"]})));
        input.extend(work("r1"));
        input.extend(write("a2", json!({"closed":["s1"]})));
        input.push(turn("let us discuss a second task"));
        let discussing = input.len() - 1;
        input.extend(write("b1", json!({"created":["s2"]})));
        // Only one task has started: nothing folds while the next is discussed.
        assert!(super::plan_tasks(&input, 0, &config).unwrap().is_none());
        input.push(turn("go ahead"));
        input.extend(write("b2", json!({"started":["s2"]})));
        input.extend(work("r2"));
        let plan = super::plan_tasks(&input, 0, &config).unwrap().unwrap();
        // The cut is the first user message after s1 closed, so the discussion stays.
        assert_eq!((plan.first, plan.drop_until), (0, discussing));
        assert_eq!(plan.boundary, super::Boundary::PriorTurn);
        // Keeping one finished task behind the current one folds nothing yet.
        assert!(super::plan_tasks(&input, 1, &config).unwrap().is_none());
    }

    /// The agent's fold at a task start cuts at the first user message after the last closed
    /// section, never past the turn that created the new one.
    #[test]
    fn agent_task_fold_cuts_after_the_last_closed_section() {
        let config = requested_config();
        let write = |id: &str, marks: Value| {
            let output = json!({"status":"success","output":marks}).to_string();
            [
                json!({"type":"function_call","call_id":id,"name":"task_list_write",
                    "arguments":"{}"}),
                json!({"type":"function_call_output","call_id":id,"output":output}),
            ]
        };
        let work = |id: &str| {
            [
                json!({"type":"function_call","call_id":id,"name":"file_read","arguments":"{}"}),
                json!({"type":"function_call_output","call_id":id,"output":"x".repeat(300)}),
            ]
        };
        let mut input = vec![turn("first task")];
        input.extend(write("a1", json!({"created":["s1"],"started":["s1"]})));
        input.extend(work("r1"));
        input.extend(write("a2", json!({"closed":["s1"]})));
        input.push(turn("let us discuss a second task"));
        let discussing = input.len() - 1;
        input.extend(write("b1", json!({"created":["s2"]})));
        input.push(turn("go ahead"));
        assert_eq!(
            super::task_fold_cut(&input, "s2", &config).unwrap(),
            Some(discussing)
        );
        // A section created by the starting write itself cuts at the latest turn at most.
        let mut fresh = input[..discussing].to_vec();
        fresh.push(turn("something else entirely"));
        assert_eq!(
            super::task_fold_cut(&fresh, "s9", &config).unwrap(),
            Some(discussing)
        );
        // Nothing earlier than the first turn: nothing to fold.
        assert!(super::task_fold_cut(&input[..1], "s1", &config)
            .unwrap()
            .is_none());
    }

    /// A fold combines the session's task list changes from its range and the tail into one
    /// `task_list_read` exchange ahead of its block, after the Main Notes one.
    #[test]
    fn folds_combine_task_list_changes() {
        let config = requested_config();
        let pair = |id: &str, name: &str, output: &str| {
            [
                json!({"type":"function_call","call_id":id,"name":name,"arguments":"{}"}),
                json!({"type":"function_call_output","call_id":id,"output":output}),
            ]
        };
        let mut input = vec![turn(&"first task ".repeat(30))];
        input.extend(pair("t1", "task_list_write", "s1 added: Work"));
        input.extend(pair("w", "main_notes_write", "own: +b"));
        input.extend(pair("r", "file_read", &"x".repeat(200)));
        input.push(turn("second task"));
        input.extend(pair(
            "task_list_read-u",
            "task_list_read",
            "s1.2 added by the user",
        ));
        let plan = super::plan_until(&input, 7, &config).unwrap().unwrap();
        let once = super::apply_summary(&input, &plan, "first task done").unwrap();
        assert_eq!(once[0]["name"], "main_notes_read");
        assert_eq!(once[2]["name"], "task_list_read");
        assert!(once[2]["call_id"]
            .as_str()
            .unwrap()
            .starts_with("task_list_read-"));
        assert_eq!(
            once[3]["output"],
            "Task list changes after the list in the instructions, oldest first:\n\n\
             This session wrote: s1 added: Work\n\ns1.2 added by the user"
        );
        assert!(super::is_summary(&once[4], &config));
        assert!(!once[5..].iter().any(|item| super::call_id(item)
            .is_some_and(|id| id.starts_with("task_list_read-") && id != once[2]["call_id"])));
        assert_eq!(super::leading_blocks(&once, &config), 5);
    }

    /// A major reorder keeps only the blocks at the start and drops the server's notes rows;
    /// the agent's own writes and skills in the verbatim tail stay where they are.
    #[test]
    fn reorders_keep_blocks_and_the_verbatim_tail() {
        let config = requested_config();
        let block = |n: usize| turn(&format!("{}block {n}", config.summary_prefix));
        let pair = |id: &str, name: &str| {
            [
                json!({"type":"function_call","call_id":id,"name":name,"arguments":"{}"}),
                json!({"type":"function_call_output","call_id":id,"output":"x"}),
            ]
        };
        let mut input = Vec::new();
        input.extend(pair("s1", "load_skill"));
        input.push(block(1));
        input.extend(pair("m1", "set_execution_mode"));
        input.push(block(2));
        input.push(turn("tail"));
        input.extend(pair("main_notes_read-a", "main_notes_read"));
        input.extend(pair("w", "main_notes_write"));
        input.extend(pair("s2", "load_skill"));
        let reordered = super::reorder(&input, &config);
        let ids: Vec<_> = reordered
            .iter()
            .map(|item| item["call_id"].clone())
            .collect();
        assert_eq!(reordered[..2], [block(1), block(2)]);
        assert_eq!(reordered[2], turn("tail"));
        assert_eq!(ids[3..], [json!("w"), json!("w"), json!("s2"), json!("s2")]);
        assert_eq!(super::summary_blocks(&reordered, &config), vec![0, 1]);
    }

    /// A merge keeps the newest two blocks with the rows kept ahead of them.
    #[test]
    fn merges_keep_the_newest_two_blocks_and_their_rows() {
        let config = requested_config();
        let block = |n: usize| {
            turn(&format!(
                "{}block {n} {}",
                config.summary_prefix,
                "x".repeat(400)
            ))
        };
        let row = |id: &str| {
            [
                json!({"type":"function_call","call_id":id,"name":"set_execution_mode","arguments":"{}"}),
                json!({"type":"function_call_output","call_id":id,"output":"mode"}),
            ]
        };
        let mut input = vec![block(1)];
        input.extend(row("a"));
        input.push(block(2));
        input.extend(row("b"));
        input.push(block(3));
        input.push(turn("now"));
        assert_eq!(super::summary_blocks(&input, &config), vec![0, 3, 6]);
        let mut merging = config.clone();
        merging.merge_tokens = 1;
        let plan = super::plan_rolling(&input, 400, &merging).unwrap().unwrap();
        assert_eq!(
            (plan.kind, plan.first, plan.drop_until),
            (super::PlanKind::Merge, 0, 1)
        );
        super::validate_external_plan(&input, &plan, &merging, 400).unwrap();
        let merged = super::apply_summary(&input, &plan, "blocks one").unwrap();
        assert_eq!(&merged[1..], &input[1..]);
    }

    /// `/compact` keeps the configured tail verbatim and folds what lies before it.
    #[test]
    fn plan_command_keeps_the_tail() {
        let config = requested_config();
        let input = vec![
            turn(&"first ".repeat(60)),
            turn(&"second ".repeat(60)),
            turn("third"),
            turn("fourth"),
        ];
        let plan = super::plan_command(&input, &config).unwrap().unwrap();
        assert_eq!((plan.first, plan.drop_until), (0, 2));
        assert!(super::plan_command(&input[2..], &config).unwrap().is_none());
    }
    #[test]
    fn a_repeated_result_does_not_make_the_history_malformed() {
        let call = json!({"type":"function_call","call_id":"m","name":"set_execution_mode","arguments":"{}"});
        let result = json!({"type":"function_call_output","call_id":"m","output":"ok"});
        let user = json!({"role":"user","content":"next"});
        let history = vec![
            json!({"role":"user","content":"first"}),
            call.clone(),
            result.clone(),
            result.clone(),
            user,
        ];
        assert!(super::requested_boundaries(&history, 0).is_some());
        // Saved once ahead of its call, then again after it (sessions before 1b7148e7).
        let early = vec![
            json!({"role":"user","content":"first"}),
            result.clone(),
            call.clone(),
            result.clone(),
            json!({"role":"user","content":"next"}),
        ];
        assert!(super::requested_boundaries(&early, 0).is_some());
        let orphan = vec![json!({"role":"user","content":"first"}), result];
        assert!(super::requested_boundaries(&orphan, 0).is_none());
    }

    #[test]
    fn summaries_omit_skill_text_and_main_notes_updates() {
        let mut input = vec![
            json!({"type":"function_call","call_id":"s","name":"load_skill","arguments":"{}"}),
            json!({"type":"function_call_output","call_id":"s",
                "output":json!({"status":"success","output":{"status":"loaded","text":"SKILL"}}).to_string()}),
            json!({"type":"function_call","call_id":"n","name":"main_notes_write","arguments":"{}"}),
            json!({"type":"function_call_output","call_id":"n","output":"- old line\n+ new line"}),
            json!({"type":"function_call","call_id":"f","name":"file_read","arguments":"{}"}),
            json!({"type":"function_call_output","call_id":"f","output":"kept"}),
        ];
        super::omit_skill_bodies(&mut input);
        let text = json!(input).to_string();
        assert!(!text.contains("SKILL") && text.contains("text_omitted"));
        assert!(
            !text.contains("new line")
                && text.contains("Omitted; kept ahead of this summary block")
        );
        assert_eq!(input[5]["output"], "kept");
    }
}
