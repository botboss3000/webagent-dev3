//! Revision vector in shadow (stream B4, guard 2 of `stream-b-design.md`).
//!
//! At the top of each run step the plain revisions of every record an access decision reads are
//! taken (the vector), then the step's `CallContext` is read. The dispatch fence and the tool
//! execution check (`foreground_authorized`) still recompute everything and **decide exactly as
//! before**; beside them this module reads the vector again and asks what the revision answer
//! would have been: an unchanged vector says the `CallContext` stands, so the recomputed access
//! inputs must equal the context's. Any difference is a disagreement: a writer that changed an
//! access record without moving its revision. Disagreements are counted per check kind, with a
//! bounded, content-free last example (ids, which part and which revisions; never message text,
//! tool arguments or grants), and persisted in the application database for the administrator
//! Diagnostics view. Nothing here changes a decision.
//!
//! Ordering keeps the comparison sound: the vector is read before the context's data and after
//! the check's data, so a write in between always shows as a moved vector, never as a false
//! disagreement.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
    Mutex,
};
use webagent_access::authority::LiveAccess;
use webagent_storage::revisions::{self, Principals, SessionHeader, SessionRevisions};

/// The plain revisions an access decision depends on. `None` = unreadable = changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevisionVector {
    /// Agent owner (directory) and both accounts' `auth_epoch`.
    pub principals: Option<Principals>,
    /// The agent database's epoch (agent row, memberships, ability connections, main chat).
    pub agent: Option<i64>,
    /// The session's mode and ability-state counters (its `record` counter is not compared:
    /// the record is rewritten for reasons that are not access; `header` stands for it).
    pub session: Option<SessionRevisions>,
    /// The session record's status, agent and participation, in plain values.
    pub header: Option<SessionHeader>,
    /// Whether the caller is a shared participant of the session (`shared_participants`).
    pub participant: Option<bool>,
    pub model_catalog: Option<i64>,
    /// The access policy file's digest (it keeps its digest, stream B part 1 assumption 4).
    pub policy: Option<String>,
}

fn differ<T: PartialEq>(before: &Option<T>, now: &Option<T>) -> bool {
    before.is_none() || now.is_none() || before != now
}

impl RevisionVector {
    #[allow(clippy::too_many_arguments)]
    pub async fn read(
        app: &sqlx::SqlitePool,
        user: &sqlx::SqlitePool,
        agent: &sqlx::SqlitePool,
        root: &std::path::Path,
        requester: &str,
        agent_id: &str,
        sid: &str,
        unseal: impl Fn(&[u8]) -> anyhow::Result<Value>,
    ) -> Self {
        Self {
            principals: revisions::principals(app, requester, agent_id).await,
            agent: revisions::agent(agent).await,
            session: revisions::session(user, sid).await,
            header: revisions::session_header(user, sid, unseal).await,
            participant: revisions::shared_participant(app, sid, requester).await,
            model_catalog: revisions::model_catalog(app).await,
            policy: webagent_access::policy_file::policy_digest(root).ok(),
        }
    }
    /// The parts that moved since `self`; a missing value always counts as moved.
    pub fn moved(&self, now: &Self) -> Vec<&'static str> {
        let mut moved = Vec::new();
        if differ(&self.principals, &now.principals) {
            moved.push("accounts");
        }
        if differ(&self.agent, &now.agent) {
            moved.push("agent");
        }
        let counters = |s: &Option<SessionRevisions>| s.map(|s| (s.mode, s.abilities));
        if differ(&counters(&self.session), &counters(&now.session))
            || differ(&self.header, &now.header)
            || differ(&self.participant, &now.participant)
        {
            moved.push("session");
        }
        if differ(&self.model_catalog, &now.model_catalog) {
            moved.push("model_catalog");
        }
        if differ(&self.policy, &now.policy) {
            moved.push("policy");
        }
        moved
    }
    /// Content-free numbers for the last-example record (the policy digest is cut short).
    fn summary(&self) -> Value {
        json!({
            "auth_epochs": self.principals.as_ref().map(|p| [p.caller_epoch, p.owner_epoch]),
            "agent_epoch": self.agent,
            "session": self.session.map(|s| [s.record, s.mode, s.abilities]),
            "header_status": self.header.as_ref().and_then(|h| h.status.clone()),
            "participant": self.participant,
            "model_catalog": self.model_catalog,
            "policy": self.policy.as_ref().map(|p| p.chars().take(12).collect::<String>()),
        })
    }
}

