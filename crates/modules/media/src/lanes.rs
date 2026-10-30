//! Transient per-caller voice lanes. A lane holds one capture, the last completed transcript,
//! the pending voice proposal and a short quick-answer history; nothing here is durable.
use std::collections::HashMap;
use std::fmt;
use std::sync::OnceLock;

use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::capture::Capture;

/// Most lanes held at once across all callers; a new lane beyond this is refused.
pub const MAX_LANES: usize = 64;
/// Seconds a lane stays alive after its last use.
pub const LANE_TTL_SECS: u64 = 1800;
/// Quick-answer messages kept per lane (user and assistant turns together).
pub const HISTORY_LIMIT: usize = 12;

/// All live lanes, keyed by [`lane_key`].
pub type Lanes = HashMap<String, Lane>;

/// One caller's voice lane in one conversation.
#[derive(Default)]
pub struct Lane {
    /// Random identity of this lane instance; a removed and recreated lane gets a new one.
    pub generation: String,
    /// Unix time after which an idle lane may be dropped.
    pub expires: u64,
    /// The recording in progress, if any.
    pub capture: Option<Capture>,
    /// The last finished transcript, kept so a retried finish replays it.
    pub completed: Option<Completed>,
    /// The pending or ready voice proposal.
    pub proposal: Option<Value>,
    /// Recent quick-answer exchange, oldest first.
    pub history: Vec<Value>,
    /// True while a transcription, answer or action runs outside the registry lock.
    pub busy: bool,
}

impl Lane {
    /// Append one quick-answer exchange and keep only the newest [`HISTORY_LIMIT`] messages.
    pub fn remember_exchange(&mut self, utterance: &str, answer: &str) {
        self.history
            .push(json!({"role": "user", "content": utterance}));
        self.history
            .push(json!({"role": "assistant", "content": answer}));
        if self.history.len() > HISTORY_LIMIT {
            let excess = self.history.len() - HISTORY_LIMIT;
            self.history.drain(..excess);
        }
    }
}

/// A finished recording's transcript.
pub struct Completed {
    /// The capture the transcript belongs to.
    pub capture_id: String,
    /// The transcript text (empty when no speech was heard).
    pub text: String,
}

/// Why a lane could not be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneError {
    /// The registry is full and this caller has no lane yet.
    Busy,
    /// The lane was removed (or replaced) while work ran outside the lock.
    Gone,
}

impl LaneError {
    /// The public message for this rejection.
    pub fn message(self) -> &'static str {
        match self {
            Self::Busy => "Voice is busy; try again shortly",
            Self::Gone => "Session not found",
        }
    }
}

impl fmt::Display for LaneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for LaneError {}

static LANES: OnceLock<Mutex<Lanes>> = OnceLock::new();

/// The process-wide lane registry.
pub fn registry() -> &'static Mutex<Lanes> {
    LANES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Registry key for one user's lane in one conversation.
pub fn lane_key(user_id: &str, sid: &str) -> String {
    format!("{user_id}\0{sid}")
}

/// Drop expired idle lanes, then return (creating if needed) the lane for `key` and extend its
/// expiry. A new lane is refused with [`LaneError::Busy`] when [`MAX_LANES`] are live.
pub fn get_lane<'a>(all: &'a mut Lanes, key: &str, now: u64) -> Result<&'a mut Lane, LaneError> {
    all.retain(|_, lane| lane.expires > now || lane.busy);
    if !all.contains_key(key) && all.len() >= MAX_LANES {
        return Err(LaneError::Busy);
    }
    let lane = all.entry(key.to_owned()).or_insert_with(|| Lane {
        generation: uuid::Uuid::new_v4().to_string(),
        ..Lane::default()
    });
    lane.expires = now + LANE_TTL_SECS;
    Ok(lane)
}

/// The lane for `key` only if it is still the instance identified by `generation`, so a result
/// computed for a removed lane can never recreate it.
pub fn existing_lane<'a>(
    all: &'a mut Lanes,
    key: &str,
    generation: &str,
) -> Result<&'a mut Lane, LaneError> {
    all.get_mut(key)
        .filter(|lane| lane.generation == generation)
        .ok_or(LaneError::Gone)
}
