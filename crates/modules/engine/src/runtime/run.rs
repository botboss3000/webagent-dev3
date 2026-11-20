//! Engine-owned claimed-run execution, context reconstruction and first-poll/tool fences.
use super::failure::{system_dispatch_run, terminal, validate_system_summary_input};
use super::ports::{
    AgentPolicy, ChatAuthority, ContextDiagnostics, CredentialBindings, FollowupQueue,
    InstallationAccess, MainGuidance, ModelDiagnostics, ModuleBindings, NativePolicy, PromptSlots,
    ProviderUsage, RunObservation, RunPresentation, ScopedStorage, SessionBinding, SkillSources,
    StartupPolicy,
};
use super::{background_compaction, config, models::ModelRecords, Runtime};
use crate::context::abilities::{self, Catalog};
use crate::gates::protocol as gate_protocol;
use crate::gates::{self as gate_registry, controls as gate_controls};
use crate::output as engine_output;
use crate::ports::{AdmissionEffects, ChatEffects, NotificationEffects, ToolJournal, UsageEffects};
use crate::tools::{self, execution as tool_execution};
use crate::{
    context_accounting, context_assembly, context_compaction, context_input, digest, now, provider,
    publication, status, uuid, workflow,
};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use webagent_access as access;

/// Number of the run's latest model turn. Unlike `turns`, the turn budget that a resume
/// resets, it never repeats, so it keys the run's durable model steps.
/// Tools offered and authorized in every session without loading their ability
/// (`core: true` in `<tool>.tool.json`); tool policy still applies.
fn always_on(name: &str) -> bool {
    webagent_abilities::tool_definitions::is_core(name)
}

/// Guidance IDs of skills whose `load_skill` result, text included, is still in `input`.
fn skills_in_history(input: &Value) -> std::collections::HashSet<String> {
    let items = input.as_array().map(Vec::as_slice).unwrap_or_default();
    // Parse only load_skill results: other tool outputs can be large and are never skills.
    let calls: std::collections::HashSet<&str> = items
        .iter()
        .filter(|item| item["type"] == "function_call" && item["name"] == "load_skill")
        .filter_map(|item| item["call_id"].as_str())
        .collect();
    if calls.is_empty() {
        return std::collections::HashSet::new();
    }
    items
        .iter()
        .filter(|item| {
            item["type"] == "function_call_output"
                && item["call_id"]
                    .as_str()
                    .is_some_and(|id| calls.contains(id))
        })
        .filter_map(|item| serde_json::from_str::<Value>(item["output"].as_str()?).ok())
        .filter_map(|result| {
            let skill = &result["output"];
            if skill["status"] != "loaded"
                || skill["context_source"] != "live_skill_catalog"
                || !skill["text"].is_string()
            {
                return None;
            }
            let (ability, name) = (skill["ability_id"].as_str()?, skill["skill_id"].as_str()?);
            Some(match skill["kind"].as_str()? {
                "ability" => format!("ability:{ability}"),
                "own" => format!("own:base:{name}"),
                kind => format!("{kind}:{ability}:{name}"),
            })
        })
        .collect()
}

/// Asking the agent to file an `agent_review` note for a new issue group is switched off:
/// it spends a call in the user's session. A forked reviewer that records the note without
/// touching the session's context is planned; until it is built and validated, issue groups
/// are still recorded but never prompted.
const REVIEW_PROMPTS_ENABLED: bool = false;

/// Add `request` to the newest result of `tool` in the run's history, as `review_request`.
/// False when there is no such result to carry it.
fn attach_review_request(run: &mut Value, tool: &str, request: &str) -> bool {
    let Some(input) = run["input"].as_array_mut() else {
        return false;
    };
    let call_id = input
        .iter()
        .rev()
        .find(|item| item["type"] == "function_call" && item["name"] == tool)
        .and_then(|item| item["call_id"].as_str())
        .map(str::to_owned);
    let Some(call_id) = call_id else {
        return false;
    };
    let Some(output) = input
        .iter_mut()
        .rev()
        .find(|item| item["type"] == "function_call_output" && item["call_id"] == call_id.as_str())
    else {
        return false;
    };
    let Some(mut result) = output["output"]
        .as_str()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .filter(Value::is_object)
    else {
        return false;
    };
    result["review_request"] = json!(request);
    output["output"] = json!(result.to_string());
    true
}

/// Remove the call-scoped note (system/developer messages with no item type) that ends a
/// projected request, before a fork or closing message is appended in its place.
fn drop_call_note(input: &mut Vec<Value>) {
    while input.last().is_some_and(|item| {
        item.get("type").is_none() && matches!(item["role"].as_str(), Some("system" | "developer"))
    }) {
        input.pop();
    }
}

pub fn model_turn(run: &Value) -> u64 {
    run["model_turn"]
        .as_u64()
        .or(run["turns"].as_u64())
        .unwrap_or(0)
}
/// Count one model response against both the turn budget and the step numbering.
pub fn advance_turn(run: &mut Value) {
    run["model_turn"] = json!(model_turn(run) + 1);
    run["turns"] = json!(run["turns"].as_u64().expect("run counters are numbers") + 1);
    super::health::record_step(run);
}
/// Drop each result that does not follow its own unanswered call. Providers and
/// context projection reject such a result; resumed runs used to lose their calls
/// from durable history while keeping the results.
pub fn drop_orphan_outputs(input: &mut Vec<Value>) {
    let mut open = std::collections::HashSet::new();
    input.retain(|item| match item["type"].as_str() {
        Some("function_call") => {
            if let Some(id) = item["call_id"].as_str() {
                open.insert(id.to_owned());
            }
            true
        }
        Some("function_call_output") => item["call_id"].as_str().is_some_and(|id| open.remove(id)),
        _ => true,
    });
}
/// History reaches a new run only after earlier runs ended. A call they left
/// unpaired, for example after abandoned recovery, gets a content-free
/// unknown-outcome result so providers accept the context and nothing is replayed.
pub fn pair_unresolved_calls(input: &mut Vec<Value>) {
    drop_orphan_outputs(input);
    let answered: std::collections::HashSet<String> = input
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .filter_map(|item| item["call_id"].as_str().map(str::to_owned))
        .collect();
    let mut index = 0;
    while index < input.len() {
        if input[index]["type"] != "function_call" {
            index += 1;
            continue;
        }
        // Keep one model turn's calls and results together; add missing results after them.
        let mut end = index;
        while end < input.len()
            && matches!(
                input[end]["type"].as_str(),
                Some("function_call" | "function_call_output" | "reasoning")
            )
        {
            end += 1;
        }
        let missing: Vec<Value> = input[index..end].iter()
            .filter(|item| item["type"] == "function_call")
            .filter_map(|item| item["call_id"].as_str())
            .filter(|id| !answered.contains(*id))
            .map(|id| json!({"type":"function_call_output","call_id":id,"output":json!({
                "status":"interrupted","execution_state":"outcome_unknown","no_retry":true,
                "output":"This call ended without a recorded result. Its outcome is unknown; do not replay it. Check existing effects before further writes."}).to_string()}))
            .collect();
        let added = missing.len();
        input.splice(end..end, missing);
        index = end + added;
    }
}
/// Serialized request bytes per provider token for this call. A previous call in the run
/// that reported usage for the same model gives the measured ratio; otherwise the model's
/// provider file may set one. `None` keeps the app-wide `characters_per_token`.
fn bytes_per_token(run: &Value, selection: &provider::ProviderSelection) -> Option<f64> {
    let last = &run["token_calibration"];
    let reported = run["usage"]["reported_calls"].as_u64();
    let measured = last["model"].as_str() == Some(selection.model.as_str())
        && reported.is_some()
        && reported == last["reported_calls"].as_u64().map(|calls| calls + 1);
    let bytes = last["envelope_bytes"].as_u64();
    let tokens = run["usage"]["latest"]["input_tokens"]
        .as_u64()
        .filter(|tokens| *tokens > 0);
    if let (true, Some(bytes), Some(tokens)) = (measured, bytes, tokens) {
        return Some((bytes as f64 / tokens as f64).clamp(1.0, 16.0));
    }
    webagent_models::providers::characters_per_token(selection.provider, &selection.model)
        .ok()
        .flatten()
}

