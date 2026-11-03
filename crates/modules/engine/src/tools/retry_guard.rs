//! Repeat guard: within one run, a write that repeats an earlier identical call (same tool,
//! same arguments) is held as a possible duplicate until the agent marks it as an intended
//! repeat; a high-risk repeat also needs the user's approval. Rejections and denials had no
//! effect and are never recorded. Decisions are kept as content-free run evidence.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Reserved argument naming the earlier call an intended repeat repeats.
pub const REPEAT_ARG: &str = "_repeat_of";
const DEFAULT_HINT: &str = "check the current state with a read tool before repeating";
const DEFAULT_WARNING: &str =
    "The agent wants to repeat an action it already ran in this run. Approving may cause \
     the action to happen again.";
/// Most decisions kept per run; older ones are dropped first.
const MAX_DECISIONS: usize = 64;

/// How a repeat of a matching call is treated.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    /// Repeats run freely and are not tracked.
    None,
    /// A repeat runs once the agent names the earlier call with `_repeat_of`.
    Low,
    /// A named repeat also needs the user's approval, in every execution mode.
    #[default]
    High,
}

/// A tool's repeat policy and the texts shown to the agent and the user.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Retry {
    pub risk: Risk,
    /// What the agent should verify before repeating.
    pub hint: Option<String>,
    /// What the user is told when asked to approve a repeat.
    pub warning: Option<String>,
}

/// What the guard decided for one call.
#[derive(Debug, PartialEq)]
pub enum Admission {
    /// Dispatch normally.
    Run,
    /// Do not dispatch; this is the call's tool result.
    Held(Value),
    /// An intended high-risk repeat: wait for the user's approval.
    Approve,
}

/// Arguments without the runtime's reserved fields.
#[must_use]
pub fn strip(args: &Value) -> Value {
    let mut clean = args.clone();
    if let Some(object) = clean.as_object_mut() {
        object.remove(REPEAT_ARG);
        object.remove("_execution_mode");
    }
    clean
}

/// Identity of "the same call": tool name and arguments without reserved fields.
#[must_use]
pub fn key(name: &str, args: &Value) -> String {
    crate::digest(&json!([name, strip(args)]))
}

/// Decide whether `call_id` may run. `batch_seen` holds keys of earlier calls in the same
/// model turn that were admitted but have no recorded result yet.
pub fn admit(
    run: &mut Value,
    name: &str,
    args: &Value,
    call_id: &str,
    retry: &Retry,
    batch_seen: Option<&std::collections::HashMap<String, String>>,
) -> Admission {
    if retry.risk == Risk::None {
        return Admission::Run;
    }
    let key = key(name, args);
    let (prior, state) = match run["retry_guard"]["actions"].get(&key) {
        Some(entry) => (
            entry["call_id"].as_str().unwrap_or("").to_owned(),
            entry["state"].as_str().unwrap_or("uncertain").to_owned(),
        ),
        None => match batch_seen.and_then(|seen| seen.get(&key)) {
            Some(prior) => (prior.clone(), "running".to_owned()),
            None => return Admission::Run,
        },
    };
    let hint = retry.hint.as_deref().unwrap_or(DEFAULT_HINT);
    if args[REPEAT_ARG].as_str() != Some(prior.as_str()) || state == "running" {
        let what = match state.as_str() {
            "completed" => format!("already completed in this run as call {prior}"),
            "running" => format!("is already being run in this turn as call {prior}"),
            _ => format!(
                "ended in this run as call {prior} with an unknown outcome; it may already \
                 have taken effect"
            ),
        };
        let approval = if retry.risk == Risk::High {
            " The user will be asked to approve the repeat."
        } else {
            ""
        };
        let repeat = if state == "running" {
            "Wait for its result instead of repeating it.".to_owned()
        } else {
            format!(
                "If a repeat is really intended, call again with the same arguments plus \
                 \"{REPEAT_ARG}\":\"{prior}\".{approval}"
            )
        };
        decide(run, name, call_id, &prior, retry.risk, "held");
        return Admission::Held(json!({"status":"held","error_kind":"possible_duplicate",
            "repeat_of":prior,"output":format!(
                "Possible duplicate, not run: this exact {name} call {what}. Repeating it may \
                 duplicate its effect. Verify first: {hint}. {repeat}")}));
    }
    match retry.risk {
        Risk::Low => {
            decide(run, name, call_id, &prior, retry.risk, "repeat");
            Admission::Run
        }
        _ if run["pending"]["decision"] == "approve" => {
            decide(run, name, call_id, &prior, retry.risk, "approved");
            Admission::Run
        }
        _ => {
            decide(run, name, call_id, &prior, retry.risk, "approval_required");
            Admission::Approve
        }
    }
}

