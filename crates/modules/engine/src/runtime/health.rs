//! Run health: a content-free record of one run's tool calls, kept in `run["run_health"]`
//! and in the run's stored view for later review. Failed calls are grouped by tool, error
//! kind and a normalized message template (ids, numbers, paths, quoted text and selectors
//! removed); each group counts its retries and recoveries. Cheap rework signals count
//! rewritten files, repeated screenshot labels and writes under another session's
//! workspace. No model text, arguments or results are stored beyond the template.
//! [`aggregate`] merges stored records across runs and sessions for the issues views.
use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};

/// Tool result statuses the chat UI (`ui/chat/core/activity.js`) marks as failed.
const FAILED_STATUSES: [&str; 8] = [
    "error",
    "rejected",
    "denied",
    "failed",
    "cancelled",
    "timeout",
    "uncertain",
    "interrupted",
];
/// Most distinct failure kinds kept per run; later ones count under `other`.
const MAX_KINDS: usize = 16;
/// Most issue groups kept per run; later failures only count in `groups_dropped`.
const MAX_GROUPS: usize = 24;
/// Most groups returned by an aggregate view.
const MAX_AGGREGATE_GROUPS: usize = 50;
/// Most path/label fingerprints remembered per run for rework detection.
const MAX_SEEN: usize = 64;
/// Failures remembered as unanswered, so a later success can credit the fix.
const MAX_OPEN_FAILURES: usize = 16;
/// Longest normalized message template.
const MAX_TEMPLATE: usize = 120;
/// Tools whose successful call writes the file named by `path`.
const WRITE_TOOLS: [&str; 2] = ["file_write", "file_edit"];

/// Classifies a stored tool output as success, error, uncertain or denied, with its `error_kind`.
/// Mirrors the chat UI: only a JSON object whose `status` is a failure status counts as failed.
#[must_use]
pub fn result_status(output: &Value) -> (&'static str, Option<String>) {
    let parsed;
    let value = match output {
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(value) => {
                parsed = value;
                &parsed
            }
            Err(_) => return ("success", None),
        },
        other => other,
    };
    let kind = match value["error_kind"].as_str() {
        Some(kind) => Some(kind.to_owned()),
        // A script reports its own failure as a step result inside `output`.
        None => nested_failure(value)
            .and_then(|step| step["error_kind"].as_str())
            .map(str::to_owned),
    };
    let status = value["status"].as_str().unwrap_or("");
    let class = if value["uncertain"] == true || matches!(status, "uncertain" | "interrupted") {
        "uncertain"
    } else if matches!(status, "denied" | "rejected" | "held") {
        "denied"
    } else if FAILED_STATUSES.contains(&status) {
        "error"
    } else {
        return ("success", None);
    };
    (class, kind)
}

/// The step a script stopped on, when that step carries the failure.
fn nested_failure(value: &Value) -> Option<&Value> {
    let steps = value["output"]["steps"].as_array()?;
    let stopped = usize::try_from(value["output"]["stopped_at_step"].as_u64()?).ok()?;
    let step = steps.get(stopped.checked_sub(1)?)?;
    let result = &step["result"];
    (result_status(result).0 != "success").then_some(result)
}

/// How many artifacts a result produced, or `None` when it reports no artifacts at all.
fn artifacts(output: &Value) -> Option<usize> {
    let parsed;
    let value = match output {
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(value) => {
                parsed = value;
                &parsed
            }
            Err(_) => return None,
        },
        other => other,
    };
    value["artifacts"].as_array().map(Vec::len)
}

/// Count produced artifacts, and how many calls came since the last one: a run that
/// "succeeds" without producing anything stays visible.
fn count_effect(health: &mut Value, output: &Value) {
    bump(&mut health["calls_since_effect"]);
    if !health["artifacts"].is_number() {
        health["artifacts"] = json!(0);
    }
    let Some(produced) = artifacts(output) else {
        return;
    };
    bump(&mut health["artifact_calls"]);
    if produced == 0 {
        return;
    }
    add(&mut health["artifacts"], &json!(produced));
    health["calls_since_effect"] = json!(0);
}

