//! Outcome rules: what a prepared, finished, replayed or recovered action tells the model, how
//! a tool result maps to a durable state and which receipts may settle an uncertain effect.
use crate::intent::resource;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use webagent_configuration::digest;

/// Status projection of an action record shown to the model.
#[must_use]
pub fn summary(action: &Value) -> Value {
    json!({"id":action["id"],"tool":action["tool"],"state":action["state"],"attempt":action["attempt"],
        "revision":action["revision"],"requires_new_intent_approval":action["requires_new_intent_approval"],
        "separate_from":action["separate_from"],"retry_safe":action["retry_safe"]})
}

/// Tool result for an existing action: the saved result replayed when completed, otherwise a
/// status that forbids blind retries.
#[must_use]
pub fn response(action: &Value) -> Value {
    if action["state"] == "completed" {
        let mut result = action["result"].clone();
        if !result.is_object() {
            result = json!({"status":"success","output":result});
        }
        result["action"] = summary(action);
        result["receipt"] = action["receipt"].clone();
        result["replayed_action"] = json!(true);
        return result;
    }
    json!({"status":"action_status","action":summary(action),"output":
        if action["state"]=="uncertain" {"Outcome is uncertain. Reconcile the existing action; do not retry with a new ID."}
        else {"Existing action found. Reuse its ID for status/retry; a matching additional action requires fresh user intent and specific authorization."}})
}

/// Whether an action's effect may already have happened and must be reconciled first.
#[must_use]
pub fn unsettled(action: &Value) -> bool {
    matches!(action["state"].as_str(), Some("uncertain" | "running"))
}

/// Pending-call identity the action is keyed by; a retried call keeps the original.
#[must_use]
pub fn pending_id(run: &Value) -> String {
    run["pending"]["action_pending_id"]
        .as_str()
        .or_else(|| run["pending"]["id"].as_str())
        .unwrap_or("")
        .to_owned()
}

/// Apply a ledger `prepare` decision to the run's pending call. Returns the tool result when
/// the call must not execute; `None` means dispatch it and claim the bound action.
pub fn admit(run: &mut Value, prepared: &Value, args: &Value) -> Option<Value> {
    let action = &prepared["action"];
    match prepared["kind"].as_str() {
        Some("conflict") => Some(
            json!({"status":"denied","output":"Action ID conflicts with its saved intent/arguments","action":summary(action)}),
        ),
        Some("duplicate") => {
            let mut value = if action["arguments_digest"] != digest(&crate::intent::strip(args))
                && action["tool"] == "test_owner_purchase"
            {
                json!({"status":"new_intent_required","action":summary(action),"output":"Similar action found with different commercial details. Distinguish new intent and obtain specific authorization; no execution occurred."})
            } else {
                response(action)
            };
            value["duplicate_notice"] = json!(
                "Matching intent found. This is the prior action, not authorization for an additional action."
            );
            value["requested_action_id"] = args["_action"]["id"].clone();
            Some(value)
        }
        _ => {
            run["pending"]["action_id"] = action["id"].clone();
            run["pending"]["action_pending_id"] = action["pending_id"].clone();
            run["pending"]["new_intent_confirmation"] =
                action["requires_new_intent_approval"].clone();
            run["pending"]["separate_from"] = action["separate_from"].clone();
            if matches!(
                action["state"].as_str(),
                Some("running" | "completed" | "uncertain")
            ) {
                Some(response(action))
            } else {
                None
            }
        }
    }
}

/// Durable outcome of one executed attempt.
#[derive(Debug, PartialEq)]
pub struct Outcome {
    /// `completed`, `confirmed_failed` or `uncertain`.
    pub state: &'static str,
    /// Tool result saved with the record.
    pub value: Value,
    /// Whether a confirmed failure may be claimed again.
    pub retry_safe: bool,
}

/// Classify a tool result: only explicit success or denial without an uncertainty flag is
/// settled; errors and everything else are uncertain.
#[must_use]
pub fn outcome(result: &Result<Value>) -> Outcome {
    let (state, value, retry_safe) = match result {
        Ok(v) if v["status"] == "success" && v["uncertain"] != true => {
            ("completed", v.clone(), false)
        }
        Ok(v) if v["status"] == "denied" && v["uncertain"] != true => {
            ("confirmed_failed", v.clone(), true)
        }
        _ => (
            "uncertain",
            result.as_ref().ok().cloned().unwrap_or(Value::Null),
            false,
        ),
    };
    Outcome {
        state,
        value,
        retry_safe,
    }
}