/// The access inputs the revision vector stands for, as one digest: the caller's live access
/// (with grants and revisions), the mode and the ability visibility and state.
pub fn access_part(
    access: &LiveAccess,
    mode_revision: u64,
    visibility: &Value,
    state: &Value,
    mode: &Value,
) -> String {
    crate::digest(&json!([
        access.context_fingerprint(),
        mode_revision,
        visibility,
        state,
        mode
    ]))
}

/// Taken at the top of a step: the vector, then the `CallContext`'s access inputs.
#[derive(Clone, Debug)]
pub struct Baseline {
    pub vector: RevisionVector,
    pub access: String,
    pub mode_revision: u64,
    pub run_id: String,
    pub generation: String,
    pub session_id: String,
    pub agent_id: String,
    pub requester: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The dispatch fence before a model call.
    Fence,
    /// `foreground_authorized`, right before a tool runs.
    ToolCheck,
}
impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Fence => "fence",
            Kind::ToolCheck => "tool_check",
        }
    }
    fn index(self) -> usize {
        match self {
            Kind::Fence => 0,
            Kind::ToolCheck => 1,
        }
    }
}

/// What the full recomputation found at the check (only the parts it got to compute).
#[derive(Clone, Debug, Default)]
pub struct Observed {
    pub model_authorized: Option<bool>,
    pub mode_revision: Option<u64>,
    pub access: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Unchanged vector and the recomputation agrees: the context stands.
    Unchanged,
    /// The vector moved: the revision answer is "rebuild", today's answer stands. `effect`
    /// tells whether the recomputed inputs actually differ.
    Changed {
        moved: Vec<&'static str>,
        effect: bool,
    },
    /// Unchanged vector but the recomputed inputs differ: a missed revision.
    Disagreement { parts: Vec<&'static str> },
}

/// The recomputed parts that differ from the baseline.
fn differing(baseline: &Baseline, observed: &Observed) -> Vec<&'static str> {
    let mut parts = Vec::new();
    // The step went on only with the model authorized, so the baseline answer is "true".
    if observed.model_authorized == Some(false) {
        parts.push("model");
    }
    if observed
        .mode_revision
        .is_some_and(|revision| revision != baseline.mode_revision)
    {
        parts.push("mode");
    }
    if observed
        .access
        .as_ref()
        .is_some_and(|access| *access != baseline.access)
    {
        parts.push("access");
    }
    parts
}

pub fn compare(baseline: &Baseline, now: &RevisionVector, observed: &Observed) -> Outcome {
    let moved = baseline.vector.moved(now);
    let parts = differing(baseline, observed);
    if !moved.is_empty() {
        return Outcome::Changed {
            moved,
            effect: !parts.is_empty(),
        };
    }
    if parts.is_empty() {
        Outcome::Unchanged
    } else {
        Outcome::Disagreement { parts }
    }
}

struct Counters {
    checks: AtomicU64,
    unchanged: AtomicU64,
    changed: AtomicU64,
    changed_without_effect: AtomicU64,
    disagreements: AtomicU64,
}
impl Counters {
    const fn new() -> Self {
        Self {
            checks: AtomicU64::new(0),
            unchanged: AtomicU64::new(0),
            changed: AtomicU64::new(0),
            changed_without_effect: AtomicU64::new(0),
            disagreements: AtomicU64::new(0),
        }
    }
    fn values(&self) -> [u64; 5] {
        [
            self.checks.load(Ordering::Relaxed),
            self.unchanged.load(Ordering::Relaxed),
            self.changed.load(Ordering::Relaxed),
            self.changed_without_effect.load(Ordering::Relaxed),
            self.disagreements.load(Ordering::Relaxed),
        ]
    }
}

