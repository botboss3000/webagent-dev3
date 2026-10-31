//! Pure staging rules: the supported event types, event identity, actor and payload, and
//! the notice a run checkpoint produces. No caller payload is ever copied into a notice.
use anyhow::{anyhow, Result};
use serde_json::{json, Value};

/// Every event type a module may stage.
pub const KINDS: [&str; 13] = [
    "session.created",
    "webhook.received",
    "main_notes.updated",
    "message.received",
    "run.started",
    "run.completed",
    "run.failed",
    "run.cancelled",
    "run.stopped",
    "run.waiting_approval",
    "session.help_requested",
    "session.help_cancelled",
    "session.help_resolved",
];

/// One notification a lifecycle owner stages.
#[derive(Clone, Copy, Debug)]
pub struct Notice<'a> {
    /// Deduplication key, unique for the originating resource lifetime.
    pub event_key: &'a str,
    /// Event type; one of [`KINDS`].
    pub kind: &'a str,
    /// Session the event belongs to.
    pub session_id: &'a str,
    /// Run the event belongs to, if any.
    pub run_id: Option<&'a str>,
    /// Status recorded in the payload, if any.
    pub status: Option<&'a str>,
}

/// The notice a persisted run produces, owned so it can outlive the run borrow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunNotice {
    /// Deduplication key (`run:<id>:<suffix>`).
    pub event_key: String,
    /// Event type.
    pub kind: &'static str,
    /// Session of the run.
    pub session_id: String,
    /// Run ID.
    pub run_id: String,
    /// Run status.
    pub status: String,
}

/// Whether `kind` is a supported event type.
#[must_use]
pub fn supported(kind: &str) -> bool {
    KINDS.contains(&kind)
}

/// Installation-wide event ID: the staging user plus the deduplication key.
#[must_use]
pub fn event_id(user_id: &str, event_key: &str) -> String {
    format!("{user_id}:{event_key}")
}

/// Run events are attributed to the runtime; everything else to the staging user.
#[must_use]
pub fn actor(kind: &str, user_id: &str) -> String {
    if kind.starts_with("run.") {
        "runtime".to_owned()
    } else {
        user_id.to_owned()
    }
}

/// The stored payload: a schema version and the status, nothing else.
#[must_use]
pub fn payload(status: Option<&str>) -> Value {
    json!({"schema_version":1,"status":status})
}

/// The notice a run checkpoint produces, or `None` for a status that emits nothing.
pub fn run_notice(run: &Value) -> Result<Option<RunNotice>> {
    let sid = run["session_id"]
        .as_str()
        .ok_or_else(|| anyhow!("Run has no session"))?;
    let rid = run["id"].as_str().ok_or_else(|| anyhow!("Run has no id"))?;
    let status = run["status"].as_str().unwrap_or("");
    let (kind, suffix) = match status {
        "completed" => ("run.completed", "terminal".to_owned()),
        "failed" => ("run.failed", "terminal".to_owned()),
        "cancelled" => ("run.cancelled", "terminal".to_owned()),
        "stopped" => ("run.stopped", "settled:stopped".to_owned()),
        "waiting_approval" => {
            let pending = run["pending"]["id"].clone();
            let batch: Vec<Value> = run["batch"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter(|row| row["state"] == "waiting")
                        .map(|row| row["pending"]["id"].clone())
                        .collect()
                })
                .unwrap_or_default();
            // Checkpoint IDs identify repeated saves of the same wait. No tool data is exported.
            (
                "run.waiting_approval",
                webagent_configuration::digest(&json!([pending, batch])),
            )
        }
        _ => return Ok(None),
    };
    Ok(Some(RunNotice {
        event_key: format!("run:{rid}:{suffix}"),
        kind,
        session_id: sid.to_owned(),
        run_id: rid.to_owned(),
        status: status.to_owned(),
    }))
}

#[cfg(test)]
mod tests {
    // The workspace has no clippy.toml with `allow-unwrap-in-tests` yet.
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn kinds_actor_identity_and_payload() {
        assert!(supported("run.waiting_approval"));
        assert!(supported("main_notes.updated"));
        assert!(!supported("checkpoint"));
        assert!(!supported(""));
        assert_eq!(event_id("admin", "created:s"), "admin:created:s");
        assert_eq!(actor("run.started", "admin"), "runtime");
        assert_eq!(actor("session.created", "admin"), "admin");
        assert_eq!(
            payload(Some("failed")).to_string(),
            r#"{"schema_version":1,"status":"failed"}"#
        );
        assert_eq!(payload(None), json!({"schema_version":1,"status":null}));
    }

    #[test]
    fn run_notice_maps_statuses_and_rejects_incomplete_runs() {
        let notice = |status: &str| {
            run_notice(&json!({"id":"r","session_id":"s","status":status,"error":"private"}))
                .unwrap()
        };
        let completed = notice("completed").unwrap();
        assert_eq!(completed.kind, "run.completed");
        assert_eq!(completed.event_key, "run:r:terminal");
        assert_eq!(completed.status, "completed");
        assert_eq!(notice("failed").unwrap().event_key, "run:r:terminal");
        assert_eq!(notice("cancelled").unwrap().kind, "run.cancelled");
        assert_eq!(
            notice("stopped").unwrap().event_key,
            "run:r:settled:stopped"
        );
        assert!(notice("running").is_none());
        assert!(notice("queued").is_none());
        let missing_session = run_notice(&json!({"id":"r","status":"completed"})).unwrap_err();
        assert_eq!(missing_session.to_string(), "Run has no session");
        let missing_id = run_notice(&json!({"session_id":"s","status":"running"})).unwrap_err();
        assert_eq!(missing_id.to_string(), "Run has no id");
    }

    #[test]
    fn approval_wait_key_follows_only_waiting_checkpoint_ids() {
        let wait = |pending: &str, batch: Value| {
            run_notice(
                &json!({"id":"r","session_id":"s","status":"waiting_approval",
                "pending":{"id":pending,"arguments":{"secret":"excluded"}},"batch":batch}),
            )
            .unwrap()
            .unwrap()
        };
        let first = wait("approval-1", json!([]));
        assert_eq!(first.kind, "run.waiting_approval");
        assert!(!first.event_key.contains("secret"));
        assert_eq!(first, wait("approval-1", json!([])));
        assert_ne!(first.event_key, wait("approval-2", json!([])).event_key);
        // Settled batch rows do not change the checkpoint identity; waiting ones do.
        let settled = json!([{"state":"approved","pending":{"id":"b"}}]);
        assert_eq!(first.event_key, wait("approval-1", settled).event_key);
        let waiting = json!([{"state":"waiting","pending":{"id":"b"}}]);
        assert_ne!(first.event_key, wait("approval-1", waiting).event_key);
    }
}