/// Mark `pending` as waiting for the user's approval of an intended repeat.
pub fn require_approval(pending: &mut Value, args: &Value, retry: &Retry) {
    // Reuses the Actions rule: never converted by Auto mode and checked again at dispatch.
    pending["new_intent_confirmation"] = json!(true);
    pending["repeat_of"] = args[REPEAT_ARG].clone();
    pending["repeat_warning"] = json!(retry.warning.as_deref().unwrap_or(DEFAULT_WARNING));
}

/// The tool result for a repeat the user declined or let expire.
#[must_use]
pub fn declined(pending: &Value) -> Option<Value> {
    if pending["repeat_of"].is_null() && pending["expired"] != true {
        return None;
    }
    let output = if pending["expired"] == true {
        "The approval request expired without a decision; treat it as declined. Do not \
         retry this action; continue without it or ask the user."
    } else {
        "The user declined repeating this action. Do not retry it; continue without it or \
         ask the user."
    };
    Some(json!({"status":"denied","error_kind":"repeat_declined","output":output}))
}

/// Record a dispatched call's outcome so a later identical call is recognised.
pub fn record(
    run: &mut Value,
    name: &str,
    args: &Value,
    call_id: &str,
    result: Option<&Value>,
    retry: &Retry,
) {
    if retry.risk == Risk::None {
        return;
    }
    let state = match result {
        // Nothing happened: a correction or retry is never a duplicate.
        Some(value)
            if matches!(
                value["status"].as_str(),
                Some("rejected" | "denied" | "held")
            ) =>
        {
            return;
        }
        Some(value)
            if value["uncertain"] != true
                && !matches!(
                    value["status"].as_str(),
                    Some(
                        "error" | "failed" | "timeout" | "cancelled" | "uncertain" | "interrupted"
                    )
                ) =>
        {
            "completed"
        }
        _ => "uncertain",
    };
    if !run["retry_guard"].is_object() {
        run["retry_guard"] = json!({"actions":{},"decisions":[]});
    }
    run["retry_guard"]["actions"][key(name, args)] =
        json!({"call_id":call_id,"state":state,"tool":name});
}

/// The agent-facing result for a write whose outcome is unknown.
#[must_use]
pub fn outcome_unknown(retry: &Retry) -> Value {
    let hint = retry.hint.as_deref().unwrap_or(DEFAULT_HINT);
    json!({"status":"error","error_kind":"outcome_unknown","uncertain":true,"output":format!(
        "The tool failed before confirming its result; it may or may not have taken effect. \
         Check before retrying: {hint}.")})
}

fn decide(run: &mut Value, tool: &str, call_id: &str, prior: &str, risk: Risk, decision: &str) {
    if !run["retry_guard"].is_object() {
        run["retry_guard"] = json!({"actions":{},"decisions":[]});
    }
    if let Some(list) = run["retry_guard"]["decisions"].as_array_mut() {
        if list.len() >= MAX_DECISIONS {
            list.remove(0);
        }
        list.push(
            json!({"tool":tool,"call_id":call_id,"repeat_of":prior,"risk":risk,
            "decision":decision,"recorded_at":crate::now()}),
        );
    }
}