/// Counts of this process, per kind; `FLUSHED` is what was already added to the database.
static COUNTS: [Counters; 2] = [Counters::new(), Counters::new()];
static FLUSHED: Mutex<[[u64; 5]; 2]> = Mutex::new([[0; 5]; 2]);
/// The newest disagreement per kind not yet written.
static PENDING_EXAMPLE: Mutex<[Option<Value>; 2]> = Mutex::new([None, None]);
static LAST_FLUSH: AtomicI64 = AtomicI64::new(0);
static TABLE_READY: AtomicBool = AtomicBool::new(false);
/// Seconds between writes of unchanged counts; a disagreement is written at its next check.
const FLUSH_SECONDS: i64 = 10;

/// Counts one check and keeps its example. Returns the outcome for tests and logs.
pub fn record(
    kind: Kind,
    baseline: &Baseline,
    now: &RevisionVector,
    observed: &Observed,
) -> Outcome {
    let outcome = compare(baseline, now, observed);
    let counts = &COUNTS[kind.index()];
    counts.checks.fetch_add(1, Ordering::Relaxed);
    match &outcome {
        Outcome::Unchanged => {
            counts.unchanged.fetch_add(1, Ordering::Relaxed);
        }
        Outcome::Changed { effect, .. } => {
            counts.changed.fetch_add(1, Ordering::Relaxed);
            if !effect {
                counts
                    .changed_without_effect
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        Outcome::Disagreement { parts } => {
            counts.disagreements.fetch_add(1, Ordering::Relaxed);
            let example = json!({
                "kind": kind.name(),
                "at": now_seconds(),
                "run_id": baseline.run_id,
                "session_id": baseline.session_id,
                "agent_id": baseline.agent_id,
                "requester_id": baseline.requester,
                "parts": parts,
                "vector": baseline.vector.summary(),
                "vector_now": now.summary(),
            });
            tracing::warn!(
                kind = kind.name(),
                run_id = %baseline.run_id,
                parts = ?parts,
                "Revision shadow disagreement"
            );
            if let Ok(mut pending) = PENDING_EXAMPLE.lock() {
                pending[kind.index()] = Some(example);
            }
        }
    }
    outcome
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Adds this process's new counts (and a pending example) to `revision_shadow` in the
/// application database: at most every `FLUSH_SECONDS`, at once when a disagreement waits.
/// Failures are logged and retried at the next check; they never affect a run.
pub async fn flush(app: &sqlx::SqlitePool) {
    let pending = PENDING_EXAMPLE
        .lock()
        .map(|p| p.iter().any(Option::is_some))
        .unwrap_or(false);
    let now = now_seconds();
    let last = LAST_FLUSH.load(Ordering::Relaxed);
    if !pending && now - last < FLUSH_SECONDS {
        return;
    }
    if LAST_FLUSH
        .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    if let Err(error) = write(app).await {
        tracing::warn!(%error, "Revision shadow counts not saved");
    }
}

async fn write(app: &sqlx::SqlitePool) -> anyhow::Result<()> {
    if !TABLE_READY.load(Ordering::Relaxed) {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS revision_shadow(kind TEXT PRIMARY KEY,
            checks INTEGER NOT NULL, unchanged INTEGER NOT NULL, changed INTEGER NOT NULL,
            changed_without_effect INTEGER NOT NULL, disagreements INTEGER NOT NULL,
            last_example TEXT, updated_at INTEGER NOT NULL)",
        )
        .execute(app)
        .await?;
        TABLE_READY.store(true, Ordering::Relaxed);
    }
    for kind in [Kind::Fence, Kind::ToolCheck] {
        let index = kind.index();
        let values = COUNTS[index].values();
        let delta = {
            let flushed = FLUSHED
                .lock()
                .map_err(|_| anyhow::anyhow!("shadow counts lock"))?;
            let mut delta = [0u64; 5];
            for (slot, value) in delta.iter_mut().enumerate() {
                *value = values[slot].saturating_sub(flushed[index][slot]);
            }
            delta
        };
        let example = PENDING_EXAMPLE
            .lock()
            .map_err(|_| anyhow::anyhow!("shadow example lock"))?[index]
            .take();
        if delta.iter().all(|value| *value == 0) && example.is_none() {
            continue;
        }
        let mut tx = app.begin().await?;
        sqlx::query(
            "INSERT INTO revision_shadow(kind,checks,unchanged,changed,changed_without_effect,
            disagreements,last_example,updated_at) VALUES(?,0,0,0,0,0,NULL,0)
            ON CONFLICT(kind) DO NOTHING",
        )
        .bind(kind.name())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE revision_shadow SET checks=checks+?,unchanged=unchanged+?,changed=changed+?,
            changed_without_effect=changed_without_effect+?,disagreements=disagreements+?,
            last_example=COALESCE(?,last_example),updated_at=? WHERE kind=?",
        )
        .bind(delta[0] as i64)
        .bind(delta[1] as i64)
        .bind(delta[2] as i64)
        .bind(delta[3] as i64)
        .bind(delta[4] as i64)
        .bind(example.as_ref().map(Value::to_string))
        .bind(now_seconds())
        .bind(kind.name())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        if let Ok(mut flushed) = FLUSHED.lock() {
            for (slot, value) in delta.iter().enumerate() {
                flushed[index][slot] += value;
            }
        }
    }
    Ok(())
}

