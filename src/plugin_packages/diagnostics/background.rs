//! Browser reports are untrusted operational observations, never runtime authority.
use crate::{
    bridge,
    recorder::{self, permits, scope, settings, Recorder},
    Error, SnapshotStore,
};
use serde_json::{json, Value};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueEvent {
    pub issue_id: String,
    pub event: String,
    pub operation: String,
    pub session_id: Option<String>,
    pub status: Option<u16>,
    pub repeat_count: u32,
    pub duration_ms: u64,
}
impl IssueEvent {
    pub fn validate(&self) -> Result<(), Error> {
        let uuid = |s: &str| uuid::Uuid::parse_str(s).is_ok();
        if !uuid(&self.issue_id)
            || self.session_id.as_deref().is_some_and(|s| !uuid(s))
            || !matches!(self.event.as_str(), "issue_opened" | "issue_resolved")
            || !matches!(
                self.operation.as_str(),
                "usage"
                    | "shared_snapshot"
                    | "history_refresh"
                    | "message_statistics"
                    | "read_ack"
                    | "manager_status"
                    | "session_controls"
                    | "shared_controls"
                    | "chat_sync"
                    | "status_poll"
                    | "sessions_list"
                    | "activity_display"
                    | "media_display"
                    | "transcript_position"
            )
            || self.status.is_some_and(|s| !(100..=599).contains(&s))
            || self.repeat_count == 0
            || self.repeat_count > 1_000_000
            || self.duration_ms > 31_536_000_000
        {
            return Err(Error::bad("Invalid background issue event"));
        }
        Ok(())
    }
}
/// Exposes presentation policy only. Capture remains restricted to the fixed admin account.
pub fn configuration<S: SnapshotStore>(recorder: Option<&Recorder<S>>, store: &S) -> Value {
    let s = settings(recorder);
    json!({
        "enabled": scope(store) && bridge::available() && s.as_ref().is_some_and(|s| permits(s,"app_background")),
        "show_toasts": s.as_ref().filter(|s| s.error.is_none()).map(|s|s.config.background_issues.show_toasts).unwrap_or(true),
        "cleared_toast_seconds": s.filter(|s| s.error.is_none()).map(|s|s.config.background_issues.cleared_toast_seconds).unwrap_or(3)
    })
}
/// Bounds and allowlists client observations; never accepts raw messages, URLs, headers or credentials.
pub async fn report<S: SnapshotStore>(
    recorder: Option<&Recorder<S>>,
    store: &S,
    body: IssueEvent,
) -> Result<Value, Error> {
    body.validate()?;
    let Some(s) =
        settings(recorder).filter(|s| bridge::available() && permits(s, "app_background"))
    else {
        return Ok(json!({"accepted":false}));
    };
    // A missing/deleted session can itself be the failure. Do not invent verified ownership.
    let verified_session = if let Some(sid) = body.session_id.as_deref() {
        store.session_exists(sid).await?
    } else {
        false
    };
    let event = json!({"owner":"admin","category":"app_background","level":if body.event=="issue_resolved"{"info"}else{"warning"},
        "source":"browser_observation","config_digest":s.digest,"issue_id":body.issue_id,"event":body.event,
        "operation":body.operation,"session_id":if verified_session {body.session_id.as_deref()}else{None},
        "client_session_id":body.session_id,"status":body.status,"repeat_count":body.repeat_count,"duration_ms":body.duration_ms});
    // Await the existing bounded recorder acknowledgement so retries cannot silently lose a queued event.
    bridge::send("event", event, None)
        .await
        .map_err(anyhow::Error::from)?;
    Ok(json!({"accepted":true}))
}