/// Content-free projection of the guard's decisions for run diagnostics.
#[must_use]
pub fn diagnostics(run: &Value) -> Value {
    let ids = |value: &Value| {
        value
            .as_str()
            .filter(|s| {
                s.len() <= 128
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-:.".contains(&b))
            })
            .map_or(Value::Null, |s| json!(s))
    };
    let decisions: Vec<Value> = run["retry_guard"]["decisions"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter(|d| {
                    matches!(
                        d["decision"].as_str(),
                        Some("held" | "repeat" | "approval_required" | "approved")
                    ) && matches!(d["risk"].as_str(), Some("low" | "high"))
                })
                .map(|d| {
                    json!({"tool":ids(&d["tool"]),"call_id":ids(&d["call_id"]),
                        "repeat_of":ids(&d["repeat_of"]),"risk":d["risk"],"decision":d["decision"],
                        "recorded_at":d["recorded_at"].as_u64()})
                })
                .collect()
        })
        .unwrap_or_default();
    json!(decisions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn high() -> Retry {
        Retry::default()
    }

    #[test]
    fn first_call_runs_and_identical_completed_call_is_held() {
        let mut run = json!({});
        let args = json!({"branch":"main"});
        assert_eq!(
            admit(&mut run, "push", &args, "c1", &high(), None),
            Admission::Run
        );
        record(
            &mut run,
            "push",
            &args,
            "c1",
            Some(&json!({"status":"success"})),
            &high(),
        );
        let Admission::Held(result) = admit(&mut run, "push", &args, "c2", &high(), None) else {
            panic!("expected a held duplicate");
        };
        assert_eq!(result["status"], "held");
        assert_eq!(result["repeat_of"], "c1");
        let text = result["output"].as_str().unwrap();
        assert!(text.contains("already completed") && text.contains("_repeat_of"));
        assert!(text.contains("approve"));
        // Different arguments are a different action.
        assert_eq!(
            admit(
                &mut run,
                "push",
                &json!({"branch":"dev"}),
                "c3",
                &high(),
                None
            ),
            Admission::Run
        );
    }

    #[test]
    fn intended_repeat_needs_approval_for_high_and_runs_for_low() {
        let mut run = json!({});
        let args = json!({"to":"a"});
        record(
            &mut run,
            "send",
            &args,
            "c1",
            Some(&json!({"status":"success"})),
            &high(),
        );
        let repeat = json!({"to":"a","_repeat_of":"c1"});
        assert_eq!(
            admit(&mut run, "send", &repeat, "c2", &high(), None),
            Admission::Approve
        );
        let mut pending = json!({});
        require_approval(&mut pending, &repeat, &high());
        assert_eq!(pending["new_intent_confirmation"], true);
        assert_eq!(pending["repeat_of"], "c1");
        run["pending"]["decision"] = json!("approve");
        assert_eq!(
            admit(&mut run, "send", &repeat, "c2", &high(), None),
            Admission::Run
        );
        let low = Retry {
            risk: Risk::Low,
            ..Retry::default()
        };
        let mut run = json!({});
        record(
            &mut run,
            "send",
            &args,
            "c1",
            Some(&json!({"status":"success"})),
            &low,
        );
        assert_eq!(
            admit(&mut run, "send", &repeat, "c2", &low, None),
            Admission::Run
        );
        // A wrong call ID is not an intended repeat of this action.
        let wrong = json!({"to":"a","_repeat_of":"other"});
        assert!(matches!(
            admit(&mut run, "send", &wrong, "c3", &low, None),
            Admission::Held(_)
        ));
    }

    #[test]
    fn rejections_and_none_risk_are_never_recorded() {
        let mut run = json!({});
        let args = json!({"path":"/"});
        record(
            &mut run,
            "t",
            &args,
            "c1",
            Some(&json!({"status":"rejected"})),
            &high(),
        );
        record(
            &mut run,
            "t",
            &args,
            "c2",
            Some(&json!({"status":"denied"})),
            &high(),
        );
        assert_eq!(
            admit(&mut run, "t", &args, "c3", &high(), None),
            Admission::Run
        );
        let none = Retry {
            risk: Risk::None,
            ..Retry::default()
        };
        record(
            &mut run,
            "r",
            &args,
            "c1",
            Some(&json!({"status":"success"})),
            &none,
        );
        assert_eq!(
            admit(&mut run, "r", &args, "c2", &none, None),
            Admission::Run
        );
    }

    #[test]
    fn unknown_outcome_and_batch_duplicates_are_held() {
        let mut run = json!({});
        let args = json!({"x":1});
        record(&mut run, "w", &args, "c1", None, &high());
        let Admission::Held(result) = admit(&mut run, "w", &args, "c2", &high(), None) else {
            panic!("expected a held duplicate");
        };
        assert!(result["output"]
            .as_str()
            .unwrap()
            .contains("unknown outcome"));
        let mut seen = std::collections::HashMap::new();
        seen.insert(key("b", &args), "first".to_owned());
        let mut run = json!({});
        let repeat = json!({"x":1,"_repeat_of":"first"});
        // An in-flight call cannot be repeated, even when named.
        assert!(matches!(
            admit(&mut run, "b", &repeat, "second", &high(), Some(&seen)),
            Admission::Held(_)
        ));
        assert_eq!(diagnostics(&run)[0]["decision"], "held");
    }

    #[test]
    fn declined_and_expired_repeats_steer_the_agent_off() {
        assert!(declined(&json!({})).is_none());
        let declined_repeat = declined(&json!({"repeat_of":"c1"})).unwrap();
        assert!(declined_repeat["output"]
            .as_str()
            .unwrap()
            .contains("declined"));
        let expired = declined(&json!({"expired":true})).unwrap();
        assert!(expired["output"].as_str().unwrap().contains("expired"));
    }
}
