//! Browser UI traces: bounded structural timelines, never typed text, URLs or credentials.
use crate::{
    bridge,
    recorder::{permits, scope, settings, Recorder},
    Error, SnapshotStore,
};
use serde_json::{json, Value};

const TRIGGERS: &[&str] = &["composer_focus_lost", "composer_keyboard_closed"];
const KINDS: &[&str] = &[
    "resize",
    "viewport_resize",
    "viewport_scroll",
    "focusin",
    "focusout",
    "blur_call",
    "focus_call",
    "pointerdown",
    "keydown",
    "input",
    "submit",
    "visibility",
    "keyboard_class",
    "attribute",
    "child_list",
];
const MAX_ENTRIES: usize = 300;
const MAX_TEXT: usize = 200;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trace {
    pub trace_id: String,
    pub trigger: String,
    pub session_id: Option<String>,
    pub width: u32,
    pub height: u32,
    pub entries: Vec<Entry>,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// Milliseconds before the report was assembled.
    pub before_ms: u32,
    pub kind: String,
    pub target: Option<String>,
    pub detail: Option<String>,
    pub active: Option<String>,
    pub inner_height: Option<u32>,
    pub viewport_height: Option<u32>,
    pub viewport_top: Option<u32>,
}

/// Element descriptors, attribute names and stack frames only; spaces separate tokens.
fn structural(text: &str) -> bool {
    text.len() <= MAX_TEXT
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " #._-:/()=>,[]".contains(c))
}

impl Trace {
    pub fn validate(&self) -> Result<(), Error> {
        let uuid = |s: &str| uuid::Uuid::parse_str(s).is_ok();
        let text = |value: &Option<String>| value.as_deref().is_none_or(structural);
        if !uuid(&self.trace_id)
            || self.session_id.as_deref().is_some_and(|s| !uuid(s))
            || !TRIGGERS.contains(&self.trigger.as_str())
            || self.width > 100_000
            || self.height > 100_000
            || self.entries.is_empty()
            || self.entries.len() > MAX_ENTRIES
            || self.entries.iter().any(|e| {
                e.before_ms > 60_000
                    || !KINDS.contains(&e.kind.as_str())
                    || !text(&e.target)
                    || !text(&e.detail)
                    || !text(&e.active)
                    || [e.inner_height, e.viewport_height, e.viewport_top]
                        .iter()
                        .flatten()
                        .any(|v| *v > 100_000)
            })
        {
            return Err(Error::bad("Invalid UI trace"));
        }
        Ok(())
    }
}

/// Tells the page whether to collect; capture remains restricted to the fixed admin account.
pub fn configuration<S: SnapshotStore>(recorder: Option<&Recorder<S>>, store: &S) -> Value {
    let enabled = scope(store)
        && bridge::available()
        && settings(recorder).is_some_and(|s| permits(&s, "ui_trace"));
    json!({ "enabled": enabled })
}

pub async fn report<S: SnapshotStore>(
    recorder: Option<&Recorder<S>>,
    store: &S,
    body: Trace,
) -> Result<Value, Error> {
    body.validate()?;
    let Some(s) = settings(recorder).filter(|s| bridge::available() && permits(s, "ui_trace"))
    else {
        return Ok(json!({"accepted":false}));
    };
    let verified_session = if let Some(sid) = body.session_id.as_deref() {
        store.session_exists(sid).await?
    } else {
        false
    };
    let event = json!({"owner":"admin","category":"ui_trace","level":"info","source":"browser_observation",
        "config_digest":s.digest,"trace_id":body.trace_id,"trigger":body.trigger,
        "session_id":if verified_session {body.session_id.as_deref()} else {None},
        "client_session_id":body.session_id,"width":body.width,"height":body.height,"entries":body.entries});
    bridge::send("event", event, None)
        .await
        .map_err(anyhow::Error::from)?;
    Ok(json!({"accepted":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn trace() -> Value {
        json!({"trace_id":uuid::Uuid::new_v4().to_string(),"trigger":"composer_focus_lost","session_id":null,
            "width":390,"height":844,"entries":[{"before_ms":12,"kind":"blur_call","target":"textarea#chat-input",
            "detail":"applyKb (boot/viewport.js:45:52)","active":"body","inner_height":500,"viewport_height":500,"viewport_top":0}]})
    }
    fn parse(value: Value) -> Result<(), Error> {
        serde_json::from_value::<Trace>(value)
            .map_err(|_| Error::bad("parse"))?
            .validate()
    }
    #[test]
    fn traces_are_bounded_structural_and_allowlisted() {
        assert!(parse(trace()).is_ok());
        let mut t = trace();
        t["trigger"] = json!("anything");
        assert!(parse(t).is_err());
        let mut t = trace();
        t["entries"][0]["kind"] = json!("typed_text");
        assert!(parse(t).is_err());
        // Queries, quotes and sentence punctuation fall outside the structural alphabet.
        for detail in [
            "it is typed text!",
            "https://example.test/a?b=c",
            "\"quoted\"",
        ] {
            let mut t = trace();
            t["entries"][0]["detail"] = json!(detail);
            assert!(parse(t).is_err(), "{detail}");
        }
        let mut t = trace();
        t["entries"][0]["detail"] = json!("x".repeat(MAX_TEXT + 1));
        assert!(parse(t).is_err());
        let mut t = trace();
        t["entries"][0]["text"] = json!("never accepted");
        assert!(parse(t).is_err());
        let mut t = trace();
        t["entries"] = json!(vec![trace()["entries"][0].clone(); MAX_ENTRIES + 1]);
        assert!(parse(t).is_err());
        let mut t = trace();
        t["session_id"] = json!("not-a-session");
        assert!(parse(t).is_err());
    }
}