/// Content-free outline of a history for the operator log: each item's kind, and the
/// call id of tool calls and results so an unpaired one can be spotted.
fn history_shape(history: &[Value]) -> String {
    history
        .iter()
        .take(64)
        .map(|item| {
            let kind = item["type"]
                .as_str()
                .or(item["role"].as_str())
                .unwrap_or("?");
            match item["call_id"].as_str() {
                Some(call) => format!("{kind}({call})"),
                None => kind.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Group digits in threes so byte and token counts read at a glance.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Explain a context assembly failure. A trigger below the fixed overhead gets the measured
/// sizes from the content-free assembly report; other codes keep the short form.
fn assembly_failure_message(
    code: &str,
    stage: &str,
    report: &Value,
    compaction: &context_compaction::Config,
) -> String {
    let short = format!("Context error: {stage}/{code}; review this run");
    if code != "trigger_overhead_exceeds_budget" {
        return short;
    }
    // Same bytes-per-token ratio the planner used for this call.
    let ratio = compaction
        .bytes_per_token
        .unwrap_or(compaction.characters_per_token as f64)
        .max(1.0);
    let tokens = |bytes: u64| (bytes as f64 / ratio).ceil() as u64;
    let sized = |bytes: u64| {
        format!(
            "{} bytes (~{} tokens)",
            grouped(bytes),
            grouped(tokens(bytes))
        )
    };
    let trigger = compaction.trigger_tokens as u64;
    let mut text = format!(
        "Context error: {stage}/{code}. The compaction trigger ({} tokens, {} bytes) is below \
         the fixed overhead every model call resends (instructions, skill guidance and tool \
         schemas). Compaction only summarizes conversation history, so even an empty history \
         would stay over the trigger and no summary was attempted.",
        grouped(trigger),
        grouped(compaction.token_bytes(compaction.trigger_tokens) as u64)
    );
    let field = |name: &str| report[name].as_u64();
    let (Some(instructions), Some(schemas)) = (
        field("instructions_json_bytes"),
        field("schemas_json_bytes"),
    ) else {
        return text;
    };
    let framing = field("framing_bytes").unwrap_or(0);
    let overhead = instructions + schemas + framing;
    text.push_str(&format!(
        " Fixed overhead: {} = instructions and skill guidance {} + tool schemas {} + framing {} bytes.",
        sized(overhead),
        sized(instructions),
        sized(schemas),
        grouped(framing)
    ));
    if let Some(history) = field("history_json_bytes") {
        text.push_str(&format!(" History: {}.", sized(history)));
    }
    if let Some(max) = field("max_bytes") {
        text.push_str(&format!(" Model limit: {}.", sized(max)));
    }
    let needed = tokens(overhead) + (compaction.tail_tokens + compaction.summary_tokens) as u64;
    text.push_str(&format!(
        " Raise the compaction trigger to at least {} tokens (overhead + tail + summary) \
         or load fewer skills and tools, then send a new message.",
        grouped(needed)
    ));
    text
}

/// Agent operators (owner or administrator member) may change the agent's compaction settings.
fn agent_operator(access: &access::authority::LiveAccess) -> bool {
    access.relationship == access::Relationship::Owner
        || (access.agent_administrator && access.relationship == access::Relationship::Member)
}

/// The trigger an operator may approve after `trigger_overhead_exceeds_budget`: the measured
/// fixed overhead plus tail and summary, rounded up to the next 5,000 tokens for growth as
/// skills load. None when it would not fit the model limit or the setting's 1,000,000 cap.
fn trigger_remedy(report: &Value, compaction: &context_compaction::Config) -> Option<u64> {
    let field = |name: &str| report[name].as_u64();
    let overhead = field("instructions_json_bytes")?
        + field("schemas_json_bytes")?
        + field("framing_bytes").unwrap_or(0);
    let ratio = compaction
        .bytes_per_token
        .unwrap_or(compaction.characters_per_token as f64)
        .max(1.0);
    let needed = (overhead as f64 / ratio).ceil() as u64
        + (compaction.tail_tokens + compaction.summary_tokens) as u64;
    let proposed = needed.div_ceil(5000) * 5000;
    let fits = (compaction.token_bytes(proposed as usize) as u64) < field("max_bytes")?;
    (fits && proposed <= 1_000_000).then_some(proposed)
}

/// Leave the agent's staff a message (the `staff_message_send` store) asking an operator to
/// approve a proposed trigger. One message per run; a repeat failure does not add another.
async fn notify_trigger_operators<S: ScopedStorage>(
    store: &S,
    run: &Value,
    agent: &str,
    from: usize,
    to: u64,
) -> Result<()> {
    let rid = run["id"].as_str().unwrap_or("");
    let sid = run["session_id"].as_str().unwrap_or("");
    let id = digest(&json!(["context_remedy", agent, rid]))[..24].to_owned();
    let key = format!("agent:{agent}:staff_message:{id}");
    if store.get(&key).await?.is_some() {
        return Ok(());
    }
    let message = format!(
        "A chat with this agent stopped with a context error: the compaction trigger ({} \
         tokens) is below the fixed instructions and tool overhead every model call resends. \
         An agent operator can approve raising this agent's compaction trigger to {} tokens \
         in the chat (/app?session_id={sid}) or set it in the agent's settings.",
        grouped(from as u64),
        grouped(to)
    );
    store
        .put(
            &key,
            &json!({"id":id,"revision":1,"status":"new","kind":"other","name":"WebAgent",
                "email":"","phone":"","message":message,"session_id":sid,
                "visitor_id":run["requester_user_id"],"created_at":now(),"notes":[]}),
        )
        .await
}

#[cfg(test)]
mod assembly_failure_tests {
    use super::*;
    fn config() -> context_compaction::Config {
        serde_json::from_value(json!({"schema_version":1,"trigger_tokens":60000,
            "tail_tokens":2000,"summary_tokens":4000,"characters_per_token":4,
            "summary_reserve_bytes":16384,"max_source_bytes":262144,"max_summary_bytes":65536,
            "summary_prefix":"","active_summary_prefix":"","summary_prompt":""}))
        .expect("compaction config fixture")
    }
    #[test]
    fn trigger_below_overhead_lists_measured_sizes() {
        let report = json!({"instructions_json_bytes":139609,"schemas_json_bytes":101499,
            "framing_bytes":35,"history_json_bytes":193265,"max_bytes":991232});
        let text = assembly_failure_message(
            "trigger_overhead_exceeds_budget",
            "compaction_plan",
            &report,
            &config(),
        );
        for part in [
            "Context error: compaction_plan/trigger_overhead_exceeds_budget.",
            "compaction trigger (60,000 tokens, 240,000 bytes)",
            "Fixed overhead: 241,143 bytes (~60,286 tokens)",
            "instructions and skill guidance 139,609 bytes (~34,903 tokens)",
            "tool schemas 101,499 bytes (~25,375 tokens)",
            "History: 193,265 bytes (~48,317 tokens).",
            "Model limit: 991,232 bytes (~247,808 tokens).",
            "at least 66,286 tokens",
        ] {
            assert!(text.contains(part), "{part} missing from {text}");
        }
    }
    #[test]
    fn remedy_rounds_overhead_tail_and_summary_up_to_5000() {
        let report = json!({"instructions_json_bytes":139609,"schemas_json_bytes":101499,
            "framing_bytes":35,"max_bytes":991232});
        // 60,286 overhead + 2,000 tail + 4,000 summary = 66,286 -> 70,000.
        assert_eq!(trigger_remedy(&report, &config()), Some(70000));
        let mut small = report.clone();
        small["max_bytes"] = json!(280000);
        assert_eq!(trigger_remedy(&small, &config()), None);
        assert_eq!(trigger_remedy(&Value::Null, &config()), None);
    }
    #[test]
    fn missing_report_and_other_codes_stay_short() {
        let text = assembly_failure_message(
            "trigger_overhead_exceeds_budget",
            "compaction_plan",
            &Value::Null,
            &config(),
        );
        assert!(text.contains("(60,000 tokens, 240,000 bytes)"));
        assert!(!text.contains("Fixed overhead"));
        assert_eq!(
            assembly_failure_message(
                "no_safe_boundary",
                "compaction_plan",
                &Value::Null,
                &config()
            ),
            "Context error: compaction_plan/no_safe_boundary; review this run"
        );
    }
}

#[cfg(test)]
mod history_journal_tests {
    use super::*;
    #[test]
    fn loaded_skill_text_in_history_is_found_until_compacted() {
        let result = |kind: &str, text: Option<&str>| {
            let mut output = json!({"status":"loaded","context_source":"live_skill_catalog",
                "ability_id":"files","kind":kind,"skill_id":"file_read"});
            if let Some(text) = text {
                output["text"] = json!(text);
            }
            json!({"type":"function_call_output","call_id":kind,
                "output":json!({"status":"success","output":output}).to_string()})
        };
        let call = |id: &str, name: &str| json!({"type":"function_call","call_id":id,"name":name});
        let input = json!([
            call("tool", "load_skill"),
            result("tool", Some("body")),
            call("selectable", "load_skill"),
            result("selectable", None)
        ]);
        let found = skills_in_history(&input);
        assert!(found.contains("tool:files:file_read"));
        // A summary keeps no text, so the skill is no longer in the history.
        assert!(!found.contains("selectable:files:file_read"));
        assert!(skills_in_history(&json!([])).is_empty());
        // Only load_skill results count, whatever another tool's output contains.
        let other = json!([call("tool", "file_read"), result("tool", Some("body"))]);
        assert!(skills_in_history(&other).is_empty());
    }
    #[test]
    fn call_note_is_dropped_before_a_fork_or_closing_message() {
        let mut input = vec![
            json!({"role":"user","content":"hi"}),
            json!({"type":"function_call_output","call_id":"a","output":"x"}),
            json!({"role":"system","content":"publish guidance"}),
        ];
        drop_call_note(&mut input);
        assert_eq!(input.len(), 2);
        // Nothing else is removed: a history ending in a result or user message stays.
        drop_call_note(&mut input);
        assert_eq!(input.len(), 2);
    }
    #[test]
    fn review_request_rides_on_the_newest_result_of_the_failed_tool() {
        let call = |id: &str, name: &str| json!({"type":"function_call","call_id":id,"name":name});
        let out = |id: &str| {
            json!({"type":"function_call_output","call_id":id,
                "output":json!({"status":"error","output":"boom"}).to_string()})
        };
        let mut run = json!({"input":[call("a","web_read"), out("a"), call("b","file_read"),
            out("b"), call("c","web_read"), out("c")]});
        assert!(attach_review_request(
            &mut run,
            "web_read",
            "call agent_review"
        ));
        let result = |index: usize| -> Value {
            serde_json::from_str(run["input"][index]["output"].as_str().unwrap()).unwrap()
        };
        assert_eq!(result(5)["review_request"], "call agent_review");
        assert_eq!(result(5)["output"], "boom");
        assert!(result(1).get("review_request").is_none());
        assert!(result(3).get("review_request").is_none());
        // No result of that tool: the caller falls back to a one-off note.
        assert!(!attach_review_request(
            &mut run,
            "shell",
            "call agent_review"
        ));
    }
    #[test]
    fn unresolved_history_calls_get_one_unknown_outcome_result_inside_their_turn() {
        let call = |id: &str| json!({"type":"function_call","call_id":id,"name":"file_write","arguments":"{}"});
        let output = |id: &str| json!({"type":"function_call_output","call_id":id,"output":"ok"});
        let mut input = vec![
            json!({"role":"user","content":"a"}),
            call("done"),
            output("done"),
            json!({"role":"user","content":"b"}),
            call("lost-1"),
            call("lost-2"),
            output("lost-2"),
            json!({"role":"user","content":"c"}),
        ];
        pair_unresolved_calls(&mut input);
        let types: Vec<_> = input
            .iter()
            .map(|i| {
                i["type"]
                    .as_str()
                    .or(i["role"].as_str())
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            types,
            [
                "user",
                "function_call",
                "function_call_output",
                "user",
                "function_call",
                "function_call",
                "function_call_output",
                "function_call_output",
                "user"
            ]
        );
        assert_eq!(input[7]["call_id"], "lost-1");
        assert!(input[7]["output"]
            .as_str()
            .unwrap()
            .contains("outcome_unknown"));
        let once = input.clone();
        pair_unresolved_calls(&mut input);
        assert_eq!(input, once);
    }
    #[test]
    fn results_without_a_preceding_open_call_are_dropped() {
        let call = |id: &str| json!({"type":"function_call","call_id":id,"name":"file_read","arguments":"{}"});
        let output = |id: &str| json!({"type":"function_call_output","call_id":id,"output":"ok"});
        let mut input = vec![
            json!({"role":"user","content":"a"}),
            output("lost"),
            call("kept"),
            output("kept"),
            output("kept"),
            output("early"),
            call("early"),
        ];
        pair_unresolved_calls(&mut input);
        let ids: Vec<_> = input
            .iter()
            .filter_map(|item| Some((item["type"].as_str()?, item["call_id"].as_str()?)))
            .collect();
        assert_eq!(
            ids,
            [
                ("function_call", "kept"),
                ("function_call_output", "kept"),
                ("function_call", "early"),
                ("function_call_output", "early"),
            ]
        );
        assert!(input[4]["output"]
            .as_str()
            .unwrap()
            .contains("outcome_unknown"));
    }
}

impl<
        S: ScopedStorage
            + ModelRecords
            + ToolJournal
            + ChatEffects
            + AdmissionEffects
            + NotificationEffects
            + UsageEffects
            + RunObservation
            + RunPresentation,
        C: config::SchemaCatalog,
        B: Catalog,
        A: super::actions::Actions<S> + Clone + PartialEq,
        H: ChatAuthority<S>,
        M: ModuleBindings<S>,
        X: AgentPolicy<S>
            + NativePolicy
            + CredentialBindings<S>
            + SkillSources<S>
            + PromptSlots<S>
            + MainGuidance<S>
            + ModelDiagnostics<S>
            + InstallationAccess<S>
            + ContextDiagnostics<S>
            + ProviderUsage
            + StartupPolicy<S>
            + FollowupQueue<S>
            + super::native::NativeDispatch<S, C, B, H::Binding>,
    > Runtime<S, C, B, A, H, M, X>
{
    pub fn append_tool(run: &mut Value, result: &Value) -> Result<()> {
        workflow::record_tool_result(run, result)?;
        let previous_tools = run["selected_tools"].clone();
        let previous_skills = run["loaded_skills"].clone();
        X::apply_skill_selection(run, result);
        crate::context_assets::record_tool_operation(
            run,
            result,
            &previous_tools,
            &previous_skills,
        )?;
        crate::tool_output::record(run, result)?;
        super::health::record_result(run, result);
        let call_id = run["pending"]["call_id"].clone();
        run["input"]
        .as_array_mut()
        .expect("run input is an array")
        .push(json!({"type":"function_call_output","call_id":call_id,"output":result.to_string()}));
        run["call_index"] = json!(
            run["call_index"]
                .as_u64()
                .expect("run counters are numbers")
                + 1
        );
        run["pending"] = Value::Null;
        Ok(())
    }
    // A read-only tool can be interrupted after the model call was journaled. Pair
    // that call with a content-free error result so any later context remains valid.
    // Writes retain the existing uncertainty/recovery behavior because their effects
    // cannot be inferred from a lost transport response.
    // Fixed reason codes only; raw worker errors stay private.
    pub fn record_tool_interruption(run: &mut Value, reason: &str) {
        let reason = match reason {
            "timeout" => "timeout",
            "cancelled" => "cancelled",
            "transport_error" => "transport_error",
            _ => "uncertain",
        };
        run["pending"]["interruption"] = json!({"reason":reason,"recorded_at":now()});
    }
    pub fn record_interrupted_tool(run: &mut Value, name: &str) -> Result<Option<Value>> {
        if A::is_read(name) {
            let result = json!({"status":"error",
            "output":"Read interrupted before a result was received; no data was returned."});
            Self::append_tool(run, &result)?;
            run["status"] = json!("failed");
            run["error"] = json!("Read tool interrupted");
            Ok(Some(result))
        } else {
            let reason = tools::failure::interruption_reason(&run["pending"]["failure"]);
            Self::record_tool_interruption(run, reason);
            run["uncertain"] = json!(true);
            run["status"] = json!("recovery");
            run["error"] = json!("Tool outcome unknown; review before continuing");
            Ok(None)
        }
    }

    /// Start a background summary for `fold` beside the main call. The same-model summary
    /// copies the running request and adds its message, so the provider reuses its cached
    /// prefix; a bridge answers it from a fork of the chat's harness session. Muse (which
    /// cannot fork), a bridge with no fork-capable client, a request that would not fit, or a
    /// failed same-model summary use System. A summary that cannot be prepared is skipped:
    /// the ceiling still folds.
    #[allow(clippy::too_many_arguments)]
    async fn start_background_compaction(
        app: &Self,
        run: &mut Value,
        fold: BackgroundFold,
        request: BackgroundRequest<'_>,
        config: &context_compaction::Config,
        workflow_policy: &workflow::Policy,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let BackgroundFold {
            plan,
            message,
            reason,
            focus,
        } = fold;
        // The Identity page's summarizer setting decides, for the trigger, a requested fold, a
        // retry and the hard limit alike: same model forks the chat's (cached) session.
        // On a bridge, after `fork_attempts` failed forks in a row the selected model gets a
        // new session with the summary request instead of another fork.
        let forks_exhausted =
            request.selection.provider.is_bridge() && request.fork_failures >= config.fork_attempts;
        let same_model = config.summarizer == context_compaction::Summarizer::SameModel
            && !forks_exhausted
            && (!request.selection.provider.is_bridge()
                || request.selection.provider.bridge_forks())
            && request.envelope_bytes + message.len() + config.token_bytes(config.summary_tokens)
                <= request.ceiling;
        let fresh_same_model = config.summarizer == context_compaction::Summarizer::SameModel
            && !same_model
            && forks_exhausted;
        let (selection, input, tools, instructions, effort, token_run) = if same_model {
            let mut input = request.input.to_vec();
            // The call's own note ends the request; the summary prompt replaces it, so a
            // bridge fork sees the same history its session recorded.
            drop_call_note(&mut input);
            // The history stays exactly as the chat sent it (a changed prefix loses the provider's
            // cache and a bridge refuses to fork it); the request says what to leave out instead.
            input.push(json!({"role":"user","content":format!(
                "{message}\n\nDo not copy loaded skill texts, Main Notes updates or execution mode \
                 guidance into the summary: they are restored after compaction."
            )}));
            (
                request.selection.clone(),
                input,
                request.schemas.to_vec(),
                request.instructions.to_owned(),
                request.effort.map(str::to_owned),
                run.clone(),
            )
        } else {
            // The selected model: System, or the chat's own model in a new session once its
            // forks keep failing.
            let (system_run, selection) = if fresh_same_model {
                (run.clone(), request.selection.clone())
            } else {
                let Ok(system_run) = system_dispatch_run(run) else {
                    return Ok(());
                };
                let selection = provider::ProviderSelection::parse(
                    &system_run["provider_selection"],
                    X::default_model(),
                )?;
                (system_run, selection)
            };
            let summary_source = app.dedup_summary_source(run, &plan.source).await?;
            let Ok(prompt) = config
                .prompt_for(&plan, &summary_source)
                .or_else(|_| config.prompt_for(&plan, &plan.source))
                .map(|prompt| background_compaction::with_focus(prompt, focus.as_deref()))
            else {
                return Ok(());
            };
            let input = vec![json!({"role":"user","content":prompt})];
            let instructions = provider::Provider::instructions_for_snapshot(
                provider::DEFAULT_INSTRUCTIONS,
                crate::ports::AgentMetadata::root(app.store.records()),
                app.abilities.source_root(),
            )?;
            let context = config::Config::<C>::read(&app.runtime_path)?.context;
            if validate_system_summary_input(
                &context,
                &selection,
                &input,
                &instructions,
                workflow_policy.max_context_bytes,
            )
            .is_err()
            {
                return Ok(());
            }
            (selection, input, Vec::new(), instructions, None, system_run)
        };
        if !app.run_model_authorized(&token_run).await {
            return Ok(());
        }
        let summarizer = if same_model || fresh_same_model {
            "self"
        } else {
            "system"
        };
        let bridge_fork = same_model && selection.provider.is_bridge();
        let step = format!(
            "compaction:{}:{}",
            run["compactions"].as_u64().unwrap_or(0) + 1,
            uuid::Uuid::new_v4()
        );
        let input_bytes: usize = input
            .iter()
            .map(|item| item["content"].as_str().map_or(0, str::len))
            .sum();
        compaction_activity(
            run,
            format!("{step}:call"),
            json!({"type":"compaction_call","model":selection.model,"input_bytes":input_bytes,
                "reason":reason,"focus":focus,"mode":"background","summarizer":summarizer,
                "new_session":fresh_same_model,
                "fork":bridge_fork}),
        );
        app.save(run).await?;
        let sid = request.sid.to_owned();
        let mut job = json!({"state":"running","plan":plan,"step":step,"summarizer":summarizer,
            "fork":bridge_fork,"fork_failures":request.fork_failures,
            "reason":reason});
        app.store
            .put(&background_compaction::key(&sid), &job)
            .await?;
        let done = background_compaction::register(&sid);
        let app = app.clone();
        let cancel = cancel.child_token();
        tokio::spawn(async move {
            let response = async {
                let prepared =
                    crate::provider::CallDraft::new(&selection, input, tools, &sid, &instructions)
                        .with_reasoning_effort(effort.as_deref())
                        .with_bridge_fork(bridge_fork)
                        .prepare(
                            crate::ports::AgentMetadata::root(app.store.records()),
                            &cancel,
                        )
                        .await?;
                let tokens = app.tokens_for(&selection, &token_run).await?;
                let ready = prepared.bind_credentials(&tokens, &cancel)?;
                let (sender, mut receiver) = tokio::sync::mpsc::channel::<String>(32);
                let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
                let response = app.provider.dispatch(ready, sender, cancel.clone()).await;
                let _ = drain.await;
                response
            }
            .await;
            match &response {
                Ok(value) => match background_compaction::summary_text(value) {
                    Some(summary) => {
                        job["state"] = json!("done");
                        job["summary"] = json!(summary);
                        job["usage"] = value["usage"].clone();
                        job["cached_input_tokens"] =
                            json!(background_compaction::cached_tokens(value));
                    }
                    None => {
                        job["state"] = json!("failed");
                        job["error"] = json!("The summary was empty or called a tool");
                    }
                },
                Err(error) => {
                    job["state"] = json!("failed");
                    job["error"] = json!(error.to_string());
                }
            }
            if let Err(error) = app.store.put(&background_compaction::key(&sid), &job).await {
                eprintln!("Background summary result for session {sid} not saved: {error}");
            }
            background_compaction::finish(&sid, done);
        });
        Ok(())
    }

    // Recheck all context authority after credential retrieval, under the dispatch gate.
    async fn dispatch_asset_key(
        app: &Self,
        sid: &str,
        agent_id: &str,
        loaded: &Value,
        selected: &Value,
        skills: &Value,
        optimization: &Value,
        requester: &str,
    ) -> Result<String> {
        app.context_fingerprint(
            sid,
            agent_id,
            loaded,
            selected,
            skills,
            optimization,
            requester,
        )
        .await
    }
    /// Invalid host input pauses for review; unavailable pure services retain accepted work.
    /// The stage and error are logged for operators and kept as the run's visible error.
    /// Context preparation precedes every model dispatch and tool effect, so the
    /// last saved checkpoint is a safe resume point. A failure caused by this
    /// generation's cancellation (runtime restart, stop or lost lease)
    /// is not a context fault: the in-flight preparation is discarded and the run
    /// stays resumable instead of surfacing a manual-continue pause.
    async fn pause_context_preparation(
        app: &Self,
        run: &mut Value,
        cancel: &CancellationToken,
        stage: &'static str,
        error: &anyhow::Error,
    ) -> Result<()> {
        if let Some(object) = run.as_object_mut() {
            object.remove("_context_assets_write");
            object.remove("_context_assets_operation");
        }
        if cancel.is_cancelled() {
            // Restart: keep `running` so startup recovery requeues it silently.
            if Self::preserve_restart_interruption(app, run).await? {
                return Ok(());
            }
            // Lease loss or a concurrent stop: the new owner or lease recovery
            // decides. Leave the checkpoint untouched rather than pausing it.
            tracing::info!(run_id=%run["id"].as_str().unwrap_or("unknown"),
                "Context preparation at {stage} discarded after cancellation");
            return Ok(());
        }
        if !crate::components::available(crate::ports::AgentMetadata::root(app.store.records())) {
            eprintln!(
                "Context preparation at {stage} paused run {} for an unavailable engine component: {error:#}",
                run["id"].as_str().unwrap_or("unknown")
            );
            return app.component_pause(run).await;
        }
        eprintln!(
            "Context preparation paused run {} at {stage}: {error:#}",
            run["id"].as_str().unwrap_or("unknown")
        );
        run["status"] = json!("paused");
        run["component_pause"] = Value::Null;
        run["error"] = json!(format!("Context preparation failed at {stage}: {error}"));
        app.save(run).await
    }
    /// Bound freshness reconstruction independently of normal model/tool turn limits.
    async fn rebuild_context(app: &Self, run: &mut Value) -> Result<bool> {
        let count = run["context_rebuilds"].as_u64().unwrap_or(0) + 1;
        run["context_rebuilds"] = json!(count);
        if count >= 3 {
            run["status"] = json!("paused");
            run["error"] = json!("Context changed repeatedly before dispatch; review and resume");
        }
        app.save(run).await?;
        Ok(count < 3)
    }
    // Process shutdown is not an intentional run stop. Keep the claimed checkpoint
    // recoverable; startup owns access checks, safe-read retry and uncertain writes.
    pub async fn preserve_restart_interruption(app: &Self, run: &mut Value) -> Result<bool> {
        let draining = !app.ready.load(std::sync::atomic::Ordering::Acquire);
        if !(app.stop.is_cancelled() || draining)
            || status(run) != "running"
            || app.shutoff().await?
        {
            return Ok(false);
        }
        run["restart_interruption"] = json!({"kind":"runtime_restart","recorded_at":now()});
        run["error"] = json!("Paused for app update; resumes automatically");
        app.save(run).await?;
        Ok(true)
    }

    // Reads fail here only when cancelled.
    pub async fn commit_tool_transport_failure(
        app: &Self,
        run: &mut Value,
        name: &str,
    ) -> Result<()> {
        let id = run["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing run id"))?
            .to_owned();
        if let Some(receipt) = Self::record_interrupted_tool(run, name)? {
            app.store
                .save_run_observed(
                    run,
                    "tool_result",
                    &json!({"run_id":id,"name":name,"result":receipt}),
                )
                .await?;
        } else {
            app.store
                .save_run_observed(run, "run_status", &json!({"run_id":id}))
                .await?;
        }
        Ok(())
    }

    /// Release disposable component accounting whenever an execution yields or terminates.
    pub async fn execute_run(
        app: Self,
        id: String,
        generation: String,
        cancel: CancellationToken,
    ) -> Result<()> {
        let run_key = format!("{id}:{generation}");
        let result = Box::pin(Self::execute_run_inner(app, id, generation, cancel)).await;
        crate::components::release_run(&run_key).await;
        result
    }
    /// Test-only access to the real unboxed inner execution future; no surrogate algorithm or size wrapper.
    #[cfg(any(test, feature = "test-support"))]
    pub fn fixture_execute_run_inner(
        app: Self,
        id: String,
        generation: String,
        cancel: CancellationToken,
    ) -> impl std::future::Future<Output = Result<()>> {
        Self::execute_run_inner(app, id, generation, cancel)
    }
    /// Give a detached call its stand-in result; the repeat guard holds an identical call
    /// until the real result arrives.
    async fn detached_result(
        app: &Self,
        mut run: Value,
        name: &str,
        args: &Value,
        id: &str,
        task_id: &str,
    ) -> Result<std::ops::ControlFlow<()>> {
        let call_id = run["pending"]["call_id"].as_str().unwrap_or("").to_owned();
        tools::retry_guard::mark_running(&mut run, name, args, &call_id, task_id, &A::retry(name));
        let result = json!({"status":"running","call_id":task_id,"output":format!(
            "Still running as background call {task_id}: the user sent a message, so this \
             call continues in the background while you answer it. Its result is delivered \
             to you automatically when it finishes. Do not repeat this call; if the user \
             asks to stop it, cancel it with background_calls_cancel.")});
        Self::append_tool(&mut run, &result)?;
        app.store
            .save_run_observed(
                &mut run,
                "tool_result",
                &json!({"run_id":id,"name":name,"result":result}),
            )
            .await?;
        Ok(std::ops::ControlFlow::Continue(()))
    }
    // Keep tool execution in its own poll frame; the model phase retains its own loop state.
    async fn execute_tool_step(
        app: &Self,
        mut run: Value,
        _guard: tokio::sync::MutexGuard<'_, ()>,
        workflow_policy: &workflow::Policy,
        access: &access::authority::LiveAccess,
        sid: &String,
        id: &String,
        generation: &String,
        cancel: &CancellationToken,
        agent_id: &str,
    ) -> Result<std::ops::ControlFlow<()>> {
        // A user message arrived before these calls started: skip every remaining call so
        // the model sees the message first and re-issues whatever still applies.
        if run["pending"].is_null() && run["batch"].is_null() && app.message_pending(&run).await? {
            let idx = run["call_index"]
                .as_u64()
                .expect("run counters are numbers") as usize;
            let calls = run["calls"].as_array().cloned().unwrap_or_default();
            for call in calls.iter().skip(idx) {
                let result = json!({"status":"not_run","output":
                    "Requested but not run: the user sent a message before this call started. \
                     Read the message, then issue the call again if it still applies."});
                run["pending"] = json!({"call_id":call["call_id"],"name":call["name"]});
                Self::append_tool(&mut run, &result)?;
                app.store
                    .save_run_observed(
                        &mut run,
                        "tool_result",
                        &json!({"run_id":id,"name":call["name"],"result":result}),
                    )
                    .await?;
            }
            if idx < calls.len() {
                return Ok(std::ops::ControlFlow::Continue(()));
            }
        }
        if run["calls"].as_array().is_some_and(|calls| {
            calls.len() > 1
                && !calls
                    .iter()
                    .any(|call| X::declares(call["name"].as_str().unwrap_or("")))
                && !calls.iter().any(|call| {
                    matches!(
                        call["name"].as_str(),
                        Some(
                            "publish_main_message"
                                | "v3_build_work"
                                | "build_work"
                                | "set_execution_mode"
                        )
                    )
                })
                && !calls
                    .iter()
                    .any(|call| super::batch::serial_discovery(call["name"].as_str().unwrap_or("")))
        }) {
            // NATIVE-GATE-SERIAL: private controls keep provider order and
            // never enter a parallel batch or the Python tool worker.
            // Discovery changes what later calls may use, and a mode change how
            // they are authorized (entering Auto needs its human checkpoint), so
            // a batch that contains either runs serially.
            Self::execute_parallel_batch(
                &app,
                &id,
                &generation,
                &cancel,
                &workflow_policy,
                &access,
                run,
                _guard,
            )
            .await?;
            return Ok(std::ops::ControlFlow::Continue(()));
        }
        let idx = run["call_index"]
            .as_u64()
            .expect("run counters are numbers") as usize;
        let calls = run["calls"].as_array().expect("run calls are an array");
        if idx >= calls.len() {
            run["phase"] = json!("model");
            app.save(&mut run).await?;
            return Ok(std::ops::ControlFlow::Continue(()));
        }
        let call = calls[idx].clone();
        let name = call["name"].as_str().unwrap_or("").to_owned();
        let args = match call["arguments"]
            .as_str()
            .map(serde_json::from_str::<Value>)
        {
            Some(Ok(v)) if v.is_object() => v,
            _ => json!({"invalid_arguments":true}),
        };
        if name == "publish_main_message" {
            // NATIVE-CONTROL-DISPATCH: reserved calls never request approval
            // or enter the Python worker. Rust checks activation and arguments.
            let call_id = call["call_id"].as_str().filter(|value| {
                !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
            });
            let Some(call_id) = call_id else {
                run["status"] = json!("paused");
                run["error"] = json!("Invalid private control call ID");
                app.save(&mut run).await?;
                return Ok(std::ops::ControlFlow::Break(()));
            };
            let enabled =
                gate_registry::selected(&workflow_policy, workflow::Checkpoint::ModelDispatch)?
                    .iter()
                    .any(|item| item.id == gate_registry::MESSAGE_VISIBILITY)
                    && matches!(run["promotion_mode"].as_str(), Some("manual" | "auto"));
            workflow::record(
                &mut run,
                &workflow_policy,
                &workflow::Input::PreTool(workflow::PreToolInput {
                    name: name.clone(),
                    arguments_digest: digest(&args),
                    policy_version: 0,
                    mode_revision: 0,
                    visibility_revision: 0,
                    ability_state_revision: 0,
                }),
                workflow::Decision::Pass,
            )?;
            run["pending"] = json!({"call_id":call_id,"name":name});
            let mut working_text = None;
            let mut echoed = Vec::new();
            let result = if !enabled {
                json!({"status":"denied","output":"Private control is not enabled"})
            } else if run["promotion_mode"] == "auto" {
                // Every message is already shown to the user; the call changes nothing.
                json!({"status":"ok","output":"Every message is already shown to the user; nothing to publish."})
            } else {
                let (preceding, ordinals) = engine_output::preceding_message(
                    run["input"].as_array().map_or(&[][..], Vec::as_slice),
                    call_id,
                );
                match gate_controls::working_text(&args, &preceding) {
                    Ok(text) => {
                        if args.get("text").is_none() {
                            echoed = ordinals;
                        }
                        working_text = Some(text);
                        json!({"status":"ok","output":"Working message published"})
                    }
                    Err(error) => json!({"status":"denied",
                        "output":format!("Invalid working message: {error}")}),
                }
            };
            Self::append_tool(&mut run, &result)?;
            if let Some(text) = working_text {
                let mut message_id = format!("working:{id}:{}:{idx}", model_turn(&run));
                // `:echo:<ordinals>` names the reply messages this bubble repeats word for word,
                // so the activity log hides them (src/storage/activity.rs reads it back).
                let echo = echoed
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join("-");
                if !echo.is_empty() && message_id.len() + echo.len() + 6 <= 256 {
                    message_id = format!("{message_id}:echo:{echo}");
                }
                app.store
                    .save_run_observed(
                        &mut run,
                        "working_message",
                        &json!({"id":message_id,"text":text,"replaces_live":true}),
                    )
                    .await?;
            } else {
                app.store
                    .save_run_observed(
                        &mut run,
                        "tool_result",
                        &json!({"run_id":id,"name":name,"result":result}),
                    )
                    .await?;
            }
            return Ok(std::ops::ControlFlow::Continue(()));
        }
        let (mode_revision, mut visibility, ability_state) = app
            .ability_controls_for(
                &sid,
                run["requester_user_id"]
                    .as_str()
                    .unwrap_or(app.store.current_user_id()),
            )
            .await?;
        let mode = app.session_mode(&sid).await?;
        let authorization_mode = mode.clone();
        let mut effective = if always_on(&name) {
            access.tool_decision(&name, true, true)
        } else {
            app.live_tool_decision(
                &access,
                &name,
                &visibility,
                &ability_state,
                &run["loaded_abilities"],
            )
        };
        effective = effective
            .for_execution_mode(&authorization_mode)
            .for_schedule_approval(&authorization_mode, &name, &args);
        workflow::record(
            &mut run,
            &workflow_policy,
            &workflow::Input::PreTool(workflow::PreToolInput {
                name: name.clone(),
                arguments_digest: digest(&args),
                policy_version: 0,
                mode_revision,
                visibility_revision: visibility["revision"].as_u64().unwrap_or(0),
                ability_state_revision: ability_state["revision"].as_u64().unwrap_or(0),
            }),
            workflow::Decision::Pass,
        )?;
        if run["pending"].is_null() {
            run["pending"] = json!({"id":uuid(),"name":name,"arguments":args,"call_id":call["call_id"],"digest":digest(&args),
                    "mode_revision":mode_revision,
                    "visibility_revision":visibility["revision"],
                    "ability_state_revision":ability_state["revision"],
                    "access_revision":access.revision.clone(),
                    "state":"requested","decision":null});
        }
        match app
            .native
            .resolve(app.store.records(), &run, &name, &args)
            .await
        {
            Ok(Some(binding)) => {
                if run["pending"]["authorization"] != binding {
                    run["pending"]["decision"] = Value::Null;
                    run["pending"]["id"] = json!(uuid());
                }
                run["pending"]["authorization"] = binding;
                if effective.permission != access::Permission::Deny {
                    effective.permission = access::Permission::Ask;
                }
            }
            Ok(None) => {}
            Err(_) => {
                let result = json!({"status":"denied","output":"The requesting user must configure their own credential before authorizing this tool"});
                Self::append_tool(&mut run, &result)?;
                app.store
                    .save_run_observed(
                        &mut run,
                        "tool_result",
                        &json!({"run_id":id,"name":name,"result":result}),
                    )
                    .await?;
                return Ok(std::ops::ControlFlow::Continue(()));
            }
        }
        // Entering Auto waives later approvals, so the switch itself crosses a human
        // checkpoint even when the level otherwise allows the call automatically.
        if name == "set_execution_mode"
            && args["mode"] == "auto"
            && authorization_mode["execution_mode"] != "auto"
            && effective.permission != access::Permission::Deny
        {
            effective.permission = access::Permission::Ask;
        }
        // Leaving Plan hands over the agreed plan, which the user sees on the approval
        // card; a call without one is returned to the agent and never reaches the user.
        if name == "set_execution_mode"
            && authorization_mode["execution_mode"] == "plan"
            && args["mode"] != "plan"
            && !args["plan"].is_object()
        {
            app.actions
                .not_dispatched(app.store.records(), &run, &run["pending"])
                .await?;
            let result = json!({"status":"rejected","error_kind":"plan_required",
                "output":"Leaving Plan mode needs the agreed plan: call set_execution_mode again \
                    with plan (title, goal, criteria, notes and checklist)."});
            Self::append_tool(&mut run, &result)?;
            app.store
                .save_run_observed(
                    &mut run,
                    "tool_result",
                    &json!({"run_id":id,"name":name,"result":result}),
                )
                .await?;
            return Ok(std::ops::ControlFlow::Continue(()));
        }
        if run["pending"]["access_revision"] != json!(access.revision)
            || run["pending"]["mode_revision"].as_u64().unwrap_or(0) != mode_revision
            || run["pending"]["visibility_revision"].as_u64().unwrap_or(0)
                != visibility["revision"]
                    .as_u64()
                    .expect("state revisions are numbers")
            || run["pending"]["ability_state_revision"]
                .as_u64()
                .unwrap_or(0)
                != ability_state["revision"]
                    .as_u64()
                    .expect("state revisions are numbers")
        {
            run["pending"]["id"] = json!(uuid());
            run["pending"]["decision"] = Value::Null;
            run["pending"]["access_revision"] = json!(access.revision.clone());
            run["pending"]["mode_revision"] = json!(mode_revision);
            run["pending"]["visibility_revision"] = visibility["revision"].clone();
            run["pending"]["ability_state_revision"] = ability_state["revision"].clone();
        }
        if effective.permission == access::Permission::Deny
            || run["pending"]["decision"] == "deny"
            || args.get("invalid_arguments").is_some()
        {
            run["workflow"]["receipts"]["pre_tool"]["result"] = json!(workflow::Decision::Reject {
                reason: "Tool invocation denied by policy or invalid arguments".into()
            });
            app.actions
                .not_dispatched(app.store.records(), &run, &run["pending"])
                .await?;
            let message = if run["pending"]["decision"] == "deny" {
                "Tool invocation denied: the user denied this call".to_owned()
            } else {
                super::batch::denial_message(
                    effective.reason,
                    args.get("invalid_arguments").is_some(),
                )
            };
            let result = match tools::retry_guard::declined(&run["pending"]) {
                Some(result) if run["pending"]["decision"] == "deny" => result,
                _ => json!({"status":"denied","output":message}),
            };
            Self::append_tool(&mut run, &result)?;
            app.store
                .save_run_observed(
                    &mut run,
                    "tool_result",
                    &json!({"run_id":id,"name":name,"result":result}),
                )
                .await?;
            return Ok(std::ops::ControlFlow::Continue(()));
        }
        // A call that does not match the tool's schema is returned to the agent with a
        // precise error; it never reaches the tool or asks the user to approve it.
        if effective.permission != access::Permission::Deny && run["pending"]["decision"] != "deny"
        {
            // Tools with a tool.json are checked from its cached definition; the few still
            // defined in code (the loaders) validate their own arguments.
            if let Some(definition) = webagent_abilities::tool_definitions::get(&name) {
                if let Err(message) = tools::arguments::check(
                    &name,
                    &definition.parameters,
                    &args,
                    definition.usage_hint.as_deref(),
                ) {
                    app.actions
                        .not_dispatched(app.store.records(), &run, &run["pending"])
                        .await?;
                    let result = json!({"status":"rejected","error_kind":"invalid_arguments","output":message});
                    Self::append_tool(&mut run, &result)?;
                    app.store
                        .save_run_observed(
                            &mut run,
                            "tool_result",
                            &json!({"run_id":id,"name":name,"result":result}),
                        )
                        .await?;
                    return Ok(std::ops::ControlFlow::Continue(()));
                }
            }
        }
        if effective.permission != access::Permission::Deny && H::messaging_owns(&name) {
            if let Err(error) = app
                .native
                .messaging_preflight(app.native_view(), &mut run, &name, &args)
                .await
            {
                let reason = error
                    .downcast_ref::<webagent_access::PublicRefusal>()
                    .map_or(
                        "Private messaging destination or authority unavailable",
                        |r| r.0,
                    );
                let result = json!({"status":"denied","output":reason});
                Self::append_tool(&mut run, &result)?;
                app.store
                    .save_run_observed(
                        &mut run,
                        "tool_result",
                        &json!({"run_id":id,"name":name,"result":result}),
                    )
                    .await?;
                return Ok(std::ops::ControlFlow::Continue(()));
            }
        }
        if effective.permission != access::Permission::Deny {
            match app
                .actions
                .preflight(app.store.records(), &mut run, &name, &args, &visibility)
                .await
            {
                Ok(Some(result)) => {
                    Self::append_tool(&mut run, &result)?;
                    app.store
                        .save_run_observed(
                            &mut run,
                            "tool_result",
                            &json!({"run_id":id,"name":name,"result":result}),
                        )
                        .await?;
                    return Ok(std::ops::ControlFlow::Continue(()));
                }
                Ok(None) => {}
                Err(_) => {
                    let result = json!({"status":"denied","output":"Invalid action metadata or unavailable durable action record; no action dispatched"});
                    Self::append_tool(&mut run, &result)?;
                    app.store
                        .save_run_observed(
                            &mut run,
                            "tool_result",
                            &json!({"run_id":id,"name":name,"result":result}),
                        )
                        .await?;
                    return Ok(std::ops::ControlFlow::Continue(()));
                }
            }
            let retry = A::retry(&name);
            let call_id = run["pending"]["call_id"].as_str().unwrap_or("").to_owned();
            // A background call this run waits on has ended: record its outcome first.
            if let Some(task_id) = tools::retry_guard::running_task(&run, &name, &args) {
                if let Some(task) = app.store.background_get(&task_id).await? {
                    if task["status"] != "running" {
                        let prior = task["call_id"].as_str().unwrap_or("").to_owned();
                        let outcome = (task["status"] == "completed").then_some(&task["result"]);
                        tools::retry_guard::record(&mut run, &name, &args, &prior, outcome, &retry);
                    }
                }
            }
            // A call detached from an earlier run is still running: hold its repeat here too.
            let same = tools::retry_guard::key(&name, &args);
            for task in app.store.background_scan(Some(sid)).await? {
                // A parked approval's own task is this very call, not a repeat of it.
                let own = task["run_id"] == run["id"] && task["call_id"] == call_id.as_str();
                if task["detached"] == true
                    && task["status"] == "running"
                    && !own
                    && tools::retry_guard::key(&name, &task["arguments"]) == same
                {
                    let prior = task["call_id"].as_str().unwrap_or("");
                    let id = task["id"].as_str().unwrap_or("");
                    tools::retry_guard::mark_running(&mut run, &name, &args, prior, id, &retry);
                }
            }
            match tools::retry_guard::admit(&mut run, &name, &args, &call_id, &retry, None) {
                tools::retry_guard::Admission::Run => {}
                tools::retry_guard::Admission::Approve => {
                    tools::retry_guard::require_approval(&mut run["pending"], &args, &retry);
                }
                tools::retry_guard::Admission::Held(result) => {
                    app.actions
                        .not_dispatched(app.store.records(), &run, &run["pending"])
                        .await?;
                    Self::append_tool(&mut run, &result)?;
                    app.store
                        .save_run_observed(
                            &mut run,
                            "tool_result",
                            &json!({"run_id":id,"name":name,"result":result}),
                        )
                        .await?;
                    return Ok(std::ops::ControlFlow::Continue(()));
                }
            }
            if run["pending"]["new_intent_confirmation"] == true {
                effective.permission = access::Permission::Ask;
            }
        }
        if effective.permission == access::Permission::Ask
            && run["pending"]["decision"] != "approve"
        {
            run["status"] = json!("waiting_approval");
            run["pending"]["state"] = json!("waiting");
            if run["pending"]["waiting_since"].is_null() {
                run["pending"]["waiting_since"] = json!(now());
            }
            let mut payload = run["pending"].clone();
            payload["run_id"] = json!(id);
            app.store
                .save_run_observed(&mut run, "approval_required", &payload)
                .await?;
            return Ok(std::ops::ControlFlow::Break(()));
        }
        if run["pending"]["digest"] != digest(&args) {
            return Err(anyhow!("Tool argument digest mismatch"));
        }
        run["pending"]["failure"] = Value::Null;
        run["pending"]["state"] = json!("executing");
        run["pending"]["human_approved"] = json!(run["pending"]["decision"] == "approve");
        run["pending"]["decision"] = json!("consumed");
        let current = app
            .chat
            .live_access(
                app.store.records(),
                run["requester_user_id"]
                    .as_str()
                    .unwrap_or(app.store.current_user_id()),
                run["session_id"].as_str().unwrap_or(""),
                &agent_id,
            )
            .await
            .ok();
        let admitted = current.as_ref().is_some_and(|current| current.admitted())
            && app.run_model_authorized(&run).await;
        let unchanged = current
            .as_ref()
            .is_some_and(|current| json!(current.revision) == run["pending"]["access_revision"]);
        if !admitted || !unchanged {
            run["pending"]["state"] = json!("requested");
            run["pending"]["decision"] = Value::Null;
            if admitted {
                // Only permissions changed: decide this call again under them, so a revoked
                // tool is refused like a disabled one and the run continues.
                app.save(&mut run).await?;
                return Ok(std::ops::ControlFlow::Continue(()));
            }
            run["status"] = json!("stopped");
            run["error"] =
                json!("Access changed before tool dispatch: this account or agent lost access");
            app.save(&mut run).await?;
            return Ok(std::ops::ControlFlow::Break(()));
        }
        app.save(&mut run).await?;
        let pending_id = run["pending"]["id"]
            .as_str()
            .expect("a pending call has an id")
            .to_owned();
        drop(_guard);
        // The loop's debug poll frame exceeds 1 MiB. Poll this already-authorized
        // call on its own ordinary executor stack; the one-child scope aborts on drop.
        let tool_started = std::time::Instant::now();
        let mut tool_task = tokio::task::JoinSet::new();
        // The call has its own token: once detached it outlives this run step.
        let call_cancel = app.stop.child_token();
        tool_task.spawn(app.clone().call(
            sid.clone(),
            id.clone(),
            generation.clone(),
            name.clone(),
            args.clone(),
            pending_id.clone(),
            run["pending"]["policy_version"].as_u64().unwrap_or(0),
            call_cancel.clone(),
        ));
        // Build calls keep their broker-job path.
        let detachable = !matches!(name.as_str(), "v3_build_work" | "build_work");
        let grace =
            Duration::from_millis(app.runtime.read().await.scheduler.message_detach_grace_ms);
        let mut message_since: Option<std::time::Instant> = None;
        let joined = loop {
            tokio::select! {
                joined = tool_task.join_next() => break Some(joined),
                _ = cancel.cancelled(), if !call_cancel.is_cancelled() => call_cancel.cancel(),
                _ = tokio::time::sleep(Duration::from_millis(250)), if detachable => {
                    if app.message_pending(&run).await.unwrap_or(false) {
                        let since = *message_since.get_or_insert_with(std::time::Instant::now);
                        // "Send now" skips the grace period.
                        if since.elapsed() >= grace
                            || app.message_forced(&run).await.unwrap_or(false)
                        {
                            break None;
                        }
                    } else {
                        message_since = None;
                    }
                }
            }
        };
        let joined = match joined {
            Some(joined) => joined,
            None => {
                // A user message is waiting: answer it now and let the call finish in the
                // background. Its result reaches the model at a later step.
                let _guard = app.handles.gate.lock().await;
                let current = app.run(&id).await?;
                if current["generation"] != *generation
                    || current["pending"]["id"] != pending_id
                    || current["pending"]["state"] != "executing"
                    || status(&current) != "running"
                {
                    call_cancel.cancel();
                    return Ok(std::ops::ControlFlow::Break(()));
                }
                match app
                    .background_detach_record(&current, &args, Value::Null)
                    .await?
                {
                    Some(task_id) => {
                        app.background_follow_detached(task_id.clone(), tool_task, call_cancel);
                        return Self::detached_result(app, current, &name, &args, id, &task_id)
                            .await;
                    }
                    // No task capacity: keep waiting for the call as before.
                    None => {
                        drop(_guard);
                        tool_task.join_next().await
                    }
                }
            }
        };
        let result = match joined {
            Some(Ok(result)) => result,
            Some(Err(error)) => Err(tools::failure::task_error(Some(&error), tool_started)),
            None => Err(tools::failure::task_error(None, tool_started)),
        };
        let _guard = app.handles.gate.lock().await;
        let mut run = app.run(&id).await?;
        if run["generation"] != *generation
            || run["pending"]["id"] != pending_id
            || run["pending"]["state"] != "executing"
        {
            return Ok(std::ops::ControlFlow::Break(()));
        }
        let result = result.and_then(|outcome| {
            tools::outcome::stage(&mut run, &outcome.metadata)?;
            Ok(outcome.content)
        });
        // Stop/pause can commit while this child is completing. Retain evidence
        // for this exact attempt before reconciliation; observation never resumes it.
        if let Err(error) = &result {
            tools::failure::record(&mut run, error, None);
        }
        if status(&run) != "running" {
            if !terminal(status(&run))
                && matches!(name.as_str(), "v3_build_work" | "build_work")
                && args["action"] == "finish"
                && run["pending"]["state"] == "executing"
                && result
                    .as_ref()
                    .map_or(true, |value| value["status"] == "success")
            {
                // A pause or revocation may race the broker acknowledgement.
                // Keep the job reference; a queued receipt is not the final tool result.
                run["pending"]["external_job"] = json!({"kind":"v3_build",
                        "task_id":args["task_id"],"expected_inputs_sha256":args["expected_inputs_sha256"],
                        "job_id":result.as_ref().ok().map(|value|value["output"]["job_id"].clone()),
                        "submitted_at":now()});
                run["uncertain"] = json!(false);
                let restricted = run["error"] == "Cancelled by user"
                    || run["error"]
                        .as_str()
                        .unwrap_or("")
                        .starts_with("Access changed")
                    || app.shutoff().await?;
                run["status"] = json!(if restricted {
                    "stopped"
                } else {
                    "waiting_external"
                });
                app.save(&mut run).await?;
                return Ok(std::ops::ControlFlow::Break(()));
            }
            if result.is_err() {
                // Keep control timestamps and all result/replay state unchanged,
                // including terminal cancellation. Storage permits only safe evidence.
                let public = app.store.records().present_run(&run);
                app.store
                    .save_run_observed(&mut run, "run_status", &public)
                    .await?;
                return Ok(std::ops::ControlFlow::Break(()));
            }
            // An already committed result can resolve uncertainty without restarting the run.
            if Self::reconcile_late_tool_result(&mut run, result)? {
                app.save(&mut run).await?;
            }
            return Ok(std::ops::ControlFlow::Break(()));
        }
        match result {
            Ok(value) => {
                if matches!(name.as_str(), "v3_build_work" | "build_work")
                    && value["status"] == "success"
                {
                    super::recovery::note_build_task(
                        app.store.records(),
                        &run,
                        &args["action"],
                        &value["output"],
                    )
                    .await;
                }
                if matches!(name.as_str(), "v3_build_work" | "build_work")
                    && value["status"] == "success"
                    && super::recovery::awaits_broker_job(&args["action"], &value["output"])
                {
                    // The broker owns the job across our own container replacement. It runs
                    // as a background task: the agent is free, and recovery follows the job
                    // and reports its end, or a slow or stalled stage, as a background notice.
                    let claims = value["output"]["status"] == "waiting_claims";
                    run["pending"]["external_job"] = json!({"kind":"v3_build",
                            "task_id":args["task_id"],"expected_inputs_sha256":args["expected_inputs_sha256"],
                            "job_id":value["output"]["job_id"],"submitted_at":now(),
                            "claims_for":if claims { value["output"]["broker_id"].clone() } else { Value::Null }});
                    super::recovery::note_build_job(app.store.records(), &run, &value["output"])
                        .await;
                    let external = run["pending"].clone();
                    if let Some(task_id) =
                        app.background_detach_record(&run, &args, external).await?
                    {
                        let call_id = run["pending"]["call_id"].as_str().unwrap_or("").to_owned();
                        tools::retry_guard::mark_running(
                            &mut run,
                            &name,
                            &args,
                            &call_id,
                            &task_id,
                            &A::retry(&name),
                        );
                        let job = value["output"]["job_id"].as_str().unwrap_or("unknown");
                        // call_id is the background call; broker_task_id and inputs_sha256 name the release.
                        let result = if claims {
                            json!({"status":"running","call_id":task_id,
                            "broker_id":value["output"]["broker_id"],"output":
                            "Your task is queued for its claims as a background call. You get a \
                             background notice when it holds them (then reread the files and apply \
                             your edits again), or if it closes or is still queued after 2 hours. \
                             End your turn now: do not retry or poll."})
                        } else {
                            json!({"status":"running","call_id":task_id,"job_id":job,
                            "broker_task_id":args["task_id"],"inputs_sha256":args["expected_inputs_sha256"],
                            "output":format!(
                            "Broker job {job} is running as background call {task_id}. You get a \
                             background notice when it ends, and if a stage runs slow or stalls. End \
                             your turn now: do not poll or wait. Use build_work status only if the \
                             user asks how it is going, and build_work cancel to stop it.")})
                        };
                        Self::append_tool(&mut run, &result)?;
                        app.store
                            .save_run_observed(
                                &mut run,
                                "tool_result",
                                &json!({"run_id":id,"name":name,"result":result}),
                            )
                            .await?;
                        return Ok(std::ops::ControlFlow::Continue(()));
                    }
                    // No task capacity: wait on the job as before.
                    run["status"] = json!("waiting_external");
                    run["error"] = Value::Null;
                    app.save(&mut run).await?;
                    return Ok(std::ops::ControlFlow::Break(()));
                }
                // An uncertain result reaches the agent like any other; the repeat guard
                // keeps an identical call from blindly running again in this run.
                let call_id = run["pending"]["call_id"].as_str().unwrap_or("").to_owned();
                tools::retry_guard::record(
                    &mut run,
                    &name,
                    &args,
                    &call_id,
                    Some(&value),
                    &A::retry(&name),
                );
                Self::append_tool(&mut run, &value)?;
                app.store
                    .save_run_observed(
                        &mut run,
                        "tool_result",
                        &json!({"run_id":id,"name":name,"result":value}),
                    )
                    .await?;
            }
            Err(_) => {
                if Self::preserve_restart_interruption(&app, &mut run).await? {
                    // Retain the pending call and its original call ID: startup retries a
                    // read and gives a cut-off write its outcome-unknown result.
                    return Ok(std::ops::ControlFlow::Break(()));
                }
                if matches!(name.as_str(), "v3_build_work" | "build_work")
                    && args["action"] == "finish"
                {
                    // The request may have reached the broker before the socket
                    // closed. Reconcile by task ID; never submit finish again.
                    run["pending"]["external_job"] = json!({"kind":"v3_build",
                            "task_id":args["task_id"],"expected_inputs_sha256":args["expected_inputs_sha256"],
                            "submitted_at":now()});
                    run["status"] = json!("waiting_external");
                    run["error"] = Value::Null;
                    app.save(&mut run).await?;
                    return Ok(std::ops::ControlFlow::Break(()));
                }
                // A definite refusal had no effect; the model receives an error and continues.
                if let Some(failure) =
                    tools::failure::refused_before_dispatch(&run["pending"]["failure"])
                {
                    let result = tools::failure::refused_result(&failure);
                    Self::append_tool(&mut run, &result)?;
                    app.store
                        .save_run_observed(
                            &mut run,
                            "tool_result",
                            &json!({"run_id":id,"name":name,"result":result}),
                        )
                        .await?;
                    return Ok(std::ops::ControlFlow::Continue(()));
                }
                // The model receives the failure and continues: a read returned nothing, and
                // a write's unknown outcome is recorded so an identical repeat is held.
                // Cancellation still settles through the interruption path.
                if tools::failure::interruption_reason(&run["pending"]["failure"]) != "cancelled" {
                    let retry = A::retry(&name);
                    let result = if A::is_read(&name) {
                        json!({"status":"error","error_kind":"read_failed",
                            "output":"The tool failed before returning a result; no data was returned. Check the arguments or try another tool."})
                    } else {
                        tools::retry_guard::outcome_unknown(&retry)
                    };
                    let call_id = run["pending"]["call_id"].as_str().unwrap_or("").to_owned();
                    tools::retry_guard::record(&mut run, &name, &args, &call_id, None, &retry);
                    Self::append_tool(&mut run, &result)?;
                    app.store
                        .save_run_observed(
                            &mut run,
                            "tool_result",
                            &json!({"run_id":id,"name":name,"result":result}),
                        )
                        .await?;
                    return Ok(std::ops::ControlFlow::Continue(()));
                }
                Self::commit_tool_transport_failure(&app, &mut run, &name).await?;
                return Ok(std::ops::ControlFlow::Break(()));
            }
        }
        Ok(std::ops::ControlFlow::Continue(()))
    }

    /// Advance one claimed run generation, rebuilding current context before model calls and retaining host authority over tool execution and publication.
    async fn execute_run_inner(
        app: Self,
        id: String,
        generation: String,
        cancel: CancellationToken,
    ) -> Result<()> {
        // Bind actor authority from the sealed run even when recovery/direct callers
        // supply a storage-owner App. Conversation ownership never supplies identity.
        let mut app = app;
        let admitted = app.run(&id).await?;
        let requester = admitted["requester_user_id"]
            .as_str()
            .unwrap_or(app.store.current_user_id())
            .to_owned();
        let sid = admitted["session_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Run session missing"))?;
        let access = app
            .chat
            .resolve(app.store.records(), sid, &requester)
            .await?;
        if !access.can_write() {
            anyhow::bail!("Observer cannot execute an agent run");
        }
        app.chat_actor = Some(requester);
        app.chat_access = Some(access);
        let mut context_accounting = context_accounting::ContextAccounting::default();
        loop {
            let _guard = app.handles.gate.lock().await;
            let mut run = app.run(&id).await?;
            if run["generation"] != generation || status(&run) != "running" {
                return Ok(());
            }
            if run["parked"] == true && run["pending"].is_null() && run["batch"].is_null() {
                return app.finish_parked(run).await;
            }
            // A summary call cut off by a restart or retry resumes as an ordinary model step.
            if run["phase"] == "compacting" {
                run["phase"] = json!("model");
            }
            if !app
                .chat
                .participation_allows_run(app.store.records(), &run)
                .await?
            {
                return Ok(());
            }
            match app.asset_catalog() {
                Ok(catalog) => app.abilities = Arc::new(catalog),
                Err(error) => {
                    Self::pause_context_preparation(
                        &app,
                        &mut run,
                        &cancel,
                        "ability_catalog",
                        &error,
                    )
                    .await?;
                    return Ok(());
                }
            }
            // A drain (SIGTERM clears `ready`) pauses between steps; startup resumes it silently.
            if cancel.is_cancelled() || !app.ready.load(std::sync::atomic::Ordering::Acquire) {
                Self::preserve_restart_interruption(&app, &mut run).await?;
                return Ok(());
            }
            // Apply a composer choice before validating the next dispatch. This never
            // cancels/replays the model response or tool effects from the previous call.
            // A compaction-only run takes waiting messages only once its summary is done.
            if run["phase"] == "model" && run["compaction_only"] != true {
                if app.apply_steering(&mut run).await? {
                    context_accounting.reset();
                }
                match app
                    .native
                    .chat_model_preference(app.native_view(), &mut run)
                    .await
                {
                    Ok(true) => {
                        context_accounting.reset();
                        app.save(&mut run).await?;
                    }
                    Ok(false) => {}
                    Err(ref error) => {
                        tracing::warn!(
                            run_id=%id,
                            stage=super::bounded_model_authorization_failure_stage(error),
                            "Composer model preference could not be applied"
                        );
                        run["status"] = json!("paused");
                        run["error"]=json!("Selected model unavailable; choose a permitted connected model and resume");
                        app.save(&mut run).await?;
                        return Ok(());
                    }
                }
            }
            let agent_id = run["agent_id"]
                .as_str()
                .unwrap_or("shared_default")
                .to_owned();
            let access = app
                .chat
                .live_access(
                    app.store.records(),
                    run["requester_user_id"]
                        .as_str()
                        .unwrap_or(app.store.current_user_id()),
                    run["session_id"].as_str().unwrap_or(""),
                    &agent_id,
                )
                .await;
            match &access {
                Err(error) => tracing::warn!(
                    run_id=%id,
                    stage=super::bounded_model_authorization_failure_stage(error),
                    "Live model access check failed"
                ),
                Ok(access) if !access.admitted() => tracing::warn!(
                    run_id=%id,
                    stage="access_denied",
                    "Live model access check denied"
                ),
                _ => {}
            }
            let access = access.ok().filter(|access| access.admitted());
            let authorized = match access {
                Some(_) => app.run_model_authorized(&run).await,
                None => false,
            };
            let (Some(access), true) = (access, authorized) else {
                run["status"] = json!("stopped");
                run["error"] = json!("Access changed; run stopped");
                app.save(&mut run).await?;
                return Ok(());
            };
            let mut app = app.clone();
            // Reload validated declarations and bodies each loop, including local linked builds.
            // A rejected source pauses accepted work; it cannot reuse an earlier catalog grant.
            app.abilities = match app.asset_catalog() {
                Ok(catalog) => Arc::new(catalog),
                Err(error) => {
                    Self::pause_context_preparation(
                        &app,
                        &mut run,
                        &cancel,
                        "ability_catalog",
                        &error,
                    )
                    .await?;
                    return Ok(());
                }
            };
            app.store = match app.store.for_agent(&agent_id).await {
                Ok(store) => store,
                Err(_) => {
                    run["status"] = json!("paused");
                    run["error"] = json!("Agent storage unavailable; review required");
                    app.save(&mut run).await?;
                    return Ok(());
                }
            };
            if app.shutoff().await? {
                run["status"] = json!("stopped");
                app.save(&mut run).await?;
                return Ok(());
            }
            let sid = run["session_id"]
                .as_str()
                .expect("a run has a session id")
                .to_owned();
            let workflow_policy = match app.current_workflow_policy().await {
                Ok(policy) => policy,
                Err(_) => {
                    run["status"] = json!("paused");
                    run["error"] = json!("Invalid workflow policy; administrator review required");
                    app.save(&mut run).await?;
                    return Ok(());
                }
            };
            if run["ability_catalog_version"] != app.abilities.version() {
                run["loaded_abilities"] = json!([]);
                run["ability_catalog_version"] = json!(app.abilities.version());
                app.save(&mut run).await?;
            }
            if run["phase"] == "model" {
                let optimization_before = digest(&json!([
                    run["optimization_checked"],
                    run["optimization_request"],
                    run["loaded_abilities"]
                ]));
                app.native
                    .optimization_prepare(app.native_view(), &sid, &mut run)
                    .await?;
                if optimization_before
                    != digest(&json!([
                        run["optimization_checked"],
                        run["optimization_request"],
                        run["loaded_abilities"]
                    ]))
                {
                    app.save(&mut run).await?;
                }
                if run.get("tool_context_state").is_none()
                    && run["requester_user_id"]
                        .as_str()
                        .unwrap_or(app.store.current_user_id())
                        != app.store.current_user_id()
                {
                    run["tool_context_state"] = json!({"clock":0,"entries":[]});
                }
                if run.get("tool_context_state").is_none() {
                    let session = app.store.get(&format!("session:{sid}")).await?;
                    run["tool_context_state"] = session
                        .as_ref()
                        .and_then(|s| s.get("tool_context_state"))
                        .cloned()
                        .unwrap_or_else(|| json!({"clock":0,"entries":[]}));
                }
                if app.background_collect(&mut run).await? {
                    context_accounting.reset();
                }
                // Older runs had no selection; bind once before any model dispatch.
                let selection = match run.get("provider_selection") {
                    Some(value) => provider::ProviderSelection::parse(value, X::default_model()),
                    None => app.selection().await,
                };
                let selection = match selection {
                    Ok(selection) => selection,
                    Err(_) => {
                        run["status"] = json!("paused");
                        run["error"] =
                            json!("Unsupported or invalid provider selection; review this run");
                        app.save(&mut run).await?;
                        return Ok(());
                    }
                };
                if run.get("provider_selection").is_none() {
                    run["provider_selection"] = selection.value();
                    app.save(&mut run).await?;
                }
                if run.get("instructions").is_none() {
                    run["instructions"] = json!(
                        app.assembled_agent_instructions(
                            app.store.records(),
                            run["agent_id"].as_str().unwrap_or("shared_default")
                        )
                        .await?
                    );
                    app.save(&mut run).await?;
                }
                if !crate::components::available(crate::ports::AgentMetadata::root(
                    app.store.records(),
                )) {
                    app.component_pause(&mut run).await?;
                    return Ok(());
                }
                if let Err(error) = app.hydrate_context(&mut run).await {
                    Self::pause_context_preparation(&app, &mut run, &cancel, "history", &error)
                        .await?;
                    return Ok(());
                }
                let prepared_source_fence = match app.source_fence() {
                    Ok(value) => value,
                    Err(_) => {
                        if Self::rebuild_context(&app, &mut run).await? {
                            continue;
                        }
                        return Ok(());
                    }
                };
                let prepared_asset_key = Self::dispatch_asset_key(
                    &app,
                    &sid,
                    &agent_id,
                    &run["loaded_abilities"],
                    &run["selected_tools"],
                    &run["loaded_skills"],
                    &run["optimization_request"],
                    run["requester_user_id"]
                        .as_str()
                        .unwrap_or(app.store.current_user_id()),
                )
                .await?;
                // max_turns 0 means no limit.
                let turn_limit = workflow_policy.max_turns;
                if (turn_limit != 0
                    && run["turns"].as_u64().expect("run counters are numbers") >= turn_limit)
                    || run["attempt"].as_u64().expect("run counters are numbers")
                        >= workflow_policy.max_attempts
                {
                    run["status"] = json!("paused");
                    run["error"] = json!("Run limit reached; explicit continuation required");
                    app.save(&mut run).await?;
                    return Ok(());
                }
                // Input hydrated before model steps had unique keys can hold results whose
                // calls were never saved; context projection rejects them.
                if let Some(input) = run["input"].as_array_mut() {
                    drop_orphan_outputs(input);
                }
                let (_, mut visibility, ability_state) = app
                    .ability_controls_for(
                        &sid,
                        run["requester_user_id"]
                            .as_str()
                            .unwrap_or(app.store.current_user_id()),
                    )
                    .await?;
                // Every enabled schema is sent, so an ability needing a model role this agent
                // lacks is switched off here.
                app.hide_without_model_role(&agent_id, &mut visibility, &ability_state)
                    .await?;
                let ability_state = app.abilities.selected_state(
                    &visibility,
                    &ability_state,
                    &run["loaded_abilities"],
                );
                let mode = app.session_mode(&sid).await?;
                // CONTEXT-ASSET-KEY: include full freshly resolved access (including
                // grants), live bundle validity, policy, visibility, loaded state and session mode.
                // Hash only in memory; never publish private grant data in reports.
                run["asset_revision"] = if app.abilities.published() {
                    app.abilities
                        .source_root()
                        .and_then(|p| p.parent())
                        .and_then(|p| p.file_name())
                        .and_then(|s| s.to_str())
                        .map(|s| json!(s))
                        .unwrap_or(Value::Null)
                } else {
                    Value::Null
                };
                run["plugin_bindings"] = json!(app
                    .abilities
                    .all_schemas()
                    .iter()
                    .filter_map(|s| s["name"].as_str().and_then(|name| app
                        .abilities
                        .plugin_binding(name)
                        .map(|binding| (name.to_owned(), binding))))
                    .collect::<std::collections::BTreeMap<_, _>>());
                let assembled_asset_key = prepared_asset_key;
                // Every enabled tool is decided below; gate them all in one pass.
                let gates =
                    app.abilities
                        .tool_gates(&visibility, &ability_state, &run["loaded_abilities"]);
                let mut schemas = app.abilities.schemas_gated(&gates);
                // Core tools are offered without loading their ability.
                for schema in app.abilities.all_schemas() {
                    let name = schema["name"].as_str().unwrap_or("");
                    if always_on(name) && !schemas.iter().any(|s| s["name"] == name) {
                        schemas.push(schema);
                    }
                }
                let schemas = schemas
                    .into_iter()
                    .map(|mut schema| {
                        // Without delegation, start_session only forks: the agent never
                        // sees an agent_id to pass. Messaging preflight still refuses it.
                        if schema["name"] == "start_session"
                            && !webagent_abilities::controls::delegation_allowed(
                                &visibility["settings"]["agent_management"],
                                run["agent_id"].as_str().unwrap_or("shared_default"),
                            )
                        {
                            if let Some(properties) =
                                schema["parameters"]["properties"].as_object_mut()
                            {
                                properties.remove("agent_id");
                            }
                        }
                        // Offer only the providers this agent switched on; a refused choice
                        // is not a choice.
                        if schema["name"] == "web_search" {
                            let providers = webagent_abilities::controls::web_search_providers(
                                &visibility["settings"]["web_search"],
                            );
                            if let Some(properties) =
                                schema["parameters"]["properties"].as_object_mut()
                            {
                                if providers.is_empty() {
                                    properties.remove("provider");
                                } else {
                                    properties.insert(
                                        "provider".into(),
                                        serde_json::json!({"type":"string","enum":providers}),
                                    );
                                }
                            }
                        }
                        schema
                    })
                    .filter(|schema| {
                        let name = schema["name"].as_str().unwrap_or("");
                        if always_on(name) {
                            access.tool_decision(name, true, true).permission
                                != access::Permission::Deny
                        } else {
                            let gate = gates.get(name).cloned().unwrap_or_default();
                            app.live_tool_decision_gated(&access, name, &visibility, &gate)
                                .offered()
                        }
                    })
                    .collect::<Vec<_>>();
                let mut entries = app.abilities.catalog_with(&visibility, &ability_state);
                entries.retain_mut(|entry| app.filter_ability_entry_gated(&access, entry, &gates));
                let mut guidance_blocks = app.abilities.capability_guidance(
                    &entries,
                    &schemas,
                    &visibility,
                    &ability_state,
                    &run["loaded_abilities"],
                    &run["loaded_skills"],
                );
                // A release elsewhere can change skills mid-read; retry before pausing the run.
                let mut guidance_read = Err(anyhow::anyhow!("Skill guidance unavailable"));
                for attempt in 0..3 {
                    guidance_read = app
                        .native
                        .own_skill_guidance(app.native_view(), &sid, &id)
                        .await;
                    if guidance_read.is_ok() {
                        break;
                    }
                    if attempt < 2 {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                }
                match guidance_read {
                    Ok(blocks) => guidance_blocks.extend(blocks),
                    Err(error) => {
                        Self::pause_context_preparation(
                            &app,
                            &mut run,
                            &cancel,
                            "skill_guidance",
                            &error,
                        )
                        .await?;
                        return Ok(());
                    }
                }
                // A loaded skill's text is its load_skill result; it joins the skills only
                // once compaction has taken that result out of the history.
                let in_history = skills_in_history(&run["input"]);
                guidance_blocks.retain(|block| block.natural || !in_history.contains(&block.id));
                let menu = X::optimization_guidance(&run).to_owned();
                let profile_request = webagent_provider_contracts::RequestV1::Profile {
                    version: 1,
                    provider: crate::provider::projection_id(selection.provider),
                    model: selection.model.clone(),
                };
                let profile = match crate::components::provider_project(
                    crate::ports::AgentMetadata::root(app.store.records()),
                    &profile_request,
                    &cancel,
                )
                .await
                {
                    Ok(webagent_provider_contracts::ResultV1::Profile { profile, .. }) => profile,
                    Ok(_) => {
                        Self::pause_context_preparation(
                            &app,
                            &mut run,
                            &cancel,
                            "provider_profile",
                            &anyhow::anyhow!("Unexpected profile result"),
                        )
                        .await?;
                        return Ok(());
                    }
                    Err(error) => {
                        Self::pause_context_preparation(
                            &app,
                            &mut run,
                            &cancel,
                            "provider_profile",
                            &error,
                        )
                        .await?;
                        return Ok(());
                    }
                };
                let mut inclusion = app.abilities.context_inclusion(
                    &entries,
                    &schemas,
                    &visibility,
                    &ability_state,
                    &run["loaded_abilities"],
                    &run["loaded_skills"],
                );
                let mut gate_schemas = schemas;
                for schema in &mut gate_schemas {
                    if let Some(object) = schema.as_object_mut() {
                        object.remove("authorization");
                    }
                    let name = schema["name"].as_str().unwrap_or("");
                    if tool_execution::background_capable(name)
                        && tool_execution::resolve::<A>(&visibility, name)?.mode == "model"
                    {
                        schema["parameters"]["properties"]["_execution_mode"] = json!({"type":"string","enum":["foreground","background"],"description":"Return a durable background call id immediately when background is selected"});
                    }
                }
                let call_number = run["attempt"].as_u64().unwrap_or(0) + 1;
                // The mode's full guidance is in the history (set_execution_mode results);
                // mode_guidance.json picks what this step's guidance message adds: nothing, a
                // one-line status, or the full guidance. set_execution_mode uses the revision
                // recorded here when the agent changes the mode.
                let mut contributions = Vec::new();
                run["mode_revision_shown"] = mode["revision"].clone();
                let placement = webagent_context::defaults::mode_guidance();
                let label = mode["definition"]["label"].as_str().unwrap_or("Unknown");
                // The per-step guidance message: the mode's per-turn label, then its per-turn
                // guidance ("per_step" adds the full mode guidance before it).
                let turn = mode["definition"]["turn_guidance"]
                    .as_str()
                    .unwrap_or("")
                    .trim();
                let body = match placement.as_str() {
                    "per_step" => format!(
                        "Execution mode: {label}\n{}\n\n{turn}",
                        mode["definition"]["prompt"].as_str().unwrap_or("").trim()
                    ),
                    "both" => turn.to_owned(),
                    _ => String::new(),
                };
                let turn_label = mode["definition"]["turn_label"]
                    .as_str()
                    .unwrap_or("")
                    .trim();
                let mode_text = (!body.trim().is_empty()).then(|| {
                    if turn_label.is_empty() {
                        body.trim().to_owned()
                    } else {
                        format!("{turn_label} {}", body.trim())
                    }
                });
                if let Some(content) = mode_text {
                    contributions.push(context_input::Contribution {
                        id: "session_mode".into(),
                        source: "native_session_mode".into(),
                        lifetime: context_input::Lifetime::Call,
                        content,
                        turn: run["turns"].as_u64().unwrap_or(0),
                        call: call_number,
                    });
                }
                // A newly seen issue group gets one call-scoped chance to be explained.
                let review_entry = run["run_health"]["review_pending"].clone();
                if review_entry.is_null() {
                    tracing::warn!(
                        calls = run["run_health"]["calls"].as_u64().unwrap_or(0),
                        failed = run["run_health"]["failed"].as_u64().unwrap_or(0),
                        "review marker absent at turn start"
                    );
                }
                if let Some(entry) = review_entry.as_object() {
                    // Diagnostics ships with the optional Administrator pack; without it
                    // there is no agent_review tool to ask for.
                    let installed = app
                        .abilities
                        .tool_abilities("agent_review", &Value::Null, &Value::Null, &Value::Null)
                        .iter()
                        .any(|id| id == "diagnostics");
                    let prompted = REVIEW_PROMPTS_ENABLED
                        && installed
                        && access.ability_permission("diagnostics") != access::Permission::Deny;
                    super::health::record_review_prompt(&mut run, prompted);
                    tracing::warn!(
                        prompted,
                        tool = entry["tool"].as_str().unwrap_or(""),
                        kind = entry["kind"].as_str().unwrap_or(""),
                        "review prompt for a newly seen issue group"
                    );
                    if prompted {
                        let field = |name: &str| entry[name].as_str().unwrap_or("").to_owned();
                        let request = format!(
                            "Before you continue: this is a new issue group (tool {}, class {}, kind {}, template \"{}\"). Call agent_review with those four values exactly and a note saying what you were trying to do, what you expected, and what would have prevented it. If you cannot tell why it happened, say so in the note. This is asked once.",
                            field("tool"), field("class"), field("kind"), field("template")
                        );
                        // The request rides on the failed tool's result, which this call sends
                        // for the first time; a call note is only the fallback.
                        if !attach_review_request(&mut run, &field("tool"), &request) {
                            contributions.push(context_input::Contribution {
                                id: "review_entry".into(),
                                source: "native_review_entry".into(),
                                lifetime: context_input::Lifetime::Call,
                                content: request,
                                turn: run["turns"].as_u64().unwrap_or(0),
                                call: call_number,
                            });
                        }
                    }
                    run["run_health"]["review_pending"] = Value::Null;
                }
                // GATE-INJECTION: Rust reads validated gate JSON and adds trusted
                // call-scoped guidance. The model sees private control schemas, but
                // their replies return to Rust for validation and durable storage.
                let before_model =
                    gate_registry::selected(&workflow_policy, workflow::Checkpoint::ModelDispatch)?;
                let agent_visibility =
                    gate_controls::agent_visibility(app.store.records(), &agent_id).await?;
                let promotion_mode = if before_model
                    .iter()
                    .any(|item| item.id == gate_registry::MESSAGE_VISIBILITY)
                {
                    agent_visibility.mode
                } else {
                    gate_controls::PromotionMode::None
                };
                // The publish tool is offered the same way whether or not every message is
                // promoted (no schema change when the toggle flips); its guidance is part of
                // the mode's per-turn guidance.
                if promotion_mode != gate_controls::PromotionMode::None {
                    let config = gate_registry::visibility_config()?;
                    gate_schemas.push(gate_controls::working_schema(&config));
                }
                let before_publication =
                    gate_registry::selected(&workflow_policy, workflow::Checkpoint::Publication)?;
                if before_publication
                    .iter()
                    .any(|item| item.id == gate_registry::CLOSING_STATUS)
                {
                    // Agent Identity selects whether the status runs and which parts it requires.
                    let closing =
                        gate_controls::agent_closing(app.store.records(), &agent_id).await?;
                    // A final reply reuses these settings to queue the closer.
                    run["closing_settings"] = serde_json::to_value(&closing)?;
                }
                // COMPACTION CONFIG: reread the single external JSON at each call boundary.
                // Agent Identity can override the two history thresholds. Hold the
                // resolved settings through planning and summary validation for this call.
                let defaults = match context_compaction::Config::read(&app.compaction_config_path) {
                    Ok(config) => config,
                    Err(error) => {
                        run["status"] = json!("paused");
                        run["error"] = json!(format!("Invalid context-compaction.json: {error}"));
                        app.save(&mut run).await?;
                        return Ok(());
                    }
                };
                let thresholds =
                    context_compaction::agent_thresholds(app.store.records(), &agent_id).await;
                // The agent's Identity setting wins over the model's provider setting.
                let cache_timeout_override = thresholds
                    .as_ref()
                    .ok()
                    .and_then(Option::as_ref)
                    .and_then(|settings| settings.cache_timeout_minutes);
                let keep_alive = thresholds
                    .as_ref()
                    .ok()
                    .and_then(Option::as_ref)
                    .and_then(|settings| settings.keep_alive)
                    .is_some_and(|settings| settings.enabled);
                // Fold by tasks (Identity): how many finished tasks stay verbatim.
                let fold_tasks = thresholds
                    .as_ref()
                    .ok()
                    .and_then(Option::as_ref)
                    .and_then(|settings| settings.fold_tasks_retained);
                let mut compaction_config = match thresholds.and_then(|settings| match settings {
                    Some(settings) => settings.apply(&defaults),
                    None => Ok(defaults),
                }) {
                    Ok(config) => config,
                    Err(_) => {
                        run["status"] = json!("paused");
                        run["error"] =
                            json!("Invalid agent compaction thresholds; review Identity settings");
                        app.save(&mut run).await?;
                        return Ok(());
                    }
                };
                // Thresholds count provider tokens: convert them with this model's measured ratio.
                compaction_config.bytes_per_token = bytes_per_token(&run, &selection);
                // A bridge that cannot fork its session cannot summarize itself, so its folds
                // are System folds and are planned as such (including the System budget).
                if compaction_config.summarizer == context_compaction::Summarizer::SameModel
                    && selection.provider.is_bridge()
                    && !selection.provider.bridge_forks()
                {
                    compaction_config.summarizer = context_compaction::Summarizer::System;
                }
                // SYSTEM-COMPACTION-BUDGET: cap automatic/manual folds by the
                // current pinned System candidate, including after backup selection.
                // Missing System still pauses only when compaction is needed.
                if let Ok(system_run) = system_dispatch_run(&run) {
                    let system_selection = provider::ProviderSelection::parse(
                        &system_run["provider_selection"],
                        X::default_model(),
                    )?;
                    compaction_config.system_input_budget = Some(
                        config::Config::<C>::read(&app.runtime_path)?
                            .context
                            .ceiling(&system_selection)?
                            .min(workflow_policy.max_context_bytes),
                    );
                }
                // Project replay only: retain the original journal/input and live policy authority.
                let retention_policy =
                    match crate::tool_output::effective(app.store.records(), &agent_id).await {
                        Ok(policy) => policy,
                        Err(_) => {
                            run["status"] = json!("paused");
                            run["error"] = json!("Invalid tool output policy; review Identity");
                            app.save(&mut run).await?;
                            return Ok(());
                        }
                    };
                let retention_prompts = match crate::tool_output::Prompts::read_at(
                    crate::ports::AgentMetadata::root(app.store.records()),
                    app.abilities.source_root(),
                ) {
                    Ok(prompts) => prompts,
                    Err(_) => {
                        run["status"] = json!("paused");
                        run["error"] =
                            json!("Invalid tool output prompts; review app prompts configuration");
                        app.save(&mut run).await?;
                        return Ok(());
                    }
                };
                run["tool_context_policy"] = serde_json::to_value(&retention_policy)?;
                if app
                    .restore_retained(&sid, &agent_id, &mut run, &retention_policy)
                    .await?
                {
                    context_accounting.reset();
                }
                crate::tool_output::synchronize(&mut run, &retention_policy);
                if retention_policy.enabled {
                    // A gate tool has no skill file: its guidance rides in its description,
                    // sent only while retention is on, instead of a note on every call.
                    let mut schema = crate::tool_output::schema();
                    schema["description"] = json!(format!(
                        "{} {}",
                        schema["description"].as_str().unwrap_or_default(),
                        retention_prompts.guidance.trim()
                    ));
                    gate_schemas.push(schema);
                }
                let retention_input = match app
                    .dedup_prepare_bounded(
                        &mut run,
                        compaction_config.tool_result_preview_bytes,
                        compaction_config.dedup_max_output_bytes,
                        &cancel,
                    )
                    .await
                {
                    Ok(input) => Some(input),
                    Err(error) => {
                        app.pause_dedup(&mut run, &error).await?;
                        return Ok(());
                    }
                };
                if retention_input.is_some() {
                    context_accounting.reset();
                }
                let capability = match app
                    .prepare_capability(
                        &run,
                        &gate_schemas,
                        guidance_blocks,
                        menu,
                        &assembled_asset_key,
                        &cancel,
                    )
                    .await
                {
                    Ok(value) => value,
                    Err(error) => {
                        Self::pause_context_preparation(
                            &app,
                            &mut run,
                            &cancel,
                            "capability",
                            &error,
                        )
                        .await?;
                        return Ok(());
                    }
                };
                if let Some(base) = inclusion.iter_mut().find(|entry| entry.id == "base") {
                    for asset in capability.inclusion.iter().filter(|asset| {
                        asset.kind == webagent_capability_contracts::GuidanceKind::Own
                    }) {
                        base.guidance_bytes += if asset.included {
                            asset.content_bytes
                        } else {
                            0
                        };
                        base.skills.push(abilities::SkillInclusion {
                            kind: "own".into(),
                            id: asset
                                .id
                                .strip_prefix("own:base:")
                                .unwrap_or(&asset.id)
                                .into(),
                            mode: "on_request".into(),
                            included: asset.included,
                            content_bytes: if asset.included {
                                asset.content_bytes
                            } else {
                                0
                            },
                            reason: if asset.included {
                                "included"
                            } else {
                                "on_request"
                            }
                            .into(),
                        });
                    }
                }
                run["ability_context"] = serde_json::to_value(abilities::AbilityContext {
                    turn: run["turns"].as_u64().unwrap_or(0),
                    call: call_number,
                    abilities: inclusion.clone(),
                })?;
                let placement_history = retention_input
                    .as_deref()
                    .unwrap_or(run["input"].as_array().expect("run input is an array"))
                    .to_vec();
                let placement_identity = context_input::CallIdentity {
                    turn: run["turns"].as_u64().unwrap_or(0),
                    call: call_number,
                };
                let projection = match app
                    .project_context(
                        &mut run,
                        &capability,
                        &profile,
                        &gate_schemas,
                        &placement_history,
                        &contributions,
                        placement_identity,
                        &cancel,
                    )
                    .await
                {
                    Ok(value) => value,
                    Err(error) => {
                        eprintln!(
                            "Projection history for run {}: {}",
                            run["id"].as_str().unwrap_or("unknown"),
                            history_shape(&placement_history)
                        );
                        Self::pause_context_preparation(
                            &app,
                            &mut run,
                            &cancel,
                            "projection",
                            &error,
                        )
                        .await?;
                        return Ok(());
                    }
                };
                let crate::context_assets::Working {
                    instructions,
                    guidance,
                    input: projected_input,
                    schemas: ordered_schemas,
                    extra_json_bytes,
                    contributions_projected,
                } = projection;
                gate_schemas = ordered_schemas;
                context_accounting.projection_extra_bytes = extra_json_bytes;
                context_accounting.contributions_projected = contributions_projected;
                // MODEL-CONTEXT-CEILING: runtime profiles bound the entire envelope;
                // compaction JSON still owns folding policy and summary reserves.
                let model_ceiling = config::Config::<C>::read(&app.runtime_path)?
                    .context
                    .ceiling(&selection)?
                    .min(workflow_policy.max_context_bytes);
                // BACKGROUND COMPACTION: between the trigger and the ceiling a summary runs
                // beside the main call, so the main call assembles with the ceiling as its
                // fold limit. The model's hard limit (`model_ceiling`) still bounds both.
                let background = compaction_config.ceiling() > compaction_config.trigger_tokens;
                // HARD-LIMIT FORK: with the same-model setting, a history past the configured
                // ceiling is summarized from a fork too. This pass assembles against the
                // model's own limit, starts that summary and waits for it.
                let hard_limit_fork = run["hard_limit_fork"] == "trying";
                let assembly_compaction = if hard_limit_fork {
                    let mut at_model = compaction_config.at_ceiling();
                    let tokens = model_ceiling / compaction_config.token_bytes(1).max(1);
                    at_model.trigger_tokens = tokens;
                    at_model.ceiling_tokens = Some(tokens);
                    context_compaction::Config(at_model)
                } else if background {
                    context_compaction::Config(compaction_config.at_ceiling())
                } else {
                    compaction_config.clone()
                };
                let job_key = background_compaction::key(&sid);
                let mut job = app.store.get(&job_key).await?.unwrap_or(Value::Null);
                // A job this process does not run was cut off by a restart; record it and let
                // the trigger plan a fresh one. A failed job never stops the run.
                if job["state"] == "running" && background_compaction::running(&sid).is_none() {
                    job["state"] = json!("failed");
                    job["error"] = json!("Background summary interrupted by a restart");
                }
                if job["state"] == "failed" {
                    let step = job["step"].as_str().unwrap_or("compaction:0").to_owned();
                    compaction_result(
                        &mut run,
                        &step,
                        json!({"error":job["error"],"summarizer":job["summarizer"],
                            "mode":"background"}),
                    );
                    // After a same-model failure the next summary uses System. A requested
                    // fold (the agent's task fold, `/compact`) is queued again so System retries it
                    // now; the user sees a failure only when System fails too.
                    let same_model = job["summarizer"] == "self";
                    let request_key = background_compaction::request_key(&sid);
                    let retried = same_model
                        && job["reason"] == "requested"
                        && match app.store.get(&request_key).await? {
                            Some(mut request) if request["state"] == "started" => {
                                request["state"] = json!("pending");
                                app.store.put(&request_key, &request).await?;
                                true
                            }
                            _ => false,
                        };
                    if !retried {
                        run["compaction_only_note"] = json!(format!(
                            "Compaction failed ({}); the conversation is unchanged.",
                            job["error"].as_str().unwrap_or("summary unavailable")
                        ));
                    }
                    // A failed bridge fork counts toward `fork_attempts`; any other outcome
                    // keeps the count as it was.
                    let failures =
                        job["fork_failures"].as_u64().unwrap_or(0) + u64::from(job["fork"] == true);
                    job = json!({"state":"idle","system_next":same_model,"fork_failures":failures});
                    app.store.put(&job_key, &job).await?;
                }
                // BACKGROUND COMPACTION RESULT: history is append-only, so the plan made at
                // the trigger still applies here; its digest rejects a changed prefix.
                if job["state"] == "done" {
                    let step = job["step"].as_str().unwrap_or("compaction:0").to_owned();
                    let summary = job["summary"].as_str().unwrap_or_default().to_owned();
                    let applied =
                        serde_json::from_value::<context_compaction::Plan>(job["plan"].clone())
                            .map_err(anyhow::Error::from)
                            .and_then(|plan| {
                                let history = retention_input.as_deref().unwrap_or(
                                    run["input"].as_array().expect("run input is an array"),
                                );
                                context_compaction::apply_summary(history, &plan, &summary)
                                    .map(|compacted| (plan, compacted))
                            });
                    let outcome = json!({"summarizer":job["summarizer"],"mode":"background",
                        "cached_input_tokens":job["cached_input_tokens"]});
                    app.store.put(&job_key, &json!({"state":"idle"})).await?;
                    match applied {
                        Ok((plan, compacted)) => {
                            let mut outcome = outcome;
                            outcome["summary"] = json!(summary);
                            compaction_result(&mut run, &step, outcome);
                            X::record_usage(&mut run, &json!({"usage": job["usage"]}));
                            run["input"] = json!(compacted);
                            run["tool_result_projection"]["rebase"] = json!(true);
                            context_accounting.reset();
                            run["compactions"] =
                                json!(run["compactions"].as_u64().unwrap_or(0) + 1);
                            run["updated_at"] = json!(now());
                            let segment_id = format!(
                                "{}-{}",
                                id,
                                run["compactions"]
                                    .as_u64()
                                    .expect("run counters are numbers")
                            );
                            app.store
                                .save_run_observed(
                                    &mut run,
                                    "context_compacted",
                                    &json!({"run_id":id,"source_digest":plan.source_digest,
                                        "segment_id":segment_id,"source":plan.source,
                                        "dropped_items":plan.drop_until,
                                        "merge":plan.kind == context_compaction::PlanKind::Merge,
                                        "summary_bytes":summary.len()}),
                                )
                                .await?;
                            continue;
                        }
                        Err(error) => {
                            let mut outcome = outcome;
                            outcome["error"] = json!(format!("Summary no longer applies: {error}"));
                            compaction_result(&mut run, &step, outcome);
                        }
                    }
                }
                let assembly_request = context_input::Request {
                    history: retention_input
                        .as_deref()
                        .unwrap_or(run["input"].as_array().expect("run input is an array")),
                    base: &instructions,
                    guidance: &guidance,
                    schemas: &gate_schemas,
                    contributions: &contributions,
                    identity: context_input::CallIdentity {
                        turn: run["turns"].as_u64().unwrap_or(0),
                        call: run["attempt"].as_u64().unwrap_or(0) + 1,
                    },
                    selection: &selection,
                    max_bytes: model_ceiling,
                    compaction: &assembly_compaction,
                };
                if !crate::components::available(crate::ports::AgentMetadata::root(
                    app.store.records(),
                )) {
                    app.component_pause(&mut run).await?;
                    return Ok(());
                }
                let prepared = crate::components::prepare(
                    crate::ports::AgentMetadata::root(app.store.records()),
                    &run,
                    assembly_request,
                    &mut context_accounting,
                    &cancel,
                )
                .await;
                let assembly_diagnostic =
                    app.native
                        .assembly(app.store.records(), &run, &assembly_request, &prepared);
                let assembled = match prepared {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        if let Some(report) = assembly_diagnostic {
                            run["latest_context_assembly"] = report;
                        }
                        if error.downcast_ref::<context_assembly::Failure>().is_none() {
                            app.component_pause(&mut run).await?;
                            return Ok(());
                        }
                        let (code, stage) = context_assembly::failure_code(&error);
                        // A budget or configuration fault, not a pause: the run ends
                        // failed and a new message (or resume) starts over.
                        run["status"] = json!("failed");
                        run["error"] = json!(assembly_failure_message(
                            code,
                            stage,
                            &run["latest_context_assembly"]["report"],
                            &assembly_compaction,
                        ));
                        let proposed = (code == "trigger_overhead_exceeds_budget")
                            .then(|| {
                                trigger_remedy(
                                    &run["latest_context_assembly"]["report"],
                                    &assembly_compaction,
                                )
                            })
                            .flatten();
                        if let Some(to) = proposed {
                            // Only an agent operator may approve; anyone else's chat says
                            // an operator was notified through the staff message store.
                            let operator = agent_operator(&access);
                            run["context_remedy"] = json!({"kind":"raise_compaction_trigger",
                                "agent_id":agent_id,"from_tokens":assembly_compaction.trigger_tokens,
                                "to_tokens":to,"state":"proposed","operator_notified":!operator});
                            if !operator {
                                if let Err(error) = notify_trigger_operators(
                                    app.store.records(),
                                    &run,
                                    &agent_id,
                                    assembly_compaction.trigger_tokens,
                                    to,
                                )
                                .await
                                {
                                    tracing::warn!(run_id=%id, "Operator notice not saved: {error}");
                                    run["context_remedy"]["operator_notified"] = json!(false);
                                }
                            }
                        }
                        app.save(&mut run).await?;
                        return Ok(());
                    }
                };
                // At the ceiling a running summary is awaited
                // instead of starting a second one; its result applies on the next pass.
                if matches!(assembled, context_assembly::Prepared::Compact(_)) {
                    if let Some(mut finished) = background_compaction::running(&sid) {
                        run["phase"] = json!("compacting");
                        app.save(&mut run).await?;
                        drop(_guard);
                        tokio::select! {
                            _ = cancel.cancelled() => return Ok(()),
                            _ = finished.wait_for(|done| *done) => {}
                        }
                        continue;
                    }
                }
                // With the same-model setting, the hard limit first tries a fork (next pass);
                // past even the model's own limit, or when the fork could not start, the System
                // fold below is the only way on.
                if matches!(assembled, context_assembly::Prepared::Compact(_))
                    && compaction_config.summarizer == context_compaction::Summarizer::SameModel
                    && (!selection.provider.is_bridge() || selection.provider.bridge_forks())
                    && run["hard_limit_fork"].is_null()
                {
                    drop(assembled);
                    run["hard_limit_fork"] = json!("trying");
                    app.save(&mut run).await?;
                    continue;
                }
                // The assembler only plans; the loop owns summarization and persistence.
                if let context_assembly::Prepared::Compact(plan) = assembled {
                    run["hard_limit_fork"] = Value::Null;
                    if let Some(report) = assembly_diagnostic {
                        run["latest_context_assembly"] = report;
                    }
                    // Compaction dispatches only its admission-pinned System role and backup cursor.
                    let system_run = match system_dispatch_run(&run) {
                        Ok(value) => value,
                        Err(error) => {
                            run["status"] = json!("paused");
                            run["error"] = json!(error.to_string());
                            if let Some(report) = app.native.validation(
                                app.store.records(),
                                &run,
                                "system_summary_input_unavailable",
                                "summary_input",
                            ) {
                                run["latest_context_assembly"] = report;
                            }
                            app.save(&mut run).await?;
                            return Ok(());
                        }
                    };
                    let selection = provider::ProviderSelection::parse(
                        &system_run["provider_selection"],
                        X::default_model(),
                    )?;
                    let summary_source = app.dedup_summary_source(&run, &plan.source).await?;
                    // Restored full tool outputs can outgrow the source the planner sized;
                    // summarize those shortened previews instead of stopping the run.
                    let prompt = match compaction_config
                        .prompt_for(&plan, &summary_source)
                        .or_else(|_| compaction_config.prompt_for(&plan, &plan.source))
                    {
                        Ok(prompt) => prompt,
                        Err(_) => {
                            run["status"] = json!("paused");
                            run["error"] =
                                json!("Context summary source is too large; review this run");
                            if let Some(report) = app.native.validation(
                                app.store.records(),
                                &run,
                                "summary_source_limit",
                                "summary_input",
                            ) {
                                run["latest_context_assembly"] = report;
                            }
                            app.save(&mut run).await?;
                            return Ok(());
                        }
                    };
                    // Validate the exact summary envelope against System, including on a backup retry.
                    let summary_context = config::Config::<C>::read(&app.runtime_path)?.context;
                    let summary_input = vec![json!({"role":"user","content":prompt})];
                    let system_instructions = provider::Provider::instructions_for_snapshot(
                        provider::DEFAULT_INSTRUCTIONS,
                        crate::ports::AgentMetadata::root(app.store.records()),
                        app.abilities.source_root(),
                    )?;
                    let validation = validate_system_summary_input(
                        &summary_context,
                        &selection,
                        &summary_input,
                        &system_instructions,
                        workflow_policy.max_context_bytes,
                    );
                    let summary_request = context_input::Request {
                        history: &summary_input,
                        base: &system_instructions,
                        guidance: "",
                        schemas: &[],
                        contributions: &[],
                        identity: context_input::CallIdentity {
                            turn: run["turns"].as_u64().unwrap_or(0),
                            call: run["attempt"].as_u64().unwrap_or(0) + 1,
                        },
                        selection: &selection,
                        max_bytes: summary_context
                            .ceiling(&selection)?
                            .min(workflow_policy.max_context_bytes),
                        compaction: &compaction_config,
                    };
                    if let Some(report) =
                        app.native
                            .summary(app.store.records(), &run, &summary_request, &validation)
                    {
                        run["latest_context_assembly"] = report;
                        app.save(&mut run).await?;
                    }
                    if validation.is_err() {
                        run["status"] = json!("paused");
                        run["error"] = json!(
                            "System summary input exceeds its context budget; review this run"
                        );
                        app.save(&mut run).await?;
                        return Ok(());
                    }
                    run["engine_component_prepared"] = json!({"version":1,"component":crate::components::CONTEXT,"kind":"compaction","call":call_number});
                    // The progress note reads the phase; the call row stays until its result row.
                    let compaction_step = format!(
                        "compaction:{}:{}",
                        run["compactions"].as_u64().unwrap_or(0) + 1,
                        uuid::Uuid::new_v4()
                    );
                    let input_bytes: usize = summary_input
                        .iter()
                        .map(|item| item["content"].as_str().map_or(0, str::len))
                        .sum();
                    run["phase"] = json!("compacting");
                    compaction_activity(
                        &mut run,
                        format!("{compaction_step}:call"),
                        json!({"type":"compaction_call","model":selection.model,
                            "input_bytes":input_bytes,"reason":"automatic","mode":"hard_limit",
                            "summarizer":"system","fork":false}),
                    );
                    app.save(&mut run).await?;
                    let prepared_system = match crate::provider::CallDraft::new(
                        &selection,
                        summary_input,
                        Vec::new(),
                        &sid,
                        &system_instructions,
                    )
                    .prepare(
                        crate::ports::AgentMetadata::root(app.store.records()),
                        &cancel,
                    )
                    .await
                    {
                        Ok(value) => value,
                        Err(error) => {
                            compaction_result(
                                &mut run,
                                &compaction_step,
                                json!({"error": error.to_string()}),
                            );
                            Self::pause_context_preparation(
                                &app,
                                &mut run,
                                &cancel,
                                "summary_request",
                                &error,
                            )
                            .await?;
                            return Ok(());
                        }
                    };
                    drop(_guard);
                    let tokens = match app.tokens_for(&selection, &system_run).await {
                        Ok(value) => value,
                        Err(error) => {
                            let _guard = app.handles.gate.lock().await;
                            let mut run = app.run(&id).await?;
                            if run["generation"] == generation && status(&run) == "running" {
                                compaction_result(
                                    &mut run,
                                    &compaction_step,
                                    json!({"error": error.to_string()}),
                                );
                                app.trusted_model_failure(&mut run, &error, true)?;
                                app.save(&mut run).await?;
                            }
                            return Ok(());
                        }
                    };
                    let (sender, mut receiver) = tokio::sync::mpsc::channel::<String>(32);
                    let drain =
                        tokio::spawn(async move { while receiver.recv().await.is_some() {} });
                    // Preserve the same shutoff/dispatch ordering as normal model calls.
                    let dispatch_guard = app.handles.gate.lock().await;
                    let mut current = app.run(&id).await?;
                    if current["generation"] != generation
                        || status(&current) != "running"
                        || app.shutoff().await?
                        || cancel.is_cancelled()
                    {
                        drop(sender);
                        drop(dispatch_guard);
                        let _ = drain.await;
                        return Ok(());
                    }
                    if !crate::components::available(crate::ports::AgentMetadata::root(
                        app.store.records(),
                    )) {
                        app.component_pause(&mut current).await?;
                        drop(sender);
                        drop(dispatch_guard);
                        let _ = drain.await;
                        return Ok(());
                    }
                    if !app
                        .run_model_authorized(&system_dispatch_run(&current)?)
                        .await
                    {
                        compaction_result(
                            &mut current,
                            &compaction_step,
                            json!({"error": "Access changed before model dispatch"}),
                        );
                        current["status"] = json!("stopped");
                        current["error"] = json!("Access changed before model dispatch");
                        app.save(&mut current).await?;
                        drop(sender);
                        drop(dispatch_guard);
                        let _ = drain.await;
                        return Ok(());
                    }
                    // A user choice made during credential retrieval is a new envelope,
                    // not an access failure. Reassemble before sending anything upstream.
                    let live_mode = app.session_mode(&sid).await?;
                    let live_choice = app
                        .store
                        .get(&format!("session_chat_controls:{sid}"))
                        .await?;
                    if live_mode["revision"] != mode["revision"]
                        || live_choice
                            .as_ref()
                            .and_then(|v| v["revision"].as_u64())
                            .unwrap_or(0)
                            != current["chat_model_revision"].as_u64().unwrap_or(0)
                    {
                        drop(sender);
                        drop(dispatch_guard);
                        let _ = drain.await;
                        continue;
                    }
                    if !Self::dispatch_asset_key(
                        &app,
                        &sid,
                        &agent_id,
                        &current["loaded_abilities"],
                        &current["selected_tools"],
                        &current["loaded_skills"],
                        &current["optimization_request"],
                        current["requester_user_id"]
                            .as_str()
                            .unwrap_or(app.store.current_user_id()),
                    )
                    .await
                    .is_ok_and(|key| key == assembled_asset_key)
                    {
                        let retry = Self::rebuild_context(&app, &mut current).await?;
                        drop(sender);
                        drop(dispatch_guard);
                        let _ = drain.await;
                        if retry {
                            continue;
                        }
                        return Ok(());
                    }
                    app.run_candidate(&system_dispatch_run(&current)?).await?;
                    if !app
                        .source_fence()
                        .is_ok_and(|key| key == prepared_source_fence)
                    {
                        let retry = Self::rebuild_context(&app, &mut current).await?;
                        drop(sender);
                        drop(dispatch_guard);
                        let _ = drain.await;
                        if retry {
                            continue;
                        }
                        return Ok(());
                    }
                    let mut completion = Box::pin(async {
                        let tokens = &tokens;
                        let cancel = cancel.clone();
                        let ready = prepared_system.bind_credentials(tokens, &cancel)?;
                        app.provider.dispatch(ready, sender, cancel).await
                    });
                    let mut immediate = None;
                    futures_util::future::poll_fn(|cx| {
                        use std::future::Future;
                        if let std::task::Poll::Ready(value) = completion.as_mut().poll(cx) {
                            immediate = Some(value);
                        }
                        std::task::Poll::Ready(())
                    })
                    .await;
                    drop(dispatch_guard);
                    let response = if let Some(value) = immediate {
                        value
                    } else {
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => Err(anyhow!("Context summary cancelled")),
                            result = &mut completion => result
                        }
                    };
                    drop(completion);
                    let _ = drain.await;
                    let _guard = app.handles.gate.lock().await;
                    let mut run = app.run(&id).await?;
                    if run["generation"] != generation || status(&run) != "running" {
                        return Ok(());
                    }
                    if !app.run_model_authorized(&system_dispatch_run(&run)?).await {
                        compaction_result(
                            &mut run,
                            &compaction_step,
                            json!({"error": "Access changed during context summary"}),
                        );
                        run["status"] = json!("stopped");
                        run["error"] = json!("Access changed during context summary");
                        app.save(&mut run).await?;
                        return Ok(());
                    }
                    if app.shutoff().await? || cancel.is_cancelled() {
                        if Self::preserve_restart_interruption(&app, &mut run).await? {
                            return Ok(());
                        }
                        compaction_result(
                            &mut run,
                            &compaction_step,
                            json!({"error": "Context summary interrupted"}),
                        );
                        run["status"] = json!("stopped");
                        run["error"] = json!("Context summary interrupted");
                        app.save(&mut run).await?;
                        return Ok(());
                    }
                    if let Err(error) = &response {
                        compaction_result(
                            &mut run,
                            &compaction_step,
                            json!({"error": error.to_string()}),
                        );
                        app.trusted_model_failure(&mut run, error, true)?;
                        app.save(&mut run).await?;
                        return Ok(());
                    }
                    if let Ok(value) = &response {
                        X::record_usage(&mut run, value);
                    }
                    let summary = response.ok().and_then(|value| {
                        value["output"].as_array().map(|output| {
                            output
                                .iter()
                                .filter(|item| item["type"] == "message")
                                .flat_map(|item| item["content"].as_array().into_iter().flatten())
                                .filter_map(|item| item["text"].as_str())
                                .collect::<Vec<_>>()
                                .join("")
                        })
                    });
                    let compacted = summary
                        .as_deref()
                        .ok_or_else(|| {
                            context_assembly::Failure::new("summary_missing", "summary_validation")
                                .into()
                        })
                        .and_then(|summary| {
                            context_compaction::apply_summary(
                                retention_input.as_deref().unwrap_or(
                                    run["input"].as_array().expect("run input is an array"),
                                ),
                                &plan,
                                summary,
                            )
                            .map_err(context_assembly::compaction_error)
                        });
                    let compacted = match compacted {
                        Ok(compacted) => compacted,
                        Err(error) => {
                            run["status"] = json!("paused");
                            let (code, stage) = context_assembly::failure_code(&error);
                            run["error"] = json!(format!(
                                "Context summary paused: {stage}/{code}; review this run"
                            ));
                            let error = run["error"].as_str().unwrap_or_default().to_owned();
                            compaction_result(&mut run, &compaction_step, json!({"error": error}));
                            if let Some(report) =
                                app.native
                                    .validation(app.store.records(), &run, code, stage)
                            {
                                run["latest_context_assembly"] = report;
                            }
                            app.save(&mut run).await?;
                            return Ok(());
                        }
                    };
                    // COMPACTION PERSISTENCE: Store::save_run below atomically saves run,
                    // event and session checkpoint under its existing ownership fences.
                    // Original interactions stay intact; continue reassembles before dispatch.
                    compaction_result(&mut run, &compaction_step, json!({"summary": summary}));
                    run["input"] = json!(compacted);
                    run["tool_result_projection"]["rebase"] = json!(true);
                    context_accounting.reset();
                    run["compactions"] = json!(run["compactions"].as_u64().unwrap_or(0) + 1);
                    run["updated_at"] = json!(now());
                    run["error"] = Value::Null;
                    let segment_id = format!(
                        "{}-{}",
                        id,
                        run["compactions"]
                            .as_u64()
                            .expect("run counters are numbers")
                    );
                    app.store
                    .save_run_observed(
                        &mut run,
                        "context_compacted",
                        &json!({"run_id":id,"source_digest":plan.source_digest,
                            "segment_id":segment_id,"source":plan.source,
                            "dropped_items":plan.drop_until,"merge":plan.kind == context_compaction::PlanKind::Merge,
                            "summary_bytes":summary.as_deref().map_or(0,str::len)}),
                    )
                    .await?;
                    continue;
                }
                let context_assembly::Prepared::Ready(envelope) = assembled else {
                    unreachable!("compaction path continues before model dispatch")
                };
                let report = serde_json::to_value(&envelope.report)?;
                let envelope_bytes = envelope.report.envelope_bytes;
                let job_free = job["state"] != "running"
                    && background_compaction::running(&sid).is_none()
                    && run["closing_turn"].is_null();
                // REQUESTED COMPACTION: the agent's task fold or `/compact`
                // folds in the background like the trigger's summary. A request is used once;
                // one that finds nothing to fold leaves a notice row.
                let request_key = background_compaction::request_key(&sid);
                let request = if job_free {
                    app.store
                        .get(&request_key)
                        .await?
                        .filter(|request| request["state"] == "pending")
                } else {
                    None
                };
                let mut request_notice = None;
                let requested = match request {
                    Some(mut request) => {
                        request["state"] = json!("started");
                        app.store.put(&request_key, &request).await?;
                        match background_compaction::requested_plan(
                            &request,
                            envelope.input,
                            &compaction_config,
                        ) {
                            Ok(plan) => Some((plan, request)),
                            Err(error) => {
                                request_notice = Some(format!("Nothing was compacted: {error}"));
                                None
                            }
                        }
                    }
                    None => None,
                };
                // Past the trigger with no summary pending: plan one to run beside this call.
                let mut trigger_notice = None;
                let background_plan = if let Some((plan, request)) = requested {
                    let focus = request["focus"]
                        .as_str()
                        .filter(|focus| !focus.trim().is_empty())
                        .map(str::to_owned);
                    let message = background_compaction::with_focus(
                        background_compaction::fork_message(
                            &plan,
                            envelope.input,
                            &compaction_config,
                        ),
                        focus.as_deref(),
                    );
                    Some((plan, message, "requested", focus))
                } else if let Some(plan) = fold_tasks.filter(|_| job_free).and_then(|retained| {
                    // A task fold is a nicety: a planning error leaves the size rule below.
                    context_compaction::plan_tasks(envelope.input, retained, &compaction_config)
                        .ok()
                        .flatten()
                }) {
                    // TASK FOLD: a newly started task folds the finished tasks beyond those kept.
                    let message = background_compaction::fork_message(
                        &plan,
                        envelope.input,
                        &compaction_config,
                    );
                    Some((plan, message, "task", None))
                } else if (background || hard_limit_fork) && job_free {
                    let planned = context_compaction::plan_trigger(
                        envelope.input,
                        envelope.report.history_budget_bytes,
                        &compaction_config,
                        envelope.report.max_bytes - envelope.report.history_budget_bytes,
                    );
                    // A trigger below the fixed instructions and tools can never start a
                    // background summary; say so once per run (the ceiling still folds).
                    if let Err(error) = &planned {
                        if error.to_string()
                            == "Compaction trigger cannot fit fixed request overhead"
                            && run["compaction_trigger_notice"] != true
                        {
                            let report = assembly_diagnostic
                                .as_ref()
                                .map_or(&Value::Null, |report| &report["report"]);
                            trigger_notice = Some(assembly_failure_message(
                                "trigger_overhead_exceeds_budget",
                                "compaction_plan",
                                report,
                                &compaction_config,
                            ));
                        }
                    }
                    planned.ok().flatten().map(|plan| {
                        let message = background_compaction::fork_message(
                            &plan,
                            envelope.input,
                            &compaction_config,
                        );
                        (plan, message, "automatic", None)
                    })
                } else {
                    None
                };
                let instructions = envelope.instructions;
                // DISPATCH-HISTORY-TRANSFER: fingerprint the borrowed history before
                // metadata mutation, then transfer its allocation only after saves.
                // Durable input remains intact; this local run is reloaded after dispatch.
                let input_digest = digest(&projected_input);
                let schemas = envelope.schemas.to_vec();
                if let Some(error) = trigger_notice {
                    run["compaction_trigger_notice"] = json!(true);
                    compaction_result(
                        &mut run,
                        &format!("compaction:trigger:{}", uuid::Uuid::new_v4()),
                        json!({"mode":"background","error":error}),
                    );
                }
                if let Some(report) = assembly_diagnostic {
                    run["latest_context_assembly"] = report;
                }
                if let Some(notice) = request_notice {
                    compaction_result(
                        &mut run,
                        &format!("compaction:request:{}", uuid::Uuid::new_v4()),
                        json!({"error":notice,"reason":"requested","mode":"background"}),
                    );
                    run["compaction_only_note"] = json!(notice);
                }
                run["attempt"] =
                    json!(run["attempt"].as_u64().expect("run counters are numbers") + 1);
                run["context_assembly_report"] = report;
                run["dispatch_mode"] =
                    json!({"execution_mode":mode["execution_mode"],"revision":mode["revision"]});
                run["context_controls"] = json!({"trigger_tokens":compaction_config.trigger_tokens,"tail_tokens":compaction_config.tail_tokens,"characters_per_token":compaction_config.characters_per_token,"bytes_per_token":compaction_config.bytes_per_token});
                // Pair this request's size with the usage its reply reports (see bytes_per_token).
                run["token_calibration"] = json!({"model":selection.model,"envelope_bytes":envelope_bytes,
                    "reported_calls":run["usage"]["reported_calls"].as_u64().unwrap_or(0)});
                app.save(&mut run).await?;
                let snapshot = workflow::ModelSnapshot {
                    input_digest,
                    instructions_digest: digest(&instructions),
                    schemas_digest: digest(&schemas),
                };
                workflow::record(
                    &mut run,
                    &workflow_policy,
                    &workflow::Input::ContextAssembly(snapshot.clone()),
                    workflow::Decision::Pass,
                )?;
                app.save(&mut run).await?;
                // Effort precedence: this run's model-switcher choice, then the conversation's
                // choice for the chosen entry, then the agent's default for the candidate in use.
                // Levels the provider file no longer lists fall back to the provider default.
                let index = run["failover_state"]["candidate_index"]
                    .as_u64()
                    .unwrap_or(0) as usize;
                let candidate = &run["selection_plan"]["candidates"][index];
                let requested = run["core_model_effort"]
                    .as_str()
                    .or_else(|| {
                        if index == 0 {
                            run["chat_effort"].as_str()
                        } else {
                            None
                        }
                    })
                    .or_else(|| candidate["default_effort"].as_str());
                let core_effort = webagent_models::providers::supported(
                    selection.provider,
                    &selection.model,
                    requested,
                );
                // Image-capable candidates see sent images directly; others keep the durable text.
                let mut projected_input = projected_input;
                if candidate["image_in"] == true {
                    let mut records = std::collections::HashMap::new();
                    for id in super::images::wanted(&projected_input)
                        .into_iter()
                        .take(super::images::MAX_IMAGES * 4)
                    {
                        // Only sent images of this session are worth reading from disk.
                        if let Some(record) = app
                            .store
                            .get(&format!("attachment:{id}"))
                            .await?
                            .filter(|r| r["session_id"] == sid && r["preview"] == true)
                        {
                            if let Ok(record) = app.store.attachment_with_data(record).await {
                                records.insert(id, record);
                            }
                        }
                    }
                    let chosen = super::images::select(&sid, &projected_input, &records);
                    super::images::apply(&mut projected_input, &chosen);
                    // The model sees its own tool screenshots instead of asking another model about them.
                    let bridge = selection.provider.is_bridge();
                    let found = super::images::tool_images(&projected_input, bridge);
                    let mut shots = std::collections::HashMap::new();
                    let most = if bridge {
                        super::images::BRIDGE_MAX_IMAGES
                    } else {
                        super::images::MAX_IMAGES
                    };
                    for (id, _) in found.iter().take(most) {
                        if let Some(record) = app
                            .store
                            .get(&format!("attachment:{id}"))
                            .await?
                            .filter(|r| r["session_id"] == sid)
                        {
                            if let Ok(record) = app.store.attachment_with_data(record).await {
                                shots.insert(id.clone(), record);
                            }
                        }
                    }
                    super::images::insert_tool_images(
                        &mut projected_input,
                        &sid,
                        &found,
                        &shots,
                        bridge,
                    );
                }
                // SESSION TASKS: the reminder call is the history, which already ends with the
                // draft reply, plus the reminder, kept out of history like the closer. The run
                // is reloaded before its response, which clears `pending` there.
                if run["task_reminder"]["pending"] == true {
                    drop_call_note(&mut projected_input);
                    projected_input
                        .push(json!({"role":"user","content":run["task_reminder"]["message"]}));
                }
                // CLOSING TURN: the same request plus a private closer message, kept out of
                // history, so the provider can reuse its cached prefix.
                if run["closing_turn"].is_object() {
                    let settings: gate_controls::AgentClosing =
                        serde_json::from_value(run["closing_settings"].clone()).unwrap_or_default();
                    let message = gate_controls::closer_message(
                        &gate_controls::closer_prompt().unwrap_or_default(),
                        &gate_registry::closing_config()?,
                        &settings,
                        run["closing_turn"]["error"].as_str(),
                    );
                    // The closer replaces the call's note, so the history before it stays the
                    // final request's (bridges compare that prefix).
                    drop_call_note(&mut projected_input);
                    projected_input.push(json!({"role":"user","content":message}));
                }
                if let Some((plan, message, reason, focus)) = background_plan {
                    let request = BackgroundRequest {
                        sid: &sid,
                        selection: &selection,
                        input: &projected_input,
                        schemas: &schemas,
                        instructions: &instructions,
                        effort: core_effort.as_deref(),
                        envelope_bytes,
                        ceiling: model_ceiling,
                        fork_failures: job["fork_failures"].as_u64().unwrap_or(0),
                    };
                    Self::start_background_compaction(
                        &app,
                        &mut run,
                        BackgroundFold {
                            plan,
                            message,
                            reason,
                            focus,
                        },
                        request,
                        &compaction_config,
                        &workflow_policy,
                        &cancel,
                    )
                    .await?;
                }
                // A hard-limit fork holds the main call until its summary is done; it applies
                // on the next pass. When it did not start, the next pass uses the System fold.
                if hard_limit_fork {
                    run["hard_limit_fork"] = Value::Null;
                    if let Some(mut finished) = background_compaction::running(&sid) {
                        run["phase"] = json!("compacting");
                        app.save(&mut run).await?;
                        drop(_guard);
                        tokio::select! {
                            _ = cancel.cancelled() => return Ok(()),
                            _ = finished.wait_for(|done| *done) => {}
                        }
                    } else {
                        run["hard_limit_fork"] = json!("system");
                        app.save(&mut run).await?;
                    }
                    continue;
                }
                // COMPACTION-ONLY RUN: `/compact` on an idle chat makes
                // no model call. It waits for its summary, applies it on the next pass and
                // ends with a short reply. Messages sent meanwhile wait for it unless the
                // user chooses "Send now", which turns this into an ordinary run; the
                // summary then applies at a later boundary.
                if run["compaction_only"] == true {
                    if app.message_forced(&run).await.unwrap_or(false) {
                        run["compaction_only"] = json!(false);
                        run["phase"] = json!("model");
                        app.save(&mut run).await?;
                        continue;
                    }
                    if let Some(mut finished) = background_compaction::running(&sid) {
                        run["phase"] = json!("compacting");
                        app.save(&mut run).await?;
                        drop(_guard);
                        loop {
                            let waiting = tokio::select! {
                                _ = cancel.cancelled() => return Ok(()),
                                _ = async { finished.wait_for(|done| *done).await.map(|_| ()) } => false,
                                _ = tokio::time::sleep(Duration::from_secs(1)) => true,
                            };
                            if !waiting || app.message_forced(&run).await.unwrap_or(false) {
                                break;
                            }
                        }
                        continue;
                    }
                    if app.apply_steering(&mut run).await? {
                        run["compaction_only"] = json!(false);
                        run["phase"] = json!("model");
                        app.save(&mut run).await?;
                        context_accounting.reset();
                        continue;
                    }
                    let text = run["compaction_only_note"]
                        .as_str()
                        .unwrap_or("Earlier context is compacted into a summary block.")
                        .to_owned();
                    match publication::terminal(&app.store, &mut run, &[], &text, &workflow_policy)
                        .await
                    {
                        Ok(()) => return Ok(()),
                        Err(error)
                            if error
                                .to_string()
                                .contains("Pending followup prevents terminal publication") =>
                        {
                            run["compaction_only"] = json!(false);
                            run["phase"] = json!("model");
                            app.save(&mut run).await?;
                            continue;
                        }
                        Err(error) => return Err(error),
                    }
                }
                // MAJOR REORDER: once the provider cache is gone (idle past the model's cache
                // timeout, or changed tool definitions) moving things costs nothing, so the
                // retained rows ahead of the blocks go to the instructions before this call.
                let timeout = Duration::from_secs(
                    60 * cache_timeout_override.unwrap_or_else(|| {
                        webagent_models::providers::cache_timeout_minutes(
                            selection.provider,
                            &selection.model,
                        )
                    }),
                );
                let mut reason = super::cache_estimate::reorder_reason(&sid, &schemas, timeout);
                // A restart forgets the last call, so the run's first pass judges idleness
                // from the session's stored activity; once only, so a reorder cannot repeat.
                if reason.is_none()
                    && !super::cache_estimate::known(&sid)
                    && run["restart_idle_checked"] != json!(true)
                {
                    run["restart_idle_checked"] = json!(true);
                    let last_at = app.store.last_other_run_at(&sid, &id).await?;
                    reason = super::cache_estimate::stored_idle(last_at, crate::now(), timeout);
                }
                if let Some(reason) = reason {
                    let input = run["input"].as_array().cloned().unwrap_or_default();
                    run["input"] = json!(context_compaction::reorder(&input, &compaction_config));
                    run["tool_result_projection"]["rebase"] = json!(true);
                    context_accounting.reset();
                    super::cache_estimate::forget(&sid);
                    super::cache_estimate::log_reorder(&sid, &id, reason);
                    app.store
                        .save_run_observed(
                            &mut run,
                            "context_reordered",
                            &json!({"run_id":id,"reason":reason}),
                        )
                        .await?;
                    continue;
                }
                let fingerprint = super::cache_estimate::Fingerprint::of(
                    &schemas,
                    &instructions,
                    &projected_input,
                );
                // Experimental keep-alive keeps this request to resend shortly before the
                // cache timeout; nothing is kept while the agent has it off.
                if keep_alive {
                    super::keep_alive::record(
                        app.store.records(),
                        &sid,
                        super::keep_alive::Request {
                            run_id: id.to_string(),
                            agent_id: agent_id.to_string(),
                            selection: selection.clone(),
                            input: projected_input.clone(),
                            tools: schemas.clone(),
                            instructions: instructions.to_string(),
                            effort: core_effort.clone(),
                            token_run: run.clone(),
                            timeout,
                        },
                        crate::now(),
                    )
                    .await;
                }
                let prepared_provider = match crate::provider::CallDraft::new(
                    &selection,
                    projected_input,
                    schemas,
                    &sid,
                    &instructions,
                )
                .with_reasoning_effort(core_effort.as_deref())
                .with_bridge_closing(run["closing_turn"].is_object())
                .prepare(
                    crate::ports::AgentMetadata::root(app.store.records()),
                    &cancel,
                )
                .await
                {
                    Ok(value) => value,
                    Err(error) => {
                        Self::pause_context_preparation(
                            &app,
                            &mut run,
                            &cancel,
                            "provider_request",
                            &error,
                        )
                        .await?;
                        return Ok(());
                    }
                };
                drop(_guard);
                let tokens = match app.tokens_for(&selection, &run).await {
                    Ok(v) => v,
                    Err(error) => {
                        let _guard = app.handles.gate.lock().await;
                        let mut run = app.run(&id).await?;
                        if run["generation"] == generation && status(&run) == "running" {
                            app.trusted_model_failure(&mut run, &error, false)?;
                            app.save(&mut run).await?;
                        }
                        drop(_guard);
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        return Ok(());
                    }
                };
                // DISPATCH-HISTORY-TRANSFER: no save may use this consumed local run.
                // Ownership/generation checks below load their own authoritative run.

                let (sender, mut receiver) = tokio::sync::mpsc::channel::<String>(32);
                // Provider fragments remain private; only a final chat row is streamed.
                let collector =
                    tokio::spawn(async move { while receiver.recv().await.is_some() {} });

                // First poll initiates local dispatch while holding the same gate as shutoff.
                let dispatch_guard = app.handles.gate.lock().await;
                let mut current = app.run(&id).await?;
                if current["generation"] != generation
                    || status(&current) != "running"
                    || app.shutoff().await?
                    || cancel.is_cancelled()
                {
                    drop(sender);
                    drop(dispatch_guard);
                    let _ = collector.await;
                    return Ok(());
                }
                if !crate::components::available(crate::ports::AgentMetadata::root(
                    app.store.records(),
                )) {
                    app.component_pause(&mut current).await?;
                    drop(sender);
                    drop(dispatch_guard);
                    let _ = collector.await;
                    return Ok(());
                }
                if !app.run_model_authorized(&current).await {
                    current["status"] = json!("stopped");
                    current["error"] = json!("Access changed before model dispatch");
                    app.save(&mut current).await?;
                    drop(sender);
                    drop(dispatch_guard);
                    let _ = collector.await;
                    return Ok(());
                }
                // Refresh choices which arrived during credential retrieval before dispatch.
                let live_mode = app.session_mode(&sid).await?;
                let live_choice = app
                    .store
                    .get(&format!("session_chat_controls:{sid}"))
                    .await?;
                if live_mode["revision"] != mode["revision"]
                    || live_choice
                        .as_ref()
                        .and_then(|v| v["revision"].as_u64())
                        .unwrap_or(0)
                        != current["chat_model_revision"].as_u64().unwrap_or(0)
                {
                    drop(sender);
                    drop(dispatch_guard);
                    let _ = collector.await;
                    continue;
                }
                if !Self::dispatch_asset_key(
                    &app,
                    &sid,
                    &agent_id,
                    &current["loaded_abilities"],
                    &current["selected_tools"],
                    &current["loaded_skills"],
                    &current["optimization_request"],
                    current["requester_user_id"]
                        .as_str()
                        .unwrap_or(app.store.current_user_id()),
                )
                .await
                .is_ok_and(|key| key == assembled_asset_key)
                {
                    let retry = Self::rebuild_context(&app, &mut current).await?;
                    drop(sender);
                    drop(dispatch_guard);
                    let _ = collector.await;
                    if retry {
                        continue;
                    }
                    return Ok(());
                }
                app.run_candidate(&current).await?;
                if app.current_workflow_policy().await?.revision()? != workflow_policy.revision()? {
                    drop(sender);
                    drop(dispatch_guard);
                    let _ = collector.await;
                    continue;
                }
                crate::tool_output::advance(&mut current)?;
                if match app.dedup_config_fingerprint(&agent_id, &sid).await {
                    Ok(value) => current["tool_result_projection"]["config_fingerprint"] != value,
                    Err(_) => true,
                } || crate::tool_output::effective(app.store.records(), &agent_id).await?
                    != retention_policy
                    || crate::tool_output::Prompts::read_at(
                        crate::ports::AgentMetadata::root(app.store.records()),
                        app.abilities.source_root(),
                    )? != retention_prompts
                {
                    drop(sender);
                    drop(dispatch_guard);
                    let _ = collector.await;
                    continue;
                }
                workflow::record(
                    &mut current,
                    &workflow_policy,
                    &workflow::Input::ModelDispatch(snapshot),
                    workflow::Decision::Pass,
                )?;
                // Pin publication routing to the call whose text will return. A
                // later settings edit changes the next call, not this response.
                current["promotion_mode"] = json!(promotion_mode);
                // Recorded at dispatch so the response's provider cache figures sit beside it.
                current["usage"]["cache_estimate"] =
                    super::cache_estimate::observe(&sid, fingerprint);
                app.save(&mut current).await?;
                if !app
                    .source_fence()
                    .is_ok_and(|key| key == prepared_source_fence)
                {
                    let retry = Self::rebuild_context(&app, &mut current).await?;
                    drop(sender);
                    drop(dispatch_guard);
                    let _ = collector.await;
                    if retry {
                        continue;
                    }
                    return Ok(());
                }
                // DISPATCH-RELEASE-SNAPSHOTS: fencing is committed; do not retain its
                // reconstructed history or consumed local metadata during network waits.
                // Response handling below reloads authoritative state under the gate.
                // "Send now" cancels only this model call; the run token stays live.
                let model_cancel = cancel.child_token();
                // A closing turn is short and its reply is already final: a sent-now message
                // waits for it rather than cancelling it.
                let closing_turn = current["closing_turn"].is_object();
                let steer_probe = json!({"session_id":current["session_id"],
                    "requester_user_id":current["requester_user_id"]});
                drop(current);
                drop(run);
                let mut completion = Box::pin(async {
                    let tokens = &tokens;
                    let ready = prepared_provider.bind_credentials(tokens, &cancel)?;
                    app.provider
                        .dispatch(ready, sender, model_cancel.clone())
                        .await
                });
                let mut immediate = None;
                futures_util::future::poll_fn(|cx| {
                    use std::future::Future;
                    if let std::task::Poll::Ready(value) = completion.as_mut().poll(cx) {
                        immediate = Some(value);
                    }
                    std::task::Poll::Ready(())
                })
                .await;
                drop(dispatch_guard);
                let response = if let Some(value) = immediate {
                    value
                } else {
                    loop {
                        tokio::select! {
                            biased;
                            _=cancel.cancelled()=>break Err(anyhow!("Execution cancelled")),
                            r=&mut completion=>break r,
                            _=tokio::time::sleep(Duration::from_millis(250))=>{
                                if !closing_turn && app.message_forced(&steer_probe).await.unwrap_or(false) {
                                    model_cancel.cancel();
                                    break Err(anyhow!("Model call replaced by a sent-now message"));
                                }
                            }
                        }
                    }
                };
                drop(completion);
                let _ = collector.await;
                let _guard = app.handles.gate.lock().await;
                let mut run = app.run(&id).await?;
                if run["generation"] != generation || status(&run) != "running" {
                    return Ok(());
                }
                if !app.run_model_authorized(&run).await {
                    run["status"] = json!("stopped");
                    run["error"] = json!("Access changed during model call");
                    app.save(&mut run).await?;
                    return Ok(());
                }
                if cancel.is_cancelled() || app.shutoff().await? {
                    if Self::preserve_restart_interruption(&app, &mut run).await? {
                        return Ok(());
                    }
                    run["status"] = json!("stopped");
                    run["error"] =
                        json!("Provider response discarded after cancellation or shutoff");
                    app.save(&mut run).await?;
                    return Ok(());
                }
                // Only the model call was cancelled ("send now"): discard its partial
                // response, take the waiting message and call the model again.
                if model_cancel.is_cancelled() {
                    if app.apply_steering(&mut run).await? {
                        run["phase"] = json!("model");
                        app.save(&mut run).await?;
                        context_accounting.reset();
                    }
                    continue;
                }
                // Reset the bounded freshness retries only after the provider was polled.
                // A change detected by the final source fence must retain its prior count.
                run["context_rebuilds"] = json!(0);
                // A failed closing call never pauses or fails the run: it counts as an attempt.
                let response = match response {
                    Err(error) if run["closing_turn"].is_object() && !cancel.is_cancelled() => {
                        Ok(json!({"output":[],"closer_error":error.to_string()}))
                    }
                    other => other,
                };
                match response {
                    Err(error) => {
                        if cancel.is_cancelled() {
                            run["status"] = json!("stopped");
                            run["error"] = json!("Provider request interrupted");
                        } else {
                            app.trusted_model_failure(&mut run, &error, false)?;
                        }
                        let payload = json!({"run_id":id,
                        "failover_state":run["failover_state"],"error":run["error"]});
                        app.store
                            .save_run_observed(&mut run, "model_selection_failure", &payload)
                            .await?;
                        drop(_guard);
                        if !cancel.is_cancelled() {
                            tokio::time::sleep(Duration::from_secs(2)).await;
                        }
                        return Ok(());
                    }
                    Ok(response) => {
                        X::record_usage(&mut run, &response);
                        super::cache_estimate::log_call(&sid, &id, &run);
                        let usage_row = super::cache_estimate::activity_item(&run);
                        compaction_activity(
                            &mut run,
                            format!("usage:{}", uuid::Uuid::new_v4()),
                            usage_row,
                        );
                        // A completed reply ends this call's failover attempt: the next
                        // call starts with a fresh retry budget and no carried-over
                        // response or tool-effect flags. The pinned candidate and its
                        // history stay; a live uncertain effect is checked at failure.
                        if run["failover_state"].is_object() {
                            run["failover_state"]["retries"] = json!(0);
                            run["failover_state"]["response_started"] = json!(false);
                            run["failover_state"]["uncertain_tool_effect"] = json!(false);
                        }
                        let mut output = response["output"]
                            .as_array()
                            .ok_or_else(|| anyhow!("Missing provider output"))?
                            .clone();
                        let controls = output
                            .iter()
                            .filter(|v| {
                                v["type"] == "function_call"
                                    && v["name"] == crate::tool_output::CONTROL_NAME
                            })
                            .cloned()
                            .collect::<Vec<_>>();
                        for call in controls {
                            let enabled = run["tool_context_policy"]["enabled"] == true;
                            let result = crate::tool_output::control(&mut run, &call, enabled);
                            output.push(
                                json!({"type":"function_call_output","call_id":call["call_id"],
                            "output":result.to_string()}),
                            );
                        }
                        let output = &output;
                        let closing_reply = if run["closing_turn"].is_object() {
                            match closing_answer(&mut run, output, &response, &selection)? {
                                Some(reply) => Some(reply),
                                None => {
                                    // Ask again with the error; nothing enters history.
                                    run["phase"] = json!("model");
                                    app.save(&mut run).await?;
                                    continue;
                                }
                            }
                        } else {
                            None
                        };
                        // SESSION TASKS: an answer to the reminder that calls no tool is dropped,
                        // and the run publishes the reply it already wrote.
                        let reminder_answered = run["task_reminder"]["pending"] == true;
                        if reminder_answered {
                            run["task_reminder"]["pending"] = json!(false);
                        }
                        let reminder_idle = reminder_answered
                            && closing_reply.is_none()
                            && engine_output::tool_calls(output, crate::tool_output::CONTROL_NAME)
                                .is_empty();
                        let closing_reply = closing_reply.or_else(|| {
                            reminder_idle.then(|| {
                                run["task_reminder"]["draft"]
                                    .as_str()
                                    .unwrap_or("")
                                    .to_owned()
                            })
                        });
                        // The closing answer is never published or kept: the run finishes
                        // with the reply it already wrote.
                        let no_output = Vec::new();
                        let output = if closing_reply.is_some() {
                            &no_output
                        } else {
                            output
                        };
                        let calls =
                            engine_output::tool_calls(output, crate::tool_output::CONTROL_NAME);
                        for item in output {
                            run["input"]
                                .as_array_mut()
                                .expect("run input is an array")
                                .push(item.clone());
                        }
                        let text = closing_reply.unwrap_or_else(|| engine_output::text(output));
                        let publication_policy = app.current_workflow_policy().await?;
                        workflow::record(
                            &mut run,
                            &publication_policy,
                            &workflow::Input::ModelOutput {
                                output_digest: digest(&json!(output)),
                            },
                            workflow::Decision::Pass,
                        )?;
                        advance_turn(&mut run);
                        run["attempt"] = json!(0);
                        run["calls"] = json!(calls);
                        run["call_index"] = json!(0);
                        run["phase"] = json!("tools");
                        let closing: gate_controls::AgentClosing =
                            serde_json::from_value(run["closing_settings"].clone())
                                .unwrap_or_default();
                        let closing_enabled = closing.active()
                            && gate_registry::selected(
                                &publication_policy,
                                workflow::Checkpoint::Publication,
                            )?
                            .iter()
                            .any(|item| item.id == gate_registry::CLOSING_STATUS);
                        if calls.is_empty() {
                            if text.trim().is_empty() {
                                let continuations =
                                    run["reasoning_only_completions"].as_u64().unwrap_or(0);
                                if engine_output::continuation(
                                    output,
                                    crate::tool_output::CONTROL_NAME,
                                ) && continuations < 2
                                {
                                    run["reasoning_only_completions"] = json!(continuations + 1);
                                    run["phase"] = json!("model");
                                    app.store
                                        .save_run_observed(
                                            &mut run,
                                            "model_output",
                                            &json!({"output":output}),
                                        )
                                        .await?;
                                    continue;
                                }
                                let kinds = output
                                    .iter()
                                    .map(|item| item["type"].as_str().unwrap_or("unknown"))
                                    .collect::<Vec<_>>();
                                tracing::warn!(run_id=%id, output_types=?kinds,
                                "Provider completed without an assistant message or tool call");
                                run["status"] = json!("failed");
                                run["error"] = json!("Provider completed without a final reply; verify the selected model");
                                app.store
                                    .save_run_observed(
                                        &mut run,
                                        "model_output",
                                        &json!({"output":output}),
                                    )
                                    .await?;
                                return Ok(());
                            }
                            // SESSION TASKS: the first final reply of a run while the session
                            // task list still has open steps gets one more call with a
                            // reminder, so the agent can finish, block or cancel them first.
                            if run["task_reminder"].is_null() {
                                let (task_sid, _, _) = webagent_context::assets::identity(
                                    &run,
                                    app.store.current_user_id(),
                                )?;
                                if let Some(message) = Box::pin(
                                    app.native.session_tasks_end(app.store.records(), &task_sid),
                                )
                                .await?
                                {
                                    run["task_reminder"] =
                                        json!({"draft":text,"message":message,"pending":true});
                                    run["phase"] = json!("model");
                                    app.store
                                        .save_run_observed(
                                            &mut run,
                                            "model_output",
                                            &json!({"output":output}),
                                        )
                                        .await?;
                                    continue;
                                }
                            }
                            // CLOSING TURN: the reply is final; before it publishes, one more
                            // call on this model asks for the status (see `closing_answer`).
                            if closing_enabled && run["closing"].is_null() {
                                run["closing_turn"] = json!({"text":text,"attempts":0});
                                run["phase"] = json!("model");
                                app.store
                                    .save_run_observed(
                                        &mut run,
                                        "model_output",
                                        &json!({"output":output}),
                                    )
                                    .await?;
                                continue;
                            }
                            if text.len() > publication_policy.max_publication_bytes {
                                workflow::record(
                                    &mut run,
                                    &publication_policy,
                                    &workflow::Input::Publication {
                                        text_digest: digest(&json!(text)),
                                    },
                                    workflow::Decision::Reject {
                                        reason: "Publication exceeds configured limit".into(),
                                    },
                                )?;
                                run["status"] = json!("paused");
                                run["error"] =
                                    json!("Publication exceeds configured limit; review required");
                                app.store
                                    .save_run_observed(
                                        &mut run,
                                        "model_output",
                                        &json!({"output":output}),
                                    )
                                    .await?;
                                return Ok(());
                            }
                            match gate_protocol::evaluate_publication(
                                &publication_policy,
                                text.len(),
                            )? {
                                gate_protocol::GateOutcome::Pass => {}
                                gate_protocol::GateOutcome::Reject {
                                    module_id,
                                    reason_code,
                                } => {
                                    workflow::record(&mut run,&publication_policy,
                                    &workflow::Input::Publication {
                                        text_digest:digest(&json!(text)) },
                                    workflow::Decision::Reject {
                                        reason:format!("Publication module {module_id} rejected: {reason_code}") })?;
                                    run["status"] = json!("paused");
                                    run["error"] = json!("Publication rejected by configured gate");
                                    app.store
                                        .save_run_observed(
                                            &mut run,
                                            "model_output",
                                            &json!({"output":output}),
                                        )
                                        .await?;
                                    return Ok(());
                                }
                            }
                            // Journal model output before any following user interaction.
                            // Terminal insertion uses the same stable interaction keys, so
                            // publication and restart cannot duplicate the private output.
                            app.store
                                .save_run_observed(
                                    &mut run,
                                    "model_output",
                                    &json!({"output":output}),
                                )
                                .await?;
                            if app.apply_steering(&mut run).await? {
                                run["phase"] = json!("model");
                                app.save(&mut run).await?;
                                context_accounting.reset();
                                continue;
                            }
                            match publication::terminal(
                                &app.store,
                                &mut run,
                                output,
                                &text,
                                &publication_policy,
                            )
                            .await
                            {
                                Ok(()) => return Ok(()),
                                Err(error)
                                    if error.to_string().contains(
                                        "Pending followup prevents terminal publication",
                                    ) =>
                                {
                                    run["phase"] = json!("model");
                                    app.save(&mut run).await?;
                                    continue;
                                }
                                Err(error) => return Err(error),
                            }
                        }
                        let auto_promote = run["promotion_mode"] == "auto";
                        if auto_promote && engine_output::message_exceeds(output, 16384) {
                            // Keep the provider output private and pause explicitly. A
                            // rejected public row must not leave a half-saved model turn.
                            run["status"] = json!("paused");
                            run["error"] = json!("Auto promotion message exceeds 16384 bytes");
                            app.store
                                .save_run_observed(
                                    &mut run,
                                    "model_output",
                                    &json!({"output":output}),
                                )
                                .await?;
                            return Ok(());
                        }
                        app.store
                            .save_run_observed(
                                &mut run,
                                "model_output",
                                &json!({"output":output,
                            "auto_promote":auto_promote}),
                            )
                            .await?;
                    }
                }
            } else {
                match Self::execute_tool_step(
                    &app,
                    run,
                    _guard,
                    &workflow_policy,
                    &access,
                    &sid,
                    &id,
                    &generation,
                    &cancel,
                    &agent_id,
                )
                .await?
                {
                    std::ops::ControlFlow::Continue(()) => continue,
                    std::ops::ControlFlow::Break(()) => return Ok(()),
                }
            }
        }
    }
}

/// The running request a background summary may copy; see `start_background_compaction`.
struct BackgroundRequest<'a> {
    sid: &'a str,
    selection: &'a provider::ProviderSelection,
    input: &'a [Value],
    schemas: &'a [Value],
    instructions: &'a str,
    effort: Option<&'a str>,
    envelope_bytes: usize,
    ceiling: usize,
    /// Failed bridge forks in a row for this session (reset by a finished summary).
    fork_failures: u64,
}

/// One background summary to start: its plan, the private message a same-model fork adds,
/// why it runs (`automatic` at the trigger, `requested` by the agent's task fold or `/compact`) and the
/// user's focus, if any.
struct BackgroundFold {
    plan: context_compaction::Plan,
    message: String,
    reason: &'static str,
    focus: Option<String>,
}

/// Queue one activity row for the next run save. Rows are stored in `interactions`
/// under an activity-only step (`compaction:`, `usage:`), which session context assembly
/// skips.
fn compaction_activity(run: &mut Value, step: String, item: Value) {
    let entry = json!({"step":step,"item":item});
    match run["_activity_items"].as_array_mut() {
        Some(items) => items.push(entry),
        None => run["_activity_items"] = json!([entry]),
    }
}

/// Read one closing-turn answer. A valid `flag:`/`note:` answer, or the last attempt's
/// fallback, sets `closing` and returns the reply to publish; otherwise the next attempt
/// carries the error. Each attempt is a `closing:` activity row, never model context.
fn closing_answer(
    run: &mut Value,
    output: &[Value],
    response: &Value,
    selection: &provider::ProviderSelection,
) -> Result<Option<String>> {
    let config = gate_registry::closing_config()?;
    let settings: gate_controls::AgentClosing =
        serde_json::from_value(run["closing_settings"].clone()).unwrap_or_default();
    let attempt = run["closing_turn"]["attempts"].as_u64().unwrap_or(0) + 1;
    let of = u64::from(config.closer.attempts);
    let answer = if let Some(error) = response["closer_error"].as_str() {
        Err(error.to_owned())
    } else if output.iter().any(|item| item["type"] == "function_call") {
        Err("a tool was called instead of answering".to_owned())
    } else {
        gate_controls::parse_closer(&engine_output::text(output), &config, &settings)
            .map_err(|error| error.to_string())
    };
    let mut item = json!({"type":"closing_status","attempt":attempt,"of":of,
        "model":selection.model});
    let step = format!("closing:status:{}:{attempt}", model_turn(run));
    let status = match answer {
        Ok(status) => {
            item["flag"] = status["flag"].clone();
            item["notification"] = status["notification"].clone();
            status
        }
        Err(error) if attempt < of => {
            item["error"] = json!(error);
            compaction_activity(run, step, item);
            run["closing_turn"]["attempts"] = json!(attempt);
            run["closing_turn"]["error"] = json!(error);
            return Ok(None);
        }
        Err(error) => {
            item["error"] = json!(error);
            item["fallback"] = json!(true);
            gate_controls::fallback_status(&config, &settings)
        }
    };
    compaction_activity(run, step, item);
    let reply = run["closing_turn"]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    run["closing"] = status;
    run["closing_turn"] = Value::Null;
    Ok(Some(reply))
}

/// Leave the compacting phase with a result row; `outcome` holds `summary` or `error`.
fn compaction_result(run: &mut Value, step: &str, mut outcome: Value) {
    run["phase"] = json!("model");
    outcome["type"] = json!("compaction_result");
    compaction_activity(run, format!("{step}:result"), outcome);
}