/// The stored counts for the administrator view; an absent table reads as no checks yet.
pub async fn report(app: &sqlx::SqlitePool) -> Value {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT kind,checks,unchanged,changed,changed_without_effect,disagreements,last_example,
        updated_at FROM revision_shadow ORDER BY kind",
    )
    .fetch_all(app)
    .await
    .unwrap_or_default();
    let mut kinds = serde_json::Map::new();
    for name in [Kind::Fence.name(), Kind::ToolCheck.name()] {
        kinds.insert(
            name.into(),
            json!({"checks":0,"unchanged":0,"changed":0,"changed_without_effect":0,
                "disagreements":0,"last_example":null,"updated_at":null}),
        );
    }
    for row in rows {
        let kind: String = row.get("kind");
        let example = row
            .get::<Option<String>, _>("last_example")
            .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        kinds.insert(
            kind,
            json!({
                "checks": row.get::<i64,_>("checks"),
                "unchanged": row.get::<i64,_>("unchanged"),
                "changed": row.get::<i64,_>("changed"),
                "changed_without_effect": row.get::<i64,_>("changed_without_effect"),
                "disagreements": row.get::<i64,_>("disagreements"),
                "last_example": example,
                "updated_at": row.get::<i64,_>("updated_at"),
            }),
        );
    }
    json!({"mode":"shadow","decides":"full recomputation","kinds":kinds})
}

/// Baselines of the newest step per run, for the tool execution check that runs further down
/// the step (`foreground_authorized`). Bounded; an absent baseline simply skips the shadow.
static BASELINES: Mutex<Option<HashMap<String, Baseline>>> = Mutex::new(None);
const MAX_BASELINES: usize = 4096;

pub fn remember(baseline: Baseline) {
    if let Ok(mut map) = BASELINES.lock() {
        let map = map.get_or_insert_with(HashMap::new);
        if map.len() >= MAX_BASELINES && !map.contains_key(&baseline.run_id) {
            map.clear();
        }
        map.insert(baseline.run_id.clone(), baseline);
    }
}

/// The step baseline of `run_id` when it belongs to `generation`.
pub fn baseline(run_id: &str, generation: &str) -> Option<Baseline> {
    BASELINES
        .lock()
        .ok()?
        .as_ref()?
        .get(run_id)
        .filter(|b| b.generation == generation)
        .cloned()
}

#[cfg(test)]
#[path = "revision_shadow_tests.rs"]
mod tests;
