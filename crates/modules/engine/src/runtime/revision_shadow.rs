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
//! Beside the totals (release R1 of the access-check switch plan) each check is also counted
//! per vector part that moved (changed, and changed with effect) and per content-free caller
//! class (administrator, guest, owner, member, shared participant, other caller), so the
//! evidence shows which parts and callers were exercised. Five check kinds: the dispatch fence,
//! the tool execution check, the tool decision when the model proposes a tool (P1), the
//! admission and model grant before an approved tool is dispatched (P2) and the model grant
//! after the model call returns (P3). A check whose run moved to another model candidate since
//! the step top counts the subject part `candidate` as moved (rule S3: rebuild, never reuse).
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
use webagent_storage::revisions::{
    self, AgentRevision, Principals, SessionHeader, SessionRevisions,
};

/// The plain revisions an access decision depends on. `None` = unreadable = changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevisionVector {
    /// Agent owner (directory) and both accounts' (instance, `auth_epoch`).
    pub principals: Option<Principals>,
    /// The agent database's (instance, epoch) (agent row, memberships, ability connections,
    /// main chat).
    pub agent: Option<AgentRevision>,
    /// The session's mode and ability-state counters (its `record` counter is not compared:
    /// the record is rewritten for reasons that are not access; `header` stands for it).
    pub session: Option<SessionRevisions>,
    /// The session record's status, agent and participation, in plain values.
    pub header: Option<SessionHeader>,
    /// Whether the caller is a shared participant of the session (`shared_participants`).
    pub participant: Option<bool>,
    pub model_catalog: Option<i64>,
    /// The platform model roster and its credentials (`platform_catalog:v1`,
    /// `platform_credential:v1:*`).
    pub platform: Option<i64>,
    /// The access policy file's digest (it keeps its digest, stream B part 1 assumption 4).
    pub policy: Option<String>,
}

/// Every part `RevisionVector::moved` can name, plus the subject part `candidate`.
pub const PARTS: [&str; 10] = [
    "accounts",
    "agent",
    "session_mode",
    "session_abilities",
    "header",
    "participant",
    "model_catalog",
    "platform",
    "policy",
    "candidate",
];

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
            agent: revisions::agent_revision(agent).await,
            session: revisions::session(user, sid).await,
            header: revisions::session_header(user, sid, unseal).await,
            participant: revisions::shared_participant(app, sid, requester).await,
            model_catalog: revisions::model_catalog(app).await,
            platform: revisions::platform_catalog(app).await,
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
        // The session `record` counter is not compared: `header` stands for the record.
        if differ(&self.session.map(|s| s.mode), &now.session.map(|s| s.mode)) {
            moved.push("session_mode");
        }
        if differ(
            &self.session.map(|s| s.abilities),
            &now.session.map(|s| s.abilities),
        ) {
            moved.push("session_abilities");
        }
        if differ(&self.header, &now.header) {
            moved.push("header");
        }
        if differ(&self.participant, &now.participant) {
            moved.push("participant");
        }
        if differ(&self.model_catalog, &now.model_catalog) {
            moved.push("model_catalog");
        }
        if differ(&self.platform, &now.platform) {
            moved.push("platform");
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
            "account_instances": self.principals.as_ref().map(|p| [p.caller_instance, p.owner_instance]),
            "agent_epoch": self.agent.as_ref().map(|a| a.epoch),
            "agent_instance": self.agent.as_ref().map(|a| a.instance.chars().take(8).collect::<String>()),
            "session": self.session.map(|s| [s.record, s.mode, s.abilities]),
            "header_status": self.header.as_ref().and_then(|h| h.status.clone()),
            "participant": self.participant,
            "model_catalog": self.model_catalog,
            "platform": self.platform,
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
    /// Content-free caller class (`caller_class`), counted per check.
    pub caller: &'static str,
    /// The run's model candidate (`failover_state.candidate_index`) at the step top.
    pub candidate: Option<u64>,
}

