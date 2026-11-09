//! Run health: a content-free count of one run's tool calls, failures and repeats, kept in
//! `run["run_health"]` for later cross-user aggregation, and the call-scoped self-review
//! note added when the configured thresholds are crossed.
use super::config::SelfReview;
use crate::context_input::{Contribution, Lifetime};
use serde_json::{json, Value};
use std::collections::BTreeSet;

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
    let kind = value["error_kind"].as_str().map(str::to_owned);
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

fn bump(slot: &mut Value) {
    *slot = json!(slot.as_u64().unwrap_or(0) + 1);
}

/// Count one model step.
pub fn record_step(run: &mut Value) {
    bump(&mut run["run_health"]["steps"]);
}

/// Count one tool result; a failure is also counted by `class/error_kind`.
pub fn record_result(run: &mut Value, output: &Value) {
    let health = &mut run["run_health"];
    bump(&mut health["calls"]);
    let (class, kind) = result_status(output);
    if class == "success" {
        return;
    }
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

/// The self-review note for this run, when a threshold (0 = off) is crossed.
#[must_use]
pub fn note(health: &Value, limits: &SelfReview) -> Option<String> {
    let failed = health["failed"].as_u64().unwrap_or(0);
    let repeats = health["repeats"].as_u64().unwrap_or(0);
    let over = |count: u64, limit: u64| limit > 0 && count >= limit;
    if !over(failed, limits.min_failed_calls) && !over(repeats, limits.min_repeated_calls) {
        return None;
    }
    let mut kinds: Vec<(String, u64)> = health["failed_by_kind"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| {
                    (
                        k.rsplit('/').next().unwrap_or(k).to_owned(),
                        v.as_u64().unwrap_or(0),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    kinds.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let detail = kinds
        .iter()
        .take(4)
        .map(|(kind, n)| format!("{n} {kind}"))
        .collect::<Vec<_>>()
        .join(", ");
    let detail = if detail.is_empty() {
        String::new()
    } else {
        format!(" ({detail})")
    };
    Some(format!(
        "This run had {failed} failed calls{detail} and {repeats} repeated calls. \
         Consider a short self-review before your final reply (load skill self_review)."
    ))
}

/// Refresh the record and, past a threshold, return the note as a call-scoped contribution
/// so it never enters durable chat history.
pub fn contribution(
    run: &mut Value,
    limits: &SelfReview,
    turn: u64,
    call: u64,
) -> Option<Contribution> {
    note(refresh(run), limits).map(|content| Contribution {
        id: "run_health".into(),
        source: "native_run_health".into(),
        lifetime: Lifetime::Call,
        content,
        turn,
        call,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: SelfReview = SelfReview {
        min_failed_calls: 5,
        min_repeated_calls: 3,
    };

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
    fn record_counts_kinds_only() {
        let mut run = json!({});
        record_step(&mut run);
        record_result(&mut run, &json!("fine"));
        let failure =
            json!({"status":"error","error_kind":"browser_invalid_request","output":"secret text"});
        record_result(&mut run, &json!(failure.to_string()));
        record_result(
            &mut run,
            &json!({"status":"error","error_kind":"has spaces and text"}),
        );
        let health = refresh(&mut run).clone();
        assert_eq!(
            health,
            json!({"steps":1,"calls":3,"failed":2,"repeats":0,"failed_by_kind":{
                "error/browser_invalid_request":1,"error/unspecified":1}})
        );
        assert!(!health.to_string().contains("secret"));
    }

    #[test]
    fn kinds_are_bounded() {
        let mut run = json!({});
        for i in 0..40 {
            record_result(
                &mut run,
                &json!({"status":"error","error_kind":format!("k{i}")}),
            );
        }
        let kinds = run["run_health"]["failed_by_kind"].as_object().unwrap();
        assert_eq!(kinds.len(), MAX_KINDS + 1);
        assert_eq!(kinds["other"], 40 - MAX_KINDS as u64);
    }

    #[test]
    fn note_follows_thresholds() {
        let mut run = json!({});
        for _ in 0..4 {
            record_result(
                &mut run,
                &json!({"status":"uncertain","error_kind":"browser_uncertain"}),
            );
        }
        assert_eq!(note(refresh(&mut run), &LIMITS), None);
        for _ in 0..3 {
            record_result(
                &mut run,
                &json!({"status":"error","error_kind":"browser_invalid_request"}),
            );
        }
        assert_eq!(
            note(refresh(&mut run), &LIMITS).unwrap(),
            "This run had 7 failed calls (4 browser_uncertain, 3 browser_invalid_request) and 0 \
             repeated calls. Consider a short self-review before your final reply (load skill \
             self_review)."
        );
        let off = SelfReview {
            min_failed_calls: 0,
            min_repeated_calls: 0,
        };
        assert_eq!(note(refresh(&mut run), &off), None);
    }

    #[test]
    fn repeats_trigger_and_count_distinct_calls() {
        let decision = |id: &str, d: &str| json!({"call_id":id,"decision":d});
        let mut run = json!({"retry_guard":{"decisions":[
            decision("a","approval_required"), decision("a","approved"),
            decision("b","held"), decision("c","repeat")]}});
        assert_eq!(refresh(&mut run)["repeats"], 3);
        assert!(note(&run["run_health"], &LIMITS)
            .unwrap()
            .contains("3 repeated calls"));
    }

    #[test]
    fn note_is_call_scoped() {
        let mut run = json!({});
        assert!(contribution(&mut run, &LIMITS, 2, 3).is_none());
        for _ in 0..5 {
            record_result(&mut run, &json!({"status":"error"}));
        }
        let item = contribution(&mut run, &LIMITS, 2, 3).unwrap();
        assert_eq!(item.lifetime, Lifetime::Call);
        assert_eq!((item.turn, item.call), (2, 3));
        // The context selector drops it on any later call.
        let identity = |call| crate::context_input::CallIdentity { turn: 2, call };
        let items = [item];
        let next = crate::context_input::select(&items, identity(4)).unwrap();
        assert_eq!(next[0].disposition, "expired");
    }
}