/// The failure message of a tool output: `message`, `error` or `output`, first line only.
fn message(output: &Value) -> String {
    let parsed;
    let value = match output {
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(value) => {
                parsed = value;
                &parsed
            }
            Err(_) => return String::new(),
        },
        other => other,
    };
    let pick = |value: &Value| {
        ["message", "error", "output"]
            .iter()
            .find_map(|field| value[*field].as_str())
            .map(str::to_owned)
    };
    // A script's own result object holds the reason its step failed.
    let text = pick(value)
        .or_else(|| nested_failure(value).and_then(pick))
        .unwrap_or_default();
    let text = text.lines().next().unwrap_or_default();
    text.chars().take(400).collect()
}

/// Normalize a failure message to a template: quoted text, paths and URLs, selectors,
/// ids and long tokens become placeholders and digit runs become `<n>`.
#[must_use]
pub fn template(text: &str) -> String {
    // Quoted spans first: they often hold selectors or user text with spaces.
    let mut unquoted = String::with_capacity(text.len());
    let mut quote: Option<char> = None;
    for c in text.chars() {
        match quote {
            Some(open) if c == open => {
                quote = None;
                unquoted.push_str("<q>");
            }
            Some(_) => {}
            None if matches!(c, '"' | '\'' | '`') => quote = Some(c),
            None => unquoted.push(c),
        }
    }
    if quote.is_some() {
        unquoted.push_str("<q>");
    }
    let mut words = Vec::new();
    for word in unquoted.split_whitespace() {
        let core = word.trim_end_matches([',', ';', ':', '.', ')']);
        let tail = &word[core.len()..];
        let hex = core.chars().filter(char::is_ascii_hexdigit).count();
        let replaced = if core.contains("<q>") {
            core.to_owned()
        } else if core.contains('/') || core.contains('\\') {
            "<path>".to_owned()
        } else if core.starts_with(['#', '.', '['])
            || (core.contains(['[', '>', '=']) && core.chars().any(|c| c.is_ascii_alphabetic()))
        {
            "<sel>".to_owned()
        } else if core.len() > 32
            || (core.len() >= 8
                && hex * 10 >= core.len() * 8
                && core.chars().any(|c| c.is_ascii_digit()))
        {
            "<id>".to_owned()
        } else {
            let mut out = String::new();
            let mut digits = false;
            for c in core.chars() {
                if c.is_ascii_digit() {
                    if !digits {
                        out.push_str("<n>");
                    }
                    digits = true;
                } else {
                    digits = false;
                    out.push(c);
                }
            }
            out
        };
        words.push(format!("{replaced}{tail}"));
    }
    let mut joined = words.join(" ");
    if joined.len() > MAX_TEMPLATE {
        let mut end = MAX_TEMPLATE;
        while !joined.is_char_boundary(end) {
            end -= 1;
        }
        joined.truncate(end);
    }
    joined
}

fn bump(slot: &mut Value) {
    *slot = json!(slot.as_u64().unwrap_or(0) + 1);
}