/// The caller's class for the per-class counts: no ids, only the relationship.
pub fn caller_class(access: &LiveAccess, participant: Option<bool>) -> &'static str {
    use webagent_access::{Relationship, Role};
    match (&access.principal.role, &access.relationship) {
        (Role::Administrator, _) => "administrator",
        (Role::Guest, _) => "guest",
        (_, Relationship::Owner) => "owner",
        (_, Relationship::Member) => "member",
        _ if participant == Some(true) => "shared_participant",
        _ => "caller",
    }
}

/// Every class `caller_class` returns.
pub const CALLERS: [&str; 6] = [
    "administrator",
    "guest",
    "owner",
    "member",
    "shared_participant",
    "caller",
];

/// The run's current model candidate, as `Baseline::candidate` holds it.
pub fn candidate_of(run: &Value) -> Option<u64> {
    run["failover_state"]["candidate_index"].as_u64()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The dispatch fence before a model call.
    Fence,
    /// `foreground_authorized`, right before a tool runs.
    ToolCheck,
    /// The tool decision's inputs when the model proposes a tool (phase 2, P1).
    ToolDecision,
    /// Admission and model grant before an approved tool is dispatched (P2).
    ApprovedDispatch,
    /// The model grant after the model call returns (P3).
    ModelReturn,
}
pub const KINDS: [Kind; 5] = [
    Kind::Fence,
    Kind::ToolCheck,
    Kind::ToolDecision,
    Kind::ApprovedDispatch,
    Kind::ModelReturn,
];
impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Fence => "fence",
            Kind::ToolCheck => "tool_check",
            Kind::ToolDecision => "tool_decision",
            Kind::ApprovedDispatch => "approved_dispatch",
            Kind::ModelReturn => "model_return",
        }
    }
    fn index(self) -> usize {
        self as usize
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
    compare_at(baseline, now, observed, None)
}

/// `compare` for a check that also knows the run's current model `candidate` (`None`: not
/// compared). A different candidate is a different subject: it counts as moved, never as a
/// disagreement, because the step top authorized another candidate.
pub fn compare_at(
    baseline: &Baseline,
    now: &RevisionVector,
    observed: &Observed,
    candidate: Option<u64>,
) -> Outcome {
    let mut moved = baseline.vector.moved(now);
    if candidate.is_some() && candidate != baseline.candidate {
        moved.push("candidate");
    }
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
static COUNTS: [Counters; 5] = [
    Counters::new(),
    Counters::new(),
    Counters::new(),
    Counters::new(),
    Counters::new(),
];
static FLUSHED: Mutex<[[u64; 5]; 5]> = Mutex::new([[0; 5]; 5]);
/// The newest disagreement per kind not yet written.
static PENDING_EXAMPLE: Mutex<[Option<Value>; 5]> = Mutex::new([None, None, None, None, None]);
/// Per-part and per-caller counts not yet written, keyed by (kind, dimension, name):
/// `[checks, changed, changed_with_effect, disagreements]`. A part row counts only the checks
/// where that part moved (`checks` stays 0).
type Detail = HashMap<(usize, &'static str, &'static str), [u64; 4]>;
static DETAIL: Mutex<Option<Detail>> = Mutex::new(None);
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
    record_at(kind, baseline, now, observed, None)
}

