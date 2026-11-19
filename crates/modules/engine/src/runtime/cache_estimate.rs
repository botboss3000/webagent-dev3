//! Estimate how much of a model request repeats the start of the session's previous one.
//! Providers cache a request's leading bytes, so this shows what could have been cached;
//! compared with the provider's reported cached tokens it tells a request that changed
//! early apart from a provider that dropped its cache.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Sessions whose last request fingerprints are kept in memory; older ones are dropped.
const MAX_SESSIONS: usize = 256;

/// One request part: a short content digest and its serialized size in bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Part {
    digest: [u8; 8],
    bytes: usize,
}

/// Fingerprints of one request in the order providers read it.
#[derive(Debug, Clone, Default)]
pub struct Fingerprint {
    tools: Option<Part>,
    instructions: Option<Part>,
    history: Vec<Part>,
}

/// Digest and size of one serialized part.
fn part(bytes: &[u8]) -> Part {
    let hash = Sha256::digest(bytes);
    let mut digest = [0u8; 8];
    digest.copy_from_slice(&hash[..8]);
    Part {
        digest,
        bytes: bytes.len(),
    }
}

/// Serialized JSON bytes of a value; serde_json cannot fail on a `Value`.
fn encoded(value: &impl serde::Serialize) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

impl Fingerprint {
    /// Fingerprint a request: tool definitions, then instructions, then each history entry.
    pub fn of(schemas: &[Value], instructions: &str, input: &[Value]) -> Self {
        Self {
            tools: Some(part(&encoded(&schemas))),
            instructions: Some(part(&encoded(&instructions))),
            history: input.iter().map(|item| part(&encoded(item))).collect(),
        }
    }

    fn sent_bytes(&self) -> usize {
        self.tools.as_ref().map_or(0, |p| p.bytes)
            + self.instructions.as_ref().map_or(0, |p| p.bytes)
            + self.history.iter().map(|p| p.bytes).sum::<usize>()
    }

    /// Compare with the previous request: bytes of the unchanged leading parts, bytes sent,
    /// and the first part that differs (`appended` when every earlier part is unchanged).
    pub fn estimate(&self, previous: Option<&Fingerprint>) -> Value {
        let sent = self.sent_bytes();
        let Some(previous) = previous else {
            return json!({"matched_bytes":0,"sent_bytes":sent,"first_change":"no_previous_call",
                "first_change_entry":null});
        };
        let mut matched = 0;
        for (label, now, before) in [
            ("tools", &self.tools, &previous.tools),
            ("instructions", &self.instructions, &previous.instructions),
        ] {
            match (now, before) {
                (Some(now), Some(before)) if now == before => matched += now.bytes,
                _ => {
                    return json!({"matched_bytes":matched,"sent_bytes":sent,
                        "first_change":label,"first_change_entry":null})
                }
            }
        }
        for (index, before) in previous.history.iter().enumerate() {
            if self.history.get(index) != Some(before) {
                return json!({"matched_bytes":matched,"sent_bytes":sent,"first_change":"history",
                    "first_change_entry":index});
            }
            matched += before.bytes;
        }
        json!({"matched_bytes":matched,"sent_bytes":sent,"first_change":"appended",
            "first_change_entry":null})
    }
}

/// One session's last dispatched request and when it was sent.
struct LastCall {
    fingerprint: Fingerprint,
    at: Instant,
}

/// The last dispatched request per session, oldest session first.
fn last_requests() -> &'static Mutex<(HashMap<String, LastCall>, VecDeque<String>)> {
    static LAST: OnceLock<Mutex<(HashMap<String, LastCall>, VecDeque<String>)>> = OnceLock::new();
    LAST.get_or_init(Default::default)
}

/// Why this session's next call should reorder its context first: the provider cache has
/// expired (`idle`, longer than `timeout` since the last call) or the tool definitions
/// changed since it (`tools`). None before the first call this process has seen.
pub fn reorder_reason(session: &str, schemas: &[Value], timeout: Duration) -> Option<&'static str> {
    let guard = last_requests()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let last = guard.0.get(session)?;
    if last.at.elapsed() > timeout {
        Some("idle")
    } else if last.fingerprint.tools != Some(part(&encoded(&schemas))) {
        Some("tools")
    } else {
        None
    }
}

/// Whether this process has seen a model call for the session; a restart forgets them.
pub fn known(session: &str) -> bool {
    last_requests()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .0
        .contains_key(session)
}

/// After a restart: a reorder is due when the session's last stored activity (Unix
/// seconds) is older than the cache timeout.
pub fn stored_idle(last_at: Option<i64>, now: u64, timeout: Duration) -> Option<&'static str> {
    let last_at = u64::try_from(last_at?).ok()?;
    (now.saturating_sub(last_at) > timeout.as_secs()).then_some("idle")
}

/// Forget a session's last request after a reorder, so the next call starts a fresh
/// comparison instead of reordering again.
pub fn forget(session: &str) {
    let mut guard = last_requests()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.0.remove(session);
    guard.1.retain(|known| known != session);
}