fn fingerprint(value: impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// A tool's arguments as an object (stored calls may hold them as a JSON string).
fn arguments(pending: &Value) -> Value {
    match &pending["arguments"] {
        Value::String(text) => serde_json::from_str(text).unwrap_or(Value::Null),
        other => other.clone(),
    }
}

/// Remember a fingerprint; `true` when it was already seen this run.
fn seen_before(list: &mut Value, print: u64) -> bool {
    if !list.is_array() {
        *list = json!([]);
    }
    let items = list.as_array_mut().expect("array");
    if items.iter().any(|v| v.as_u64() == Some(print)) {
        return true;
    }
    if items.len() < MAX_SEEN {
        items.push(json!(print));
    }
    false
}

/// A write under `/workspaces/<id>` where `<id>` is not this session. A sub-agent writing
/// in its parent's workspace counts too; the review decides whether that was intended.
fn foreign_workspace(args: &Value, sid: &str) -> bool {
    args["path"]
        .as_str()
        .and_then(|path| path.strip_prefix("/workspaces/"))
        .and_then(|rest| rest.split('/').next())
        .is_some_and(|owner| !owner.is_empty() && owner != sid)
}

/// Count one model step.
pub fn record_step(run: &mut Value) {
    bump(&mut run["run_health"]["steps"]);
}

/// Count one review prompt: whether the call-scoped notice was added or suppressed.
pub fn record_review_prompt(run: &mut Value, prompted: bool) {
    let health = &mut run["run_health"];
    bump(
        &mut health[if prompted {
            "review_prompts"
        } else {
            "review_skipped"
        }],
    );
}

/// Count one tool result for the pending call; a failure is also counted by
/// `class/error_kind` and in its issue group, and retries and rework are tracked.
pub fn record_result(run: &mut Value, output: &Value) {
    let tool = run["pending"]["name"].as_str().unwrap_or("");
    let tool = if !tool.is_empty()
        && tool.len() <= 64
        && tool.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        tool.to_owned()
    } else {
        "unknown".to_owned()
    };
    let args = arguments(&run["pending"]);
    let sid = run["session_id"].as_str().unwrap_or("").to_owned();
    let signature = fingerprint((&tool, args.to_string()));
    // Fingerprints for retry and rework detection stay in `run_health_track`, which the
    // stored run view leaves out.
    let last = run["run_health_track"]["last"].take();
    // Failures that no later success of the same tool has answered yet, so a fix counts
    // even when other calls came in between.
    let mut open = run["run_health_track"]["open_failures"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let (class, kind) = result_status(output);
    let failed = class != "success";
    let health = &mut run["run_health"];
    bump(&mut health["calls"]);
    if tool == "agent_review" && !failed {
        bump(&mut health["review_notes"]);
    }
    count_effect(health, output);

    // A call of the same tool right after a failure retries that failure's group.
    if last["failed"] == true && last["tool"].as_str() == Some(tool.as_str()) {
        let index = last["group"].as_u64().and_then(|i| usize::try_from(i).ok());
        if let Some(group) = index.and_then(|i| health["groups"].get_mut(i)) {
            bump(&mut group["retries"]);
            if last["signature"].as_u64() == Some(signature) {
                bump(&mut group["identical_retries"]);
            }
        }
    }

    let mut group_index = None;
    let mut fresh_entry: Option<Value> = None;
    if failed {
        bump(&mut health["failed"]);
        // Only short identifier-like kinds are kept: no tool text enters the record.
        let kind = kind
            .filter(|k| k.len() <= 64 && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
            .unwrap_or_else(|| "unspecified".into());
        let mut key = format!("{class}/{kind}");
        if !health["failed_by_kind"].is_object() {
            health["failed_by_kind"] = json!({});
        }
        let kinds = &mut health["failed_by_kind"];
        if kinds.get(&key).is_none() && kinds.as_object().is_some_and(|m| m.len() >= MAX_KINDS) {
            key = "other".into();
        }
        bump(&mut kinds[key]);

        let template = template(&message(output));
        if !health["groups"].is_array() {
            health["groups"] = json!([]);
        }
        let groups = health["groups"].as_array_mut().expect("array");
        let found = groups.iter().position(|g| {
            g["tool"] == tool.as_str()
                && g["class"] == class
                && g["kind"] == kind.as_str()
                && g["template"] == template.as_str()
        });
        let mut fresh = false;
        group_index = match found {
            Some(i) => Some(i),
            None if groups.len() < MAX_GROUPS => {
                groups.push(
                    json!({"tool":tool,"class":class,"kind":kind,"template":template,
                    "count":0,"retries":0,"identical_retries":0,"recovered":0}),
                );
                fresh = true;
                Some(groups.len() - 1)
            }
            None => None,
        };
        fresh_entry = fresh.then(|| {
            // One chance to explain a group the first time it appears; the run loop turns
            // this into a single call-scoped notice and clears it.
            json!({"tool":tool,"class":class,"kind":kind,"template":template})
        });
        match group_index {
            Some(i) => bump(&mut groups[i]["count"]),
            None => bump(&mut health["groups_dropped"]),
        }
    } else {
        let track = &mut run["run_health_track"];
        let mut rework = Vec::new();
        if WRITE_TOOLS.contains(&tool.as_str()) {
            if let Some(path) = args["path"].as_str() {
                if seen_before(&mut track["written"], fingerprint(path)) {
                    rework.push("rewritten_files");
                }
            }
            if foreign_workspace(&args, &sid) {
                rework.push("other_workspace_writes");
            }
        }
        if args["action"] == "screenshot" {
            if let Some(label) = args["label"].as_str().filter(|l| !l.is_empty()) {
                if seen_before(&mut track["labels"], fingerprint(label)) {
                    rework.push("repeated_screenshots");
                }
            }
        }
        for signal in rework {
            bump(&mut run["run_health"]["rework"][signal]);
        }
    }
    // A later success of the same tool answers its earlier failures, whatever came between.
    if failed {
        if let Some(index) = group_index {
            open.push(json!({"tool":tool,"group":index}));
            while open.len() > MAX_OPEN_FAILURES {
                open.remove(0);
            }
        }
    } else {
        let mut credited = Vec::new();
        for entry in &open {
            if entry["tool"].as_str() == Some(tool.as_str()) {
                if let Some(index) = entry["group"].as_u64() {
                    if !credited.contains(&index) {
                        credited.push(index);
                    }
                }
            }
        }
        let mut groups = run["run_health"]["groups"].as_array_mut();
        for index in credited {
            let group = usize::try_from(index)
                .ok()
                .and_then(|i| groups.as_mut().and_then(|list| list.get_mut(i)));
            if let Some(group) = group {
                bump(&mut group["recovered"]);
            }
        }
        open.retain(|entry| entry["tool"].as_str() != Some(tool.as_str()));
    }
    if let Some(entry) = fresh_entry {
        // In `run_health`, not the turn-local track: every turn reloads the run, so a
        // marker outside the persisted health record never reaches the next model call.
        tracing::warn!(
            tool = entry["tool"].as_str().unwrap_or(""),
            kind = entry["kind"].as_str().unwrap_or(""),
            "review marker set for a newly seen issue group"
        );
        run["run_health"]["review_pending"] = entry;
    }
    run["run_health_track"]["open_failures"] = Value::Array(open);
    run["run_health_track"]["last"] = json!({"tool":tool,"signature":signature,
        "failed":failed,"group":group_index});
    refresh(run);
}

/// Store the repeat count (distinct calls the repeat guard matched to an earlier identical
/// call) and return the record.
pub fn refresh(run: &mut Value) -> &Value {
    let repeats = run["retry_guard"]["decisions"]
        .as_array()
        .map_or(0, |list| {
            list.iter()
                .filter_map(|d| d["call_id"].as_str())
                .collect::<BTreeSet<_>>()
                .len()
        });
    run["run_health"]["repeats"] = json!(repeats);
    &run["run_health"]
}

fn add(total: &mut Value, part: &Value) {
    *total = json!(total.as_u64().unwrap_or(0) + part.as_u64().unwrap_or(0));
}

type GroupKey = (String, String, String, String);

/// Merge stored run views (`{session_id, run_health, ...}`) into one issues view: totals,
/// rework counts and issue groups across runs, ranked by cost (failed calls plus retries).
/// Each group also counts the runs and sessions it appeared in and the runs where it never
/// recovered. Only counts, kinds, tool names and templates are read.
#[must_use]
pub fn aggregate(views: &[Value]) -> Value {
    let mut totals = json!({"runs":0,"steps":0,"calls":0,"failed":0,"repeats":0,"groups_dropped":0,
               "artifacts":0,"artifact_calls":0,
               "review_prompts":0,"review_skipped":0,"review_notes":0});
    let mut rework = json!({});
    let mut sessions = BTreeSet::new();
    let mut groups: BTreeMap<GroupKey, (Value, BTreeSet<String>)> = BTreeMap::new();
    for view in views {
        let health = &view["run_health"];
        if !health.is_object() {
            continue;
        }
        let sid = view["session_id"].as_str().unwrap_or("").to_owned();
        sessions.insert(sid.clone());
        bump(&mut totals["runs"]);
        for field in [
            "steps",
            "calls",
            "failed",
            "repeats",
            "groups_dropped",
            "artifacts",
            "artifact_calls",
            "review_prompts",
            "review_skipped",
            "review_notes",
        ] {
            add(&mut totals[field], &health[field]);
        }
        if let Some(signals) = health["rework"].as_object() {
            for (name, count) in signals {
                add(&mut rework[name], count);
            }
        }
        for group in health["groups"].as_array().into_iter().flatten() {
            let text = |f: &str| group[f].as_str().unwrap_or("").to_owned();
            let key = (text("tool"), text("class"), text("kind"), text("template"));
            let (entry, seen) = groups.entry(key.clone()).or_insert_with(|| {
                (
                    json!({"tool":key.0,"class":key.1,"kind":key.2,"template":key.3,"count":0,
                        "retries":0,"identical_retries":0,"recovered":0,"runs":0,
                        "unrecovered_runs":0}),
                    BTreeSet::new(),
                )
            });
            for field in ["count", "retries", "identical_retries", "recovered"] {
                add(&mut entry[field], &group[field]);
            }
            bump(&mut entry["runs"]);
            if group["recovered"].as_u64().unwrap_or(0) == 0 {
                bump(&mut entry["unrecovered_runs"]);
            }
            seen.insert(sid.clone());
        }
    }
    let mut ranked: Vec<Value> = groups
        .into_values()
        .map(|(mut group, seen)| {
            let calls =
                group["count"].as_u64().unwrap_or(0) + group["retries"].as_u64().unwrap_or(0);
            group["calls"] = json!(calls);
            group["sessions"] = json!(seen.len());
            group
        })
        .collect();
    ranked.sort_by(|a, b| {
        b["calls"]
            .as_u64()
            .cmp(&a["calls"].as_u64())
            .then_with(|| b["runs"].as_u64().cmp(&a["runs"].as_u64()))
    });
    let truncated = ranked.len() > MAX_AGGREGATE_GROUPS;
    ranked.truncate(MAX_AGGREGATE_GROUPS);
    totals["sessions"] = json!(sessions.len());
    json!({"totals":totals,"rework":rework,"groups":ranked,"groups_truncated":truncated})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(run: &mut Value, tool: &str, args: Value, output: Value) {
        run["pending"] = json!({"name":tool,"arguments":args.to_string()});
        record_result(run, &output);
    }

    #[test]
    fn classifies_tool_results_like_the_chat_ui() {
        let cases = [
            (json!("plain text"), "success", None),
            (json!(r#"{"status":"ok","output":"x"}"#), "success", None),
            (
                json!(r#"{"status":"error","error_kind":"not_found"}"#),
                "error",
                Some("not_found"),
            ),
            (
                json!(r#"{"status":"error","uncertain":true,"error_kind":"outcome_unknown"}"#),
                "uncertain",
                Some("outcome_unknown"),
            ),
            (json!({"status":"denied","output":"no"}), "denied", None),
            (json!(r#"{"status":"timeout"}"#), "error", None),
        ];
        for (output, status, kind) in cases {
            assert_eq!(
                result_status(&output),
                (status, kind.map(str::to_owned)),
                "{output}"
            );
        }
    }

    #[test]
    fn a_later_success_answers_an_earlier_failure() {
        let mut run = json!({"session_id":"s"});
        call(
            &mut run,
            "file_write",
            json!({"path":"/w/a"}),
            json!({"status":"failed","error_kind":"invalid_arguments","output":"mode"}),
        );
        call(
            &mut run,
            "file_read",
            json!({"path":"/w/a"}),
            json!({"status":"success","output":"x"}),
        );
        call(
            &mut run,
            "file_write",
            json!({"path":"/w/a"}),
            json!({"status":"success","output":"written"}),
        );
        let groups = run["run_health"]["groups"].as_array().expect("groups");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["recovered"], 1);
        assert_eq!(groups[0]["retries"], 0);
    }

    #[test]
    fn artifacts_are_counted_and_a_capture_without_one_stays_visible() {
        let mut run = json!({"session_id":"s"});
        call(
            &mut run,
            "browser_input",
            json!({}),
            json!({"status":"success","artifacts":[]}),
        );
        assert_eq!(run["run_health"]["artifact_calls"], 1);
        assert_eq!(run["run_health"]["artifacts"], 0);
        assert_eq!(run["run_health"]["calls_since_effect"], 1);
        call(
            &mut run,
            "script_run",
            json!({}),
            json!({"status":"success","artifacts":[{"step":10},{"step":18}]}),
        );
        assert_eq!(run["run_health"]["artifacts"], 2);
        assert_eq!(run["run_health"]["calls_since_effect"], 0);
        call(
            &mut run,
            "file_read",
            json!({"path":"/w"}),
            json!({"status":"success","output":"x"}),
        );
        assert_eq!(run["run_health"]["calls_since_effect"], 1);
    }

    #[test]
    fn totals_sum_the_review_counters() {
        let view = json!({"session_id":"s","run_health":{"groups":[],
            "review_prompts":2,"review_skipped":1,"review_notes":3}});
        let totals = &aggregate(&[view])["totals"];
        assert_eq!(totals["review_prompts"], 2);
        assert_eq!(totals["review_skipped"], 1);
        assert_eq!(totals["review_notes"], 3);
    }

    #[test]
    fn review_prompts_skips_and_notes_are_counted() {
        let mut run = json!({"session_id":"s"});
        record_review_prompt(&mut run, true);
        record_review_prompt(&mut run, true);
        record_review_prompt(&mut run, false);
        assert_eq!(run["run_health"]["review_prompts"], 2);
        assert_eq!(run["run_health"]["review_skipped"], 1);
        call(
            &mut run,
            "agent_review",
            json!({"tool":"file_write","kind":"invalid_arguments","note":"x"}),
            json!({"status":"success","output":{"recorded":true}}),
        );
        assert_eq!(run["run_health"]["review_notes"], 1);
        call(
            &mut run,
            "agent_review",
            json!({"tool":"file_write","kind":"invalid_arguments","note":""}),
            json!({"status":"rejected","error_kind":"invalid_arguments","output":"empty"}),
        );
        assert_eq!(
            run["run_health"]["review_notes"], 1,
            "only a written note counts"
        );
    }

    #[test]
    fn a_new_group_asks_for_a_note_once() {
        let mut run = json!({"session_id":"s"});
        let failure = json!({"status":"failed","error_kind":"invalid_arguments","output":"mode"});
        call(
            &mut run,
            "file_write",
            json!({"path":"/w"}),
            failure.clone(),
        );
        assert_eq!(run["run_health"]["review_pending"]["tool"], "file_write");
        assert_eq!(
            run["run_health"]["review_pending"]["kind"],
            "invalid_arguments"
        );
        run["run_health"]["review_pending"] = Value::Null;
        call(&mut run, "file_write", json!({"path":"/w"}), failure);
        assert!(
            run["run_health"]["review_pending"].is_null(),
            "a known group does not ask again"
        );
    }

    #[test]
    fn a_denied_chat_control_keeps_its_tool_name() {
        let mut run = json!({"session_id":"s"});
        call(
            &mut run,
            "publish_main_message",
            json!({}),
            json!({"status":"denied","output":"Invalid working message: No message precedes this call in the same reply; pass text"}),
        );
        let groups = run["run_health"]["groups"].as_array().expect("groups");
        assert_eq!(groups[0]["tool"], "publish_main_message");
        assert_eq!(groups[0]["class"], "denied");
    }

    #[test]
    fn a_script_failure_is_read_from_its_step_result() {
        let run = json!({"status":"error","artifacts":[],"output":{
            "script":"capture","steps_run":1,"steps_total":18,"stopped_at_step":1,
            "steps":[{"tool":"browser_read","result":{
                "status":"error","error_kind":"browser_invalid_request",
                "output":"Private browser request rejected: Unsupported current-session read route"
            }}]
        }});
        assert_eq!(
            result_status(&run),
            ("error", Some("browser_invalid_request".to_owned()))
        );
        assert_eq!(
            message(&run),
            "Private browser request rejected: Unsupported current-session read route"
        );
        // A successful script still classifies as success and keeps no message.
        let ok = json!({"status":"success","artifacts":[],"output":{
            "steps_run":18,"steps_total":18,"stopped_at_step":18,
            "steps":[{"tool":"browser_read","result":{"status":"success","output":"ok"}}]
        }});
        assert_eq!(result_status(&ok), ("success", None));
        assert_eq!(message(&ok), "");
    }

    #[test]
    fn templates_strip_ids_numbers_paths_and_selectors() {
        assert_eq!(
            template("No element matches '#save-btn' after 5000 ms"),
            "No element matches <q> after <n> ms"
        );
        assert_eq!(
            template("File /workspaces/abc/src/app.rs not found: line 42"),
            "File <path> not found: line <n>"
        );
        assert_eq!(
            template("Run 3f2a9c1e-77aa-4b1e-9d0c-1234567890ab is busy"),
            "Run <id> is busy"
        );
        assert_eq!(
            template("Selector div.card>button[data-x=1] missing"),
            "Selector <sel> missing"
        );
        assert_eq!(
            template("open https://example.test/a?b=1 failed"),
            "open <path> failed"
        );
        assert!(template(&"word ".repeat(100)).len() <= MAX_TEMPLATE);
        // Messages that differ only in their variable parts share one template.
        assert_eq!(
            template("timeout after 30s on step 2"),
            template("timeout after 45s on step 7")
        );
    }

    #[test]
    fn record_counts_kinds_and_templates_only() {
        let mut run = json!({});
        record_step(&mut run);
        call(&mut run, "file_read", json!({"path":"a"}), json!("fine"));
        let failure = json!({"status":"error","error_kind":"browser_invalid_request",
            "message":"Bad selector \"secret text\" (42)","output":"more secret"});
        call(
            &mut run,
            "browser_read",
            json!({"x":"secret arg"}),
            json!(failure.to_string()),
        );
        call(
            &mut run,
            "browser_read",
            json!({}),
            json!({"status":"error","error_kind":"has spaces and text"}),
        );
        let health = run["run_health"].clone();
        assert_eq!(health["steps"], 1);
        assert_eq!(health["calls"], 3);
        assert_eq!(health["failed"], 2);
        assert_eq!(
            health["failed_by_kind"],
            json!({"error/browser_invalid_request":1,"error/unspecified":1})
        );
        assert_eq!(health["groups"][0]["template"], "Bad selector <q> (<n>)");
        let text = health.to_string();
        assert!(!text.contains("secret"));
        assert!(!text.contains("signature"));
    }

    #[test]
    fn kinds_and_groups_are_bounded() {
        let mut run = json!({});
        for i in 0..40 {
            call(
                &mut run,
                &format!("tool{i}"),
                json!({}),
                json!({"status":"error","error_kind":format!("k{i}")}),
            );
        }
        let health = &run["run_health"];
        let kinds = health["failed_by_kind"].as_object().unwrap();
        assert_eq!(kinds.len(), MAX_KINDS + 1);
        assert_eq!(kinds["other"], 40 - MAX_KINDS as u64);
        assert_eq!(health["groups"].as_array().unwrap().len(), MAX_GROUPS);
        assert_eq!(health["groups_dropped"], 40 - MAX_GROUPS as u64);
    }

    #[test]
    fn retries_and_recovery_are_detected() {
        let mut run = json!({});
        let fail = json!({"status":"error","error_kind":"not_found","message":"No row 7"});
        let args = json!({"id":7});
        call(&mut run, "lookup", args.clone(), fail.clone());
        call(&mut run, "lookup", args.clone(), fail.clone());
        call(&mut run, "lookup", json!({"id":8}), json!("ok"));
        // A failure after a success is a new attempt; another tool after it is no retry.
        call(&mut run, "lookup", args, fail);
        call(&mut run, "other", json!({}), json!("ok"));
        let group = &run["run_health"]["groups"][0];
        assert_eq!(group["count"], 3);
        assert_eq!(group["retries"], 2);
        assert_eq!(group["identical_retries"], 1);
        assert_eq!(group["recovered"], 1);
        assert_eq!(run["run_health"]["groups"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn rework_signals() {
        let mut run = json!({"session_id":"own"});
        let write = |path: &str| json!({ "path": path });
        call(
            &mut run,
            "file_write",
            write("/workspaces/own/a.rs"),
            json!("ok"),
        );
        call(
            &mut run,
            "file_edit",
            write("/workspaces/own/a.rs"),
            json!("ok"),
        );
        call(
            &mut run,
            "file_write",
            write("/workspaces/other/b"),
            json!("ok"),
        );
        let shot = json!({"action":"screenshot","label":"phone"});
        call(&mut run, "browser_read", shot.clone(), json!("ok"));
        call(&mut run, "browser_read", shot, json!("ok"));
        // A failed write is not rework.
        let failed = json!({"status":"error"});
        call(&mut run, "file_edit", write("/workspaces/own/a.rs"), failed);
        assert_eq!(
            run["run_health"]["rework"],
            json!({"rewritten_files":1,"other_workspace_writes":1,"repeated_screenshots":1})
        );
    }

    #[test]
    fn repeats_count_distinct_calls() {
        let decision = |id: &str, d: &str| json!({"call_id":id,"decision":d});
        let mut run = json!({"retry_guard":{"decisions":[
            decision("a","approval_required"), decision("a","approved"),
            decision("b","held"), decision("c","repeat")]}});
        assert_eq!(refresh(&mut run)["repeats"], 3);
    }

    #[test]
    fn aggregate_merges_runs_and_sessions_by_cost() {
        let timeout = |n: u64| {
            json!({"status":"error","error_kind":"timeout",
                "message":format!("Timed out after {n}s")})
        };
        let mut one = json!({"session_id":"root"});
        call(&mut one, "browser_read", json!({}), timeout(5));
        call(&mut one, "browser_read", json!({}), json!("ok"));
        let mut two = json!({"session_id":"child"});
        for n in 0..3 {
            call(&mut two, "browser_read", json!({ "n": n }), timeout(n * 10));
        }
        let plain = json!({"status":"error","message":"x"});
        call(&mut two, "file_read", json!({}), plain);
        let views: Vec<Value> = [one, two]
            .iter()
            .map(|r| json!({"session_id":r["session_id"],"run_health":r["run_health"]}))
            .chain([json!({"session_id":"none"})])
            .collect();
        let view = aggregate(&views);
        assert_eq!(view["totals"]["runs"], 2);
        assert_eq!(view["totals"]["sessions"], 2);
        assert_eq!(view["totals"]["failed"], 5);
        let top = &view["groups"][0];
        assert_eq!(top["tool"], "browser_read");
        assert_eq!(top["template"], "Timed out after <n>s");
        assert_eq!(top["count"], 4);
        assert_eq!(top["retries"], 3);
        assert_eq!(top["calls"], 7);
        assert_eq!(top["runs"], 2);
        assert_eq!(top["sessions"], 2);
        assert_eq!(top["unrecovered_runs"], 1);
        assert_eq!(view["groups"][1]["tool"], "file_read");
    }
}