/// `record` with the run's current model candidate (`compare_at`).
pub fn record_at(
    kind: Kind,
    baseline: &Baseline,
    now: &RevisionVector,
    observed: &Observed,
    candidate: Option<u64>,
) -> Outcome {
    let outcome = compare_at(baseline, now, observed, candidate);
    count_detail(kind, baseline.caller, &outcome);
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

fn count_detail(kind: Kind, caller: &'static str, outcome: &Outcome) {
    if let Ok(mut detail) = DETAIL.lock() {
        add_detail(
            detail.get_or_insert_with(HashMap::new),
            kind,
            caller,
            outcome,
        );
    }
}

fn add_detail(detail: &mut Detail, kind: Kind, caller: &'static str, outcome: &Outcome) {
    let index = kind.index();
    let row = detail.entry((index, "caller", caller)).or_default();
    row[0] += 1;
    match outcome {
        Outcome::Unchanged => {}
        Outcome::Changed { moved, effect } => {
            row[1] += 1;
            row[2] += u64::from(*effect);
            for part in moved {
                let row = detail.entry((index, "part", *part)).or_default();
                row[1] += 1;
                row[2] += u64::from(*effect);
            }
        }
        Outcome::Disagreement { .. } => row[3] += 1,
    }
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
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS revision_shadow_detail(kind TEXT NOT NULL,
            dimension TEXT NOT NULL, name TEXT NOT NULL, checks INTEGER NOT NULL,
            changed INTEGER NOT NULL, changed_with_effect INTEGER NOT NULL,
            disagreements INTEGER NOT NULL, PRIMARY KEY(kind,dimension,name))",
        )
        .execute(app)
        .await?;
        TABLE_READY.store(true, Ordering::Relaxed);
    }
    write_detail(app).await?;
    for kind in KINDS {
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

/// Adds the pending per-part and per-caller counts in one transaction; on failure they are
/// put back for the next flush.
async fn write_detail(app: &sqlx::SqlitePool) -> anyhow::Result<()> {
    let pending = DETAIL
        .lock()
        .map_err(|_| anyhow::anyhow!("shadow detail lock"))?
        .take()
        .unwrap_or_default();
    if pending.is_empty() {
        return Ok(());
    }
    let result = async {
        let mut tx = app.begin().await?;
        for ((index, dimension, name), values) in &pending {
            sqlx::query(
                "INSERT INTO revision_shadow_detail(kind,dimension,name,checks,changed,
                changed_with_effect,disagreements) VALUES(?,?,?,?,?,?,?)
                ON CONFLICT(kind,dimension,name) DO UPDATE SET checks=checks+excluded.checks,
                changed=changed+excluded.changed,
                changed_with_effect=changed_with_effect+excluded.changed_with_effect,
                disagreements=disagreements+excluded.disagreements",
            )
            .bind(KINDS[*index].name())
            .bind(*dimension)
            .bind(*name)
            .bind(values[0] as i64)
            .bind(values[1] as i64)
            .bind(values[2] as i64)
            .bind(values[3] as i64)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        anyhow::Ok(())
    }
    .await;
    if result.is_err() {
        if let Ok(mut detail) = DETAIL.lock() {
            let detail = detail.get_or_insert_with(HashMap::new);
            for (key, values) in pending {
                let row = detail.entry(key).or_default();
                for (slot, value) in values.iter().enumerate() {
                    row[slot] += value;
                }
            }
        }
    }
    result
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
    for kind in KINDS {
        kinds.insert(
            kind.name().into(),
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
    let detail = sqlx::query(
        "SELECT kind,dimension,name,checks,changed,changed_with_effect,disagreements
        FROM revision_shadow_detail ORDER BY kind,dimension,name",
    )
    .fetch_all(app)
    .await
    .unwrap_or_default();
    for kind in kinds.values_mut() {
        kind["parts"] = json!(PARTS
            .iter()
            .map(|part| (
                part.to_string(),
                json!({"changed":0,"changed_with_effect":0})
            ))
            .collect::<serde_json::Map<_, _>>());
        kind["callers"] = json!(CALLERS
            .iter()
            .map(|caller| (
                caller.to_string(),
                json!({"checks":0,"changed":0,"changed_with_effect":0,"disagreements":0})
            ))
            .collect::<serde_json::Map<_, _>>());
    }
    for row in detail {
        let kind: String = row.get("kind");
        let dimension: String = row.get("dimension");
        let name: String = row.get("name");
        let Some(entry) = kinds.get_mut(&kind) else {
            continue;
        };
        let value = if dimension == "part" {
            json!({"changed": row.get::<i64,_>("changed"),
                "changed_with_effect": row.get::<i64,_>("changed_with_effect")})
        } else {
            json!({"checks": row.get::<i64,_>("checks"), "changed": row.get::<i64,_>("changed"),
                "changed_with_effect": row.get::<i64,_>("changed_with_effect"),
                "disagreements": row.get::<i64,_>("disagreements")})
        };
        let group = if dimension == "part" {
            "parts"
        } else {
            "callers"
        };
        entry[group][name] = value;
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
