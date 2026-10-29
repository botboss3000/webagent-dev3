//! Administrator build dashboard requests and the broker replies they accept.
use serde::Deserialize;
use serde_json::{json, Value};

/// The fixed session the dashboard's broker requests run under.
pub const DASHBOARD_SESSION: &str = "00000000-0000-0000-0000-000000000000";

fn canonical_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok()
}

/// A build job ID in a dashboard path: a UUID.
#[must_use]
pub fn valid_job_id(value: &str) -> bool {
    canonical_uuid(value)
}

/// Cancel one build job, naming every job the caller saw it in.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelRequest {
    request_id: String,
    #[serde(default)]
    expected_job_ids: Vec<String>,
}

impl CancelRequest {
    /// Broker arguments to cancel `job_id`, or `None` when an identifier is invalid.
    #[must_use]
    pub fn arguments(&self, job_id: &str) -> Option<Value> {
        let valid = canonical_uuid(job_id)
            && (1..=8).contains(&self.expected_job_ids.len())
            && self.expected_job_ids.iter().all(|id| canonical_uuid(id))
            && !self.request_id.is_empty()
            && self.request_id.len() <= 80
            && self
                .request_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-');
        valid.then(|| {
            json!({"job_id": job_id, "request_id": self.request_id,
                "expected_job_ids": self.expected_job_ids})
        })
    }
}

/// One scheduler control: reservations, ownership, handoff, priority and the like.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulerRequest {
    action: String,
    task_id: Option<String>,
    task_key: Option<String>,
    summary: Option<String>,
    job_id: Option<String>,
    request_id: Option<String>,
    expected_revision: Option<u64>,
    expected_epoch: Option<u64>,
    phase_id: Option<String>,
    priority: Option<i8>,
    offset: Option<u64>,
    limit: Option<u64>,
    paths: Option<Vec<String>>,
    target_task_id: Option<String>,
    reason: Option<String>,
    commit: Option<String>,
    acceptance: Option<Vec<Value>>,
}

impl SchedulerRequest {
    /// Broker arguments with absent fields left out, or `None` when the request is out of
    /// bounds.
    #[must_use]
    pub fn arguments(&self) -> Option<Value> {
        let known = matches!(
            self.action.as_str(),
            "reservations"
                | "create"
                | "reserve"
                | "heartbeat"
                | "continue"
                | "acceptance"
                | "complete"
                | "handoff"
                | "takeover_preview"
                | "takeover"
                | "recover"
                | "resources"
                | "pause"
                | "resume"
                | "priority"
                | "reconcile"
        );
        let bounded = self.paths.as_ref().is_none_or(|paths| {
            !paths.is_empty() && paths.len() <= 128 && paths.iter().all(|path| path.len() <= 240)
        }) && self
            .reason
            .as_ref()
            .is_none_or(|reason| !reason.is_empty() && reason.len() <= 500)
            && self
                .summary
                .as_ref()
                .is_none_or(|summary| summary.len() <= 200)
            && self
                .acceptance
                .as_ref()
                .is_none_or(|items| !items.is_empty() && items.len() <= 32)
            && self
                .priority
                .is_none_or(|priority| (-2..=2).contains(&priority))
            && self.offset.is_none_or(|offset| offset <= 1000)
            && self.limit.is_none_or(|limit| limit > 0 && limit <= 50);
        if !known || !bounded {
            return None;
        }
        let mut args = json!({"action": self.action});
        for (key, value) in [
            ("task_id", json!(self.task_id)),
            ("task_key", json!(self.task_key)),
            ("summary", json!(self.summary)),
            ("job_id", json!(self.job_id)),
            ("request_id", json!(self.request_id)),
            ("expected_revision", json!(self.expected_revision)),
            ("expected_epoch", json!(self.expected_epoch)),
            ("phase_id", json!(self.phase_id)),
            ("offset", json!(self.offset)),
            ("limit", json!(self.limit)),
            ("priority", json!(self.priority)),
            ("paths", json!(self.paths)),
            ("target_task_id", json!(self.target_task_id)),
            ("reason", json!(self.reason)),
            ("commit", json!(self.commit)),
            ("acceptance", json!(self.acceptance)),
        ] {
            if !value.is_null() {
                args[key] = value;
            }
        }
        Some(args)
    }
}

/// Whether a successful broker reply to `name` has the shape the dashboard shows.
#[must_use]
pub fn reply_complete(name: &str, response: &Value) -> bool {
    response["status"] == "success"
        && (name != "v3_build_dashboard" || response["output"]["jobs"].is_array())
        && (!matches!(name, "v3_build_result" | "v3_build_cancel")
            || response["output"].get("job").is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cancel(value: Value) -> CancelRequest {
        serde_json::from_value(value).expect("cancel request")
    }

    #[test]
    fn cancellation_needs_canonical_identifiers() {
        let job = "11111111-1111-4111-8111-111111111111";
        let valid = cancel(json!({"request_id":"test-cancel","expected_job_ids":[job]}));
        assert_eq!(
            valid.arguments(job),
            Some(json!({"job_id":job,"request_id":"test-cancel","expected_job_ids":[job]}))
        );
        assert!(valid.arguments("invalid").is_none());
        assert!(cancel(json!({"request_id":"test-cancel"}))
            .arguments(job)
            .is_none());
        assert!(cancel(json!({"request_id":"a b","expected_job_ids":[job]}))
            .arguments(job)
            .is_none());
        let extra = json!({"request_id":"x","expected_job_ids":[job],"owner":"admin"});
        assert!(serde_json::from_value::<CancelRequest>(extra).is_err());
    }

    #[test]
    fn scheduler_requests_are_closed_and_bounded() {
        let request = |value: Value| -> SchedulerRequest {
            serde_json::from_value(value).expect("scheduler request")
        };
        assert_eq!(
            request(json!({"action":"reservations","offset":10})).arguments(),
            Some(json!({"action":"reservations","offset":10}))
        );
        for bad in [
            json!({"action":"delete"}),
            json!({"action":"priority","priority":3}),
            json!({"action":"reserve","paths":[]}),
            json!({"action":"reservations","limit":0}),
            json!({"action":"handoff","reason":""}),
        ] {
            assert!(request(bad.clone()).arguments().is_none(), "{bad}");
        }
        let smuggled = json!({"action":"reservations","untrusted_owner":"admin"});
        assert!(serde_json::from_value::<SchedulerRequest>(smuggled).is_err());
    }

    #[test]
    fn replies_carry_the_fields_the_dashboard_reads() {
        let ok = |output: Value| json!({"status":"success","output":output});
        assert!(reply_complete(
            "v3_build_dashboard",
            &ok(json!({"jobs":[]}))
        ));
        assert!(!reply_complete("v3_build_dashboard", &ok(json!({}))));
        assert!(reply_complete("v3_build_result", &ok(json!({"job":null}))));
        assert!(!reply_complete("v3_build_cancel", &ok(json!({}))));
        assert!(!reply_complete(
            "v3_build_leftover_review",
            &json!({"status":"error"})
        ));
        assert!(valid_job_id("11111111-1111-4111-8111-111111111111") && !valid_job_id("x"));
    }
}