/// Estimate this session's request against its previous one and remember it for the next.
pub fn observe(session: &str, current: Fingerprint) -> Value {
    let mut guard = last_requests()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (requests, order) = &mut *guard;
    let estimate = current.estimate(requests.get(session).map(|last| &last.fingerprint));
    let call = LastCall {
        fingerprint: current,
        at: Instant::now(),
    };
    if requests.insert(session.to_owned(), call).is_none() {
        order.push_back(session.to_owned());
        while order.len() > MAX_SESSIONS {
            if let Some(oldest) = order.pop_front() {
                requests.remove(&oldest);
            }
        }
    }
    estimate
}

/// Log one line per model call with the provider's cache figures beside the estimate.
pub fn log_call(session: &str, run_id: &str, run: &Value) {
    let (latest, estimate) = (&run["usage"]["latest"], &run["usage"]["cache_estimate"]);
    tracing::info!(session_id=%session, run_id=%run_id,
        input_tokens=latest["input_tokens"].as_u64(), cached_tokens=latest["cached_input_tokens"].as_u64(),
        cache_write_tokens=latest["cache_write_tokens"].as_u64(),
        matched_bytes=estimate["matched_bytes"].as_u64(), sent_bytes=estimate["sent_bytes"].as_u64(),
        first_change=estimate["first_change"].as_str().unwrap_or(""),
        first_change_entry=estimate["first_change_entry"].as_u64(),
        compactions=run["compactions"].as_u64().unwrap_or(0), "model call cache");
}

/// Log a major reorder in this module, whose info lines the app shows (engine logs are
/// otherwise filtered to warnings).
pub fn log_reorder(session: &str, run_id: &str, reason: &str) {
    tracing::info!(session_id=%session, run_id=%run_id, reason, "context reordered");
}

/// The activity row for one model call: the provider's input and cache figures beside the
/// estimate, shown as a "Model call" row in the chat's activity.
pub fn activity_item(run: &Value) -> Value {
    let (latest, estimate) = (&run["usage"]["latest"], &run["usage"]["cache_estimate"]);
    json!({"type":"model_usage","input_tokens":latest["input_tokens"],
        "cached_input_tokens":latest["cached_input_tokens"],
        "cache_write_tokens":latest["cache_write_tokens"],
        "matched_bytes":estimate["matched_bytes"],"sent_bytes":estimate["sent_bytes"],
        "first_change":estimate["first_change"],"first_change_entry":estimate["first_change_entry"]})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(instructions: &str, history: &[&str]) -> Fingerprint {
        let input: Vec<Value> = history
            .iter()
            .map(|text| json!({"role":"user","content":text}))
            .collect();
        Fingerprint::of(&[json!({"name":"file_read"})], instructions, &input)
    }

    #[test]
    fn appended_history_matches_the_whole_previous_request() {
        let before = request("prompt", &["a", "b"]);
        let now = request("prompt", &["a", "b", "c"]);
        let estimate = now.estimate(Some(&before));
        assert_eq!(estimate["first_change"], "appended");
        assert_eq!(estimate["matched_bytes"], before.sent_bytes());
        assert_eq!(estimate["sent_bytes"], now.sent_bytes());
    }

    #[test]
    fn first_change_names_the_part_that_differs() {
        let before = request("prompt", &["a", "b"]);
        let changed_history = request("prompt", &["a", "summary"]).estimate(Some(&before));
        assert_eq!(changed_history["first_change"], "history");
        assert_eq!(changed_history["first_change_entry"], 1);
        let changed_prompt = request("new prompt", &["a", "b"]).estimate(Some(&before));
        assert_eq!(changed_prompt["first_change"], "instructions");
        assert!(changed_prompt["matched_bytes"].as_u64().unwrap() > 0);
        assert_eq!(
            request("prompt", &[]).estimate(None)["first_change"],
            "no_previous_call"
        );
    }

    #[test]
    fn a_reorder_is_due_after_the_timeout_or_a_tool_change() {
        let session = format!("cache-reorder-test-{}", uuid::Uuid::new_v4());
        let tools = [json!({"name":"file_read"})];
        assert_eq!(reorder_reason(&session, &tools, Duration::ZERO), None);
        observe(&session, request("p", &["a"]));
        let long = Duration::from_secs(3600);
        assert_eq!(reorder_reason(&session, &tools, long), None);
        assert_eq!(reorder_reason(&session, &[], long), Some("tools"));
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(
            reorder_reason(&session, &tools, Duration::from_millis(1)),
            Some("idle")
        );
        forget(&session);
        assert_eq!(reorder_reason(&session, &tools, Duration::ZERO), None);
    }

    #[test]
    fn after_a_restart_stored_activity_decides_idle() {
        let hour = Duration::from_secs(3600);
        assert_eq!(stored_idle(Some(1_000), 1_000 + 3601, hour), Some("idle"));
        assert_eq!(stored_idle(Some(1_000), 1_000 + 3600, hour), None);
        assert_eq!(stored_idle(None, 5_000, hour), None);
        assert_eq!(stored_idle(Some(-1), 5_000, hour), None);
        assert!(!known(&format!("cache-unknown-{}", uuid::Uuid::new_v4())));
    }

    #[test]
    fn observe_remembers_each_session_separately() {
        let session = format!("cache-estimate-test-{}", uuid::Uuid::new_v4());
        assert_eq!(
            observe(&session, request("p", &["a"]))["first_change"],
            "no_previous_call"
        );
        assert_eq!(
            observe(&session, request("p", &["a", "b"]))["first_change"],
            "appended"
        );
    }
}