/// Whether a completed native action takes its receipt from the agent transaction.
#[must_use]
pub fn uses_agent_receipt(state: &str, action: &Value) -> bool {
    state == "completed" && action["tool"] != "test_owner_purchase"
}

/// Tool result after `finish`: the saved record when this attempt lost the claim, an
/// uncertainty notice, or the attempt's result with its action status and receipt.
#[must_use]
pub fn finished(changed: bool, saved: &Value, outcome: Outcome) -> Value {
    if !changed {
        return response(saved);
    }
    if outcome.state == "uncertain" {
        return json!({"status":"uncertain","uncertain":true,"action":summary(saved),"output":"Action outcome requires reconciliation; no blind retry"});
    }
    let mut value = outcome.value;
    value["action"] = summary(saved);
    value["receipt"] = saved["receipt"].clone();
    value
}

/// The successful result a native receipt proves, if any. A `prompt_slot_write` receipt must
/// match the action's arguments; the receipt key is native-owned and bound to the original
/// pending identity, and its mutation and result commit together in the agent transaction.
pub fn confirmed_result<'r>(
    name: &str,
    action: &Value,
    receipt: &'r Value,
) -> Result<Option<&'r Value>> {
    if name == "prompt_slot_write" && receipt["arguments_digest"] != action["arguments_digest"] {
        bail!("Action reconciliation receipt mismatch");
    }
    let result = &receipt["result"];
    if result["status"] != "success" {
        return Ok(None);
    }
    Ok(Some(result))
}

/// One pending call that recovery must settle.
#[derive(Debug, PartialEq)]
pub struct RecoveryTarget {
    /// Batch position, or `None` for the run's single pending call.
    pub batch_index: Option<usize>,
    /// The pending call.
    pub pending: Value,
}

/// Pending calls of an interrupted run: executing or uncertain batch items, or the single call.
#[must_use]
pub fn recovery_targets(run: &Value) -> Vec<RecoveryTarget> {
    if run["batch"].is_array() {
        run["batch"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .filter(|(_, i)| matches!(i["state"].as_str(), Some("executing" | "uncertain")))
            .map(|(n, i)| RecoveryTarget {
                batch_index: Some(n),
                pending: i["pending"].clone(),
            })
            .collect()
    } else {
        vec![RecoveryTarget {
            batch_index: None,
            pending: run["pending"].clone(),
        }]
    }
}

/// Tool result recovered from a settled action, or `None` while it is unsettled.
#[must_use]
pub fn recovered_result(action: &Value) -> Option<Value> {
    if !matches!(
        action["state"].as_str(),
        Some("completed" | "confirmed_failed")
    ) {
        return None;
    }
    Some(if action["state"] == "completed" {
        response(action)
    } else {
        let mut result = action["result"].clone();
        result["action"] = summary(action);
        result
    })
}

/// After every write settled, return executing read calls of a batch to `requested`.
pub fn requeue_reads(run: &mut Value, is_read: impl Fn(&str) -> bool) {
    if let Some(items) = run["batch"].as_array_mut() {
        for item in items {
            if item["state"] == "executing"
                && is_read(item["pending"]["name"].as_str().unwrap_or(""))
            {
                item["state"] = json!("requested");
                item["pending"]["state"] = json!("requested");
            }
        }
    }
}

/// Action IDs bound to the run's pending call and batch items, in that order.
#[must_use]
pub fn pending_action_ids(run: &Value) -> Vec<&str> {
    let batch = run["batch"].as_array().into_iter().flatten();
    let pending = std::iter::once(&run["pending"]).chain(batch.map(|i| &i["pending"]));
    pending.filter_map(|p| p["action_id"].as_str()).collect()
}

/// Whether `action` belongs to `requester` and the resource of `agent`.
#[must_use]
pub fn owned_by(action: &Value, requester: &str, agent: &str) -> bool {
    action["requester"] == requester
        && action["resource"] == resource(agent, action["tool"].as_str().unwrap_or(""))
}

/// Whether `actor` may decide the run's pending approval as far as Actions is concerned: an
/// additional matching action can be confirmed only by the requesting user.
#[must_use]
pub fn may_confirm_new_intent(run: &Value, current_user: &str, actor: &str) -> bool {
    run["pending"]["new_intent_confirmation"] != true
        || run["requester_user_id"].as_str().unwrap_or(current_user) == actor
}
