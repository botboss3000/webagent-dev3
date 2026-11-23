//! Daily usage ledger in the shared `app.db`: per requester and per agent, per UTC day, the
//! model turns, tokens and known provider cost (numbers and ids only, no content). Daily turn
//! limits (account levels' `daily_turns`, the policy's `run_limits.agent_daily_turns`) are
//! checked against it at message admission and before each model turn; cost is recorded and
//! shown, never limited (plus tier spec 8.1). Rows older than [`RETENTION_DAYS`] are deleted by
//! the first write of each day.
//!
//! Beside it, the usage feed (plus tier spec 8.6): `usage_turns`, one append-only row per run
//! flush with the tokens, compute time and cost the run added since its last flush, named by
//! agent owner, agent, requester and model; and `usage_owner`, the agent owner's day and month
//! totals. A run flushes during a model call at most once a minute (inside that call's ledger
//! transaction) and once when it finishes, stops, pauses or parks; nothing else is written per
//! model or tool call. `usage_marks` holds what each run has flushed, so a repeated or
//! recovered flush adds only the difference.
use anyhow::Result;
use serde_json::Value;
use sqlx::{Sqlite, SqlitePool, Transaction};
use std::sync::atomic::{AtomicI64, Ordering};
use webagent_access::authority::LiveAccess;
use webagent_access::run_limits::daily_refusal;

/// Usage of one requester (the account that sent the run's message).
pub const REQUESTER: &str = "requester";
/// Usage of one agent by non-administrators.
pub const AGENT: &str = "agent";
/// Command processor time of one run (its `cpu_ms`, on the days its commands ran).
pub const RUN: &str = "run";
/// Days of rows kept (today included).
pub const RETENTION_DAYS: i64 = 35;

/// One model call's numbers.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Turn {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Known provider cost in millionths of a US dollar (0 when not reported).
    pub cost_micros: u64,
}

impl Turn {
    /// The call `webagent_usage::record` just stored as `run.usage.latest` (null: a turn only).
    #[must_use]
    pub fn latest(run: &serde_json::Value) -> Self {
        let latest = &run["usage"]["latest"];
        Self {
            input_tokens: latest["input_tokens"].as_u64().unwrap_or(0),
            output_tokens: latest["output_tokens"].as_u64().unwrap_or(0),
            cost_micros: latest["cost_usd"]
                .as_f64()
                .filter(|cost| cost.is_finite() && *cost >= 0.0)
                .map_or(0, |cost| (cost * 1_000_000.0).round() as u64),
        }
    }
}

/// The UTC day number of a Unix time.
#[must_use]
pub fn day(unix_seconds: u64) -> i64 {
    i64::try_from(unix_seconds / 86_400).unwrap_or(i64::MAX)
}

fn today() -> i64 {
    day(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs()))
}

/// Create the tables; idempotent.
pub async fn schema(pool: &SqlitePool) -> Result<()> {
    sqlx::query("CREATE TABLE IF NOT EXISTS usage_turns(seq INTEGER PRIMARY KEY AUTOINCREMENT,owner_id TEXT NOT NULL,agent_id TEXT NOT NULL,requester_id TEXT NOT NULL,model TEXT NOT NULL,catalog INTEGER NOT NULL,input_tokens INTEGER NOT NULL,cached_input_tokens INTEGER NOT NULL,output_tokens INTEGER NOT NULL,compute_ms INTEGER NOT NULL,cost_micros INTEGER,finished_at INTEGER NOT NULL)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS usage_turns_finished ON usage_turns(finished_at)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS usage_owner(owner_id TEXT NOT NULL,period TEXT NOT NULL,number INTEGER NOT NULL,input_tokens INTEGER NOT NULL DEFAULT 0,cached_input_tokens INTEGER NOT NULL DEFAULT 0,output_tokens INTEGER NOT NULL DEFAULT 0,compute_ms INTEGER NOT NULL DEFAULT 0,cost_micros INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(owner_id,period,number)) WITHOUT ROWID")
        .execute(pool)
        .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS usage_marks(run_id TEXT PRIMARY KEY,owner_id TEXT NOT NULL,agent_id TEXT NOT NULL,requester_id TEXT NOT NULL,model TEXT NOT NULL,catalog INTEGER NOT NULL,input_tokens INTEGER NOT NULL,cached_input_tokens INTEGER NOT NULL,output_tokens INTEGER NOT NULL,compute_ms INTEGER NOT NULL,cost_micros INTEGER NOT NULL,flushed_at INTEGER NOT NULL) WITHOUT ROWID")
        .execute(pool)
        .await?;
    // `usage_cursor`: the highest `usage_turns.seq` the billing service has acknowledged.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS billing_state(key TEXT PRIMARY KEY,value INTEGER NOT NULL)",
    )
    .execute(pool)
    .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS usage_daily(scope TEXT NOT NULL,id TEXT NOT NULL,day INTEGER NOT NULL,turns INTEGER NOT NULL DEFAULT 0,input_tokens INTEGER NOT NULL DEFAULT 0,output_tokens INTEGER NOT NULL DEFAULT 0,cost_micros INTEGER NOT NULL DEFAULT 0,cpu_ms INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(scope,id,day))")
        .execute(pool)
        .await?;
    // Tables made before command processor time was counted.
    let has: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pragma_table_info('usage_daily') WHERE name='cpu_ms'",
    )
    .fetch_one(pool)
    .await?;
    if has == 0 {
        sqlx::query("ALTER TABLE usage_daily ADD COLUMN cpu_ms INTEGER NOT NULL DEFAULT 0")
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// The last day this process pruned old rows.
static PRUNED: AtomicI64 = AtomicI64::new(i64::MIN);

/// Add one model call on `day` to the requester and, for a non-administrator, to the agent.
pub async fn record_on(
    pool: &SqlitePool,
    requester: &str,
    agent: Option<&str>,
    day: i64,
    turn: Turn,
) -> Result<()> {
    record_with(pool, requester, agent, day, turn, None).await
}

/// [`record_on`], also flushing a run's usage feed in the same transaction when one is given.
async fn record_with(
    pool: &SqlitePool,
    requester: &str,
    agent: Option<&str>,
    day: i64,
    turn: Turn,
    flush: Option<(&str, &RunParty, RunTotals)>,
) -> Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    for (scope, id) in [(REQUESTER, Some(requester)), (AGENT, agent)] {
        let Some(id) = id else { continue };
        sqlx::query("INSERT INTO usage_daily(scope,id,day,turns,input_tokens,output_tokens,cost_micros) VALUES(?,?,?,1,?,?,?) ON CONFLICT(scope,id,day) DO UPDATE SET turns=turns+1,input_tokens=input_tokens+excluded.input_tokens,output_tokens=output_tokens+excluded.output_tokens,cost_micros=cost_micros+excluded.cost_micros")
            .bind(scope)
            .bind(id)
            .bind(day)
            .bind(i64::try_from(turn.input_tokens).unwrap_or(i64::MAX))
            .bind(i64::try_from(turn.output_tokens).unwrap_or(i64::MAX))
            .bind(i64::try_from(turn.cost_micros).unwrap_or(i64::MAX))
            .execute(&mut *tx)
            .await?;
    }
    if PRUNED.swap(day, Ordering::Relaxed) != day {
        sqlx::query("DELETE FROM usage_daily WHERE day<?")
            .bind(day - RETENTION_DAYS + 1)
            .execute(&mut *tx)
            .await?;
    }
    if let Some((run, party, totals)) = flush {
        flush_in(&mut tx, run, Some(party), totals, now()).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Whether a call by `principal` also counts for its agent: not for an administrator, and
/// not for a test account, whose usage stays under the test account so a test never uses
/// up an agent's daily allowance that real users share.
#[must_use]
pub fn charges_agent(principal: &webagent_access::Principal) -> bool {
    !principal.administrator() && !principal.test_account
}

/// Add one model call made for `access` (its requester and agent) today; `run` is the run
/// record holding that call, whose usage feed is flushed with it when a minute has passed
/// since its last flush (or at its first call).
pub async fn record(pool: &SqlitePool, access: &LiveAccess, turn: Turn, run: &Value) -> Result<()> {
    let agent = charges_agent(&access.principal).then_some(access.agent_id.as_str());
    let now = now();
    let due = run["id"].as_str().filter(|id| flush_due(id, now));
    let party = due.map(|_| RunParty::of(access, run));
    let flush = due
        .zip(party.as_ref())
        .map(|(id, party)| (id, party, RunTotals::of(run)));
    record_with(pool, &access.principal.user_id, agent, today(), turn, flush).await?;
    if let Some(id) = due {
        flushed(id, now, false);
    }
    Ok(())
}

/// One scope's model turns on `day` (zero when no row exists).
pub async fn used(pool: &SqlitePool, scope: &str, id: &str, day: i64) -> Result<u64> {
    let turns: Option<i64> =
        sqlx::query_scalar("SELECT turns FROM usage_daily WHERE scope=? AND id=? AND day=?")
            .bind(scope)
            .bind(id)
            .bind(day)
            .fetch_optional(pool)
            .await?;
    Ok(turns.map_or(0, |turns| u64::try_from(turns).unwrap_or(0)))
}

/// Why `access`'s requester may not use another model turn on its agent on `day`, or `None`.
/// Administrators are never limited. Reads only the ledger rows a limit applies to.
pub async fn refusal_on(
    pool: &SqlitePool,
    access: &LiveAccess,
    day: i64,
) -> Result<Option<String>> {
    if access.principal.administrator() {
        return Ok(None);
    }
    if let Some(limit) = access.tier.daily_turns {
        let mine = used(pool, REQUESTER, &access.principal.user_id, day).await?;
        if let Some(refusal) = daily_refusal(mine, Some(limit), "your account level") {
            return Ok(Some(refusal));
        }
    }
    if let Some(limit) = access.run_limits.agent_daily_turns {
        let agent = used(pool, AGENT, &access.agent_id, day).await?;
        return Ok(daily_refusal(agent, Some(limit), "this agent"));
    }
    Ok(None)
}

/// Add one finished command's processor time to its run and to its requester's day.
pub async fn record_command_on(
    pool: &SqlitePool,
    requester: &str,
    run: &str,
    day: i64,
    cpu_ms: u64,
) -> Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    for (scope, id) in [(REQUESTER, requester), (RUN, run)] {
        sqlx::query("INSERT INTO usage_daily(scope,id,day,cpu_ms) VALUES(?,?,?,?) ON CONFLICT(scope,id,day) DO UPDATE SET cpu_ms=cpu_ms+excluded.cpu_ms")
            .bind(scope)
            .bind(id)
            .bind(day)
            .bind(i64::try_from(cpu_ms).unwrap_or(i64::MAX))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// [`record_command_on`] for today, for `access`'s requester.
pub async fn record_command(
    pool: &SqlitePool,
    access: &LiveAccess,
    run: &str,
    cpu_ms: u64,
) -> Result<()> {
    record_command_on(pool, &access.principal.user_id, run, today(), cpu_ms).await
}

/// Why `access`'s requester may not start another command in `run` on `day`: the run's or the
/// account's command processor time is used up. Administrators are never limited.
pub async fn command_refusal_on(
    pool: &SqlitePool,
    access: &LiveAccess,
    run: &str,
    day: i64,
) -> Result<Option<String>> {
    if access.principal.administrator() {
        return Ok(None);
    }
    let limits = access.run_limits;
    if let Some(limit) = limits.command_cpu_seconds_per_run {
        let used: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(cpu_ms),0) FROM usage_daily WHERE scope=? AND id=?",
        )
        .bind(RUN)
        .bind(run)
        .fetch_one(pool)
        .await?;
        if used >= i64::from(limit) * 1000 {
            return Ok(Some(format!(
                "Command budget used: this run's commands have used their {limit} seconds of processor time. Start a new run to continue."
            )));
        }
    }
    if let Some(limit) = limits.command_cpu_seconds_per_user_day {
        let used: Option<i64> =
            sqlx::query_scalar("SELECT cpu_ms FROM usage_daily WHERE scope=? AND id=? AND day=?")
                .bind(REQUESTER)
                .bind(&access.principal.user_id)
                .bind(day)
                .fetch_optional(pool)
                .await?;
        if used.unwrap_or(0) >= i64::from(limit) * 1000 {
            return Ok(Some(format!(
                "Command budget used: your account's commands have used their {limit} seconds of processor time today. It resets at 00:00 UTC."
            )));
        }
    }
    Ok(None)
}

/// [`command_refusal_on`] for today.
pub async fn command_refusal(
    pool: &SqlitePool,
    access: &LiveAccess,
    run: &str,
) -> Result<Option<String>> {
    command_refusal_on(pool, access, run, today()).await
}

/// The tools whose commands count against the command budget: workspace and Sandbox commands.
#[must_use]
pub fn metered(name: &str) -> bool {
    matches!(
        name,
        "shell" | "python" | "sandbox_shell" | "sandbox_python"
    )
}

/// Commands going now, per run and per account, in this process (the loop runs them all).
static RUNNING: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, u32>>> =
    std::sync::LazyLock::new(Default::default);

/// One running command's place in the per-run and per-account counts; dropping it frees it.
#[derive(Debug)]
pub struct CommandSlot(Vec<String>);

impl CommandSlot {
    /// Take a place for a command of `run`, or say which limit is full. Administrators are
    /// counted but never refused.
    pub fn take(access: &LiveAccess, run: &str) -> std::result::Result<Self, String> {
        let limits = access.run_limits;
        let keys = [
            (
                format!("run:{run}"),
                limits.concurrent_commands_per_run,
                "this run",
            ),
            (
                format!("user:{}", access.principal.user_id),
                limits.concurrent_commands_per_user,
                "your account",
            ),
        ];
        let mut running = RUNNING.lock().unwrap_or_else(|error| error.into_inner());
        if !access.principal.administrator() {
            for (key, limit, whose) in &keys {
                if let Some(limit) = limit {
                    if running.get(key).copied().unwrap_or(0) >= *limit {
                        return Err(format!(
                            "Command limit reached: {whose} may run {limit} command{} at once. Wait for one to finish.",
                            if *limit == 1 { "" } else { "s" }
                        ));
                    }
                }
            }
        }
        let keys: Vec<String> = keys.into_iter().map(|(key, _, _)| key).collect();
        for key in &keys {
            *running.entry(key.clone()).or_insert(0) += 1;
        }
        Ok(Self(keys))
    }
}

impl Drop for CommandSlot {
    fn drop(&mut self) {
        let mut running = RUNNING.lock().unwrap_or_else(|error| error.into_inner());
        for key in &self.0 {
            if let Some(count) = running.get_mut(key) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    running.remove(key);
                }
            }
        }
    }
}

/// [`refusal_on`] for today.
pub async fn refusal(pool: &SqlitePool, access: &LiveAccess) -> Result<Option<String>> {
    refusal_on(pool, access, today()).await
}

/// Seconds between a running run's usage flushes.
pub const FLUSH_SECONDS: i64 = 60;
/// Usage rows the billing service has acknowledged are kept this long (seconds).
const ACKNOWLEDGED_ROWS_KEPT: i64 = 7 * 86_400;
/// Usage rows are kept at most this long, acknowledged or not (seconds).
const ROWS_KEPT: i64 = 35 * 86_400;
/// Days of owner day and month totals kept.
const OWNER_TOTAL_DAYS: i64 = 400;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

/// A run's usage totals so far, as its record's `usage` holds them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunTotals {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    /// Provider-reported cost in millionths of a US dollar.
    pub cost_micros: u64,
    /// Whether any of the run's calls reported a cost (otherwise its rows carry no cost).
    pub cost_reported: bool,
}

impl RunTotals {
    /// The totals of `run` (zero before its first model call).
    #[must_use]
    pub fn of(run: &Value) -> Self {
        let usage = &run["usage"];
        let count = |field: &str| usage[field].as_u64().unwrap_or(0);
        let cost = usage["known_cost_usd"]
            .as_f64()
            .filter(|cost| cost.is_finite() && *cost >= 0.0);
        let calls = count("reported_calls") + count("unreported_calls");
        Self {
            input_tokens: count("input_tokens"),
            cached_input_tokens: count("cached_input_tokens"),
            output_tokens: count("output_tokens"),
            cost_micros: cost.map_or(0, |cost| (cost * 1_000_000.0).round() as u64),
            cost_reported: calls > count("unknown_cost_calls"),
        }
    }
}

/// Whose usage a run's rows are: the agent's owner (who pays), the agent, the requester and
/// the model of the current call, and whether that model is a catalog model (any model whose
/// connection the agent's owner did not add; own models are never metered, spec 3.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunParty {
    pub owner: String,
    pub agent: String,
    pub requester: String,
    pub model: String,
    pub catalog: bool,
}

impl RunParty {
    /// The party of `run`'s current call for `access` (the call's live access).
    #[must_use]
    pub fn of(access: &LiveAccess, run: &Value) -> Self {
        Self::for_run(
            &access.agent_owner_id,
            &access.agent_id,
            &access.principal.user_id,
            run,
        )
    }

    /// The party of `run`'s current call on `agent`, owned by `owner`, for `requester`.
    #[must_use]
    pub fn for_run(owner: &str, agent: &str, requester: &str, run: &Value) -> Self {
        let index = run["failover_state"]["candidate_index"]
            .as_u64()
            .unwrap_or(0);
        let candidate = usize::try_from(index)
            .ok()
            .and_then(|index| run["selection_plan"]["candidates"].get(index));
        let model = candidate
            .and_then(|candidate| candidate["upstream_model"].as_str())
            .or_else(|| run["model"]["model"].as_str())
            .or_else(|| run["provider_selection"]["model"].as_str())
            .unwrap_or("");
        let connection_owner =
            candidate.and_then(|candidate| candidate["connection_owner_id"].as_str());
        Self {
            owner: owner.to_owned(),
            agent: agent.to_owned(),
            requester: requester.to_owned(),
            model: model.chars().take(128).collect(),
            catalog: connection_owner.is_none_or(|added| added.is_empty() || added != owner),
        }
    }
}

/// When each run of this process last flushed during a model call (Unix seconds).
static LAST_FLUSH: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, i64>>> =
    std::sync::LazyLock::new(Default::default);

/// Whether `run`'s usage should flush with its model call at `now`: at its first call in this
/// process and then once [`FLUSH_SECONDS`] have passed.
fn flush_due(run: &str, now: i64) -> bool {
    let last = LAST_FLUSH.lock().unwrap_or_else(|error| error.into_inner());
    last.get(run).is_none_or(|at| now - at >= FLUSH_SECONDS)
}

/// Note `run`'s flush at `now`, or forget it once the run has stopped (`ended`).
fn flushed(run: &str, now: i64, ended: bool) {
    let mut last = LAST_FLUSH.lock().unwrap_or_else(|error| error.into_inner());
    if ended {
        last.remove(run);
        return;
    }
    // Runs that ended in another process are never forgotten here; bound the map.
    if last.len() >= 10_000 {
        last.clear();
    }
    last.insert(run.to_owned(), now);
}

/// Whether a saved run in this state flushes its usage: it finished, stopped, paused or parked.
#[must_use]
pub fn flushes_when_saved(run: &Value) -> bool {
    run["parked"] == true
        || matches!(
            run["status"].as_str(),
            Some("completed" | "cancelled" | "failed" | "stopped" | "paused" | "parked")
        )
}

/// Flush `run`'s usage feed after its record was saved in a finished, stopped, paused or
/// parked state. A run with no model call flushed yet has nothing to add.
pub async fn flush_saved(pool: &SqlitePool, run: &Value) -> Result<()> {
    let Some(id) = run["id"].as_str() else {
        return Ok(());
    };
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    flush_in(&mut tx, id, None, RunTotals::of(run), now()).await?;
    tx.commit().await?;
    flushed(id, 0, true);
    Ok(())
}

/// The UTC month number (year × 12 + month − 1) of a UTC day number.
#[must_use]
pub fn month_of_day(day: i64) -> i64 {
    // Days to civil date (H. Hinnant's algorithm), proleptic Gregorian.
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    year * 12 + month - 1
}

/// The last day this process pruned the usage feed.
static FEED_PRUNED: AtomicI64 = AtomicI64::new(i64::MIN);

fn signed(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Add what `run` used since its last flush to the feed and its owner's day and month totals,
/// inside `tx`. `party` names whose usage it is (from the current call); without it the
/// party of the run's last flush is used, and a run never flushed adds nothing.
async fn flush_in(
    tx: &mut Transaction<'_, Sqlite>,
    run: &str,
    party: Option<&RunParty>,
    totals: RunTotals,
    now: i64,
) -> Result<bool> {
    let mark = sqlx::query_as::<_, (String, String, String, String, bool, i64, i64, i64, i64, i64)>(
        "SELECT owner_id,agent_id,requester_id,model,catalog,input_tokens,cached_input_tokens,output_tokens,cost_micros,compute_ms FROM usage_marks WHERE run_id=?",
    )
    .bind(run)
    .fetch_optional(&mut **tx)
    .await?;
    let party = match (party, &mark) {
        (Some(party), _) => party.clone(),
        (None, Some(mark)) => RunParty {
            owner: mark.0.clone(),
            agent: mark.1.clone(),
            requester: mark.2.clone(),
            model: mark.3.clone(),
            catalog: mark.4,
        },
        (None, None) => return Ok(false),
    };
    let before = mark
        .as_ref()
        .map_or([0; 5], |mark| [mark.5, mark.6, mark.7, mark.8, mark.9]);
    let compute: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(cpu_ms),0) FROM usage_daily WHERE scope=? AND id=?",
    )
    .bind(RUN)
    .bind(run)
    .fetch_one(&mut **tx)
    .await?;
    let after = [
        signed(totals.input_tokens),
        signed(totals.cached_input_tokens),
        signed(totals.output_tokens),
        signed(totals.cost_micros),
        compute,
    ];
    // Never negative: totals only grow, and a lower reading (a stale copy) adds nothing.
    let added: Vec<i64> = after
        .iter()
        .zip(before)
        .map(|(after, before)| (after - before).max(0))
        .collect();
    let wrote = added.iter().any(|value| *value > 0);
    if wrote {
        sqlx::query("INSERT INTO usage_turns(owner_id,agent_id,requester_id,model,catalog,input_tokens,cached_input_tokens,output_tokens,compute_ms,cost_micros,finished_at) VALUES(?,?,?,?,?,?,?,?,?,?,?)")
            .bind(&party.owner)
            .bind(&party.agent)
            .bind(&party.requester)
            .bind(&party.model)
            .bind(party.catalog)
            .bind(added[0])
            .bind(added[1])
            .bind(added[2])
            .bind(added[4])
            .bind(totals.cost_reported.then_some(added[3]))
            .bind(now)
            .execute(&mut **tx)
            .await?;
        let day_number = day(u64::try_from(now).unwrap_or(0));
        for (period, number) in [("day", day_number), ("month", month_of_day(day_number))] {
            sqlx::query("INSERT INTO usage_owner(owner_id,period,number,input_tokens,cached_input_tokens,output_tokens,compute_ms,cost_micros) VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(owner_id,period,number) DO UPDATE SET input_tokens=input_tokens+excluded.input_tokens,cached_input_tokens=cached_input_tokens+excluded.cached_input_tokens,output_tokens=output_tokens+excluded.output_tokens,compute_ms=compute_ms+excluded.compute_ms,cost_micros=cost_micros+excluded.cost_micros")
                .bind(&party.owner)
                .bind(period)
                .bind(number)
                .bind(added[0])
                .bind(added[1])
                .bind(added[2])
                .bind(added[4])
                .bind(added[3])
                .execute(&mut **tx)
                .await?;
        }
    }
    let kept = |index: usize| after[index].max(before[index]);
    sqlx::query("INSERT INTO usage_marks(run_id,owner_id,agent_id,requester_id,model,catalog,input_tokens,cached_input_tokens,output_tokens,cost_micros,compute_ms,flushed_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(run_id) DO UPDATE SET owner_id=excluded.owner_id,agent_id=excluded.agent_id,requester_id=excluded.requester_id,model=excluded.model,catalog=excluded.catalog,input_tokens=excluded.input_tokens,cached_input_tokens=excluded.cached_input_tokens,output_tokens=excluded.output_tokens,cost_micros=excluded.cost_micros,compute_ms=excluded.compute_ms,flushed_at=excluded.flushed_at")
        .bind(run)
        .bind(&party.owner)
        .bind(&party.agent)
        .bind(&party.requester)
        .bind(&party.model)
        .bind(party.catalog)
        .bind(kept(0))
        .bind(kept(1))
        .bind(kept(2))
        .bind(kept(3))
        .bind(kept(4))
        .bind(now)
        .execute(&mut **tx)
        .await?;
    let today = day(u64::try_from(now).unwrap_or(0));
    if FEED_PRUNED.swap(today, Ordering::Relaxed) != today {
        prune_feed(tx, now).await?;
    }
    Ok(wrote)
}

/// Retention (spec 8.6): a feed row goes once the billing cursor has passed it and it is older
/// than 7 days, and after 35 days in any case (also with no billing service); owner totals
/// after 400 days; a run's flush mark 35 days after its last flush.
async fn prune_feed(tx: &mut Transaction<'_, Sqlite>, now: i64) -> Result<()> {
    sqlx::query("DELETE FROM usage_turns WHERE finished_at<? OR (finished_at<? AND seq<=COALESCE((SELECT value FROM billing_state WHERE key='usage_cursor'),0))")
        .bind(now - ROWS_KEPT)
        .bind(now - ACKNOWLEDGED_ROWS_KEPT)
        .execute(&mut **tx)
        .await?;
    let oldest_day = day(u64::try_from(now).unwrap_or(0)) - OWNER_TOTAL_DAYS;
    sqlx::query("DELETE FROM usage_owner WHERE (period='day' AND number<?) OR (period='month' AND number<?)")
        .bind(oldest_day)
        .bind(month_of_day(oldest_day))
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM usage_marks WHERE flushed_at<?")
        .bind(now - ROWS_KEPT)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        schema(&pool).await.unwrap();
        schema(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn turns_add_up_per_scope_and_day_and_old_days_are_pruned() {
        let pool = pool().await;
        let turn = Turn {
            input_tokens: 10,
            output_tokens: 2,
            cost_micros: 500,
        };
        record_on(&pool, "u1", Some("a1"), 100, turn).await.unwrap();
        record_on(&pool, "u1", Some("a1"), 100, turn).await.unwrap();
        record_on(&pool, "admin", None, 100, turn).await.unwrap();
        assert_eq!(used(&pool, REQUESTER, "u1", 100).await.unwrap(), 2);
        // Cost is still recorded (and shown), never limited.
        let cost: i64 = sqlx::query_scalar(
            "SELECT cost_micros FROM usage_daily WHERE scope='requester' AND id='u1' AND day=100",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(cost, 1000);
        // An administrator's turn is counted for them, not for the agent.
        assert_eq!(used(&pool, AGENT, "a1", 100).await.unwrap(), 2);
        assert_eq!(used(&pool, REQUESTER, "admin", 100).await.unwrap(), 1);
        // The next day starts from zero and its first write prunes days past retention.
        assert_eq!(used(&pool, REQUESTER, "u1", 101).await.unwrap(), 0);
        record_on(&pool, "u1", None, 100 + RETENTION_DAYS, turn)
            .await
            .unwrap();
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM usage_daily WHERE day=100")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }

    #[tokio::test]
    async fn command_time_adds_up_per_run_and_per_requester_day() {
        let pool = pool().await;
        record_command_on(&pool, "u1", "r1", 100, 1500)
            .await
            .unwrap();
        record_command_on(&pool, "u1", "r1", 101, 500)
            .await
            .unwrap();
        let run: i64 =
            sqlx::query_scalar("SELECT SUM(cpu_ms) FROM usage_daily WHERE scope='run' AND id='r1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(run, 2000);
        let day: i64 = sqlx::query_scalar(
            "SELECT cpu_ms FROM usage_daily WHERE scope='requester' AND id='u1' AND day=100",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(day, 1500);
        // A command adds no model turn.
        assert_eq!(used(&pool, REQUESTER, "u1", 100).await.unwrap(), 0);
    }

    #[test]
    fn test_accounts_and_administrators_are_not_charged_to_the_agent() {
        let principal = |role, test_account| webagent_access::Principal {
            user_id: "u".into(),
            status: webagent_access::AccountStatus::Active,
            role,
            tier_id: "free".into(),
            access_revision: 1,
            test_account,
        };
        use webagent_access::Role;
        assert!(charges_agent(&principal(Role::User, false)));
        assert!(charges_agent(&principal(Role::Guest, false)));
        assert!(!charges_agent(&principal(Role::User, true)));
        assert!(!charges_agent(&principal(Role::Guest, true)));
        assert!(!charges_agent(&principal(Role::Administrator, false)));
    }

    #[test]
    fn latest_call_numbers_and_unknown_cost() {
        let run = serde_json::json!({"usage":{"latest":{"input_tokens":7,"output_tokens":3,"cost_usd":0.0125}}});
        assert_eq!(
            Turn::latest(&run),
            Turn {
                input_tokens: 7,
                output_tokens: 3,
                cost_micros: 12_500
            }
        );
        assert_eq!(Turn::latest(&serde_json::json!({})), Turn::default());
    }

    fn party(model: &str, catalog: bool) -> RunParty {
        RunParty {
            owner: "owner".into(),
            agent: "agent".into(),
            requester: "visitor".into(),
            model: model.into(),
            catalog,
        }
    }

    fn totals(input: u64, output: u64, cost_micros: u64) -> RunTotals {
        RunTotals {
            input_tokens: input,
            cached_input_tokens: input / 2,
            output_tokens: output,
            cost_micros,
            cost_reported: true,
        }
    }

    async fn flush(
        pool: &SqlitePool,
        run: &str,
        party: Option<&RunParty>,
        t: RunTotals,
        now: i64,
    ) -> bool {
        let mut tx = pool.begin().await.unwrap();
        let wrote = flush_in(&mut tx, run, party, t, now).await.unwrap();
        tx.commit().await.unwrap();
        wrote
    }

    async fn rows(
        pool: &SqlitePool,
    ) -> Vec<(String, String, bool, i64, i64, i64, i64, Option<i64>)> {
        sqlx::query_as("SELECT requester_id,model,catalog,input_tokens,cached_input_tokens,output_tokens,compute_ms,cost_micros FROM usage_turns ORDER BY seq")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_run_flushes_only_what_it_added_and_its_owner_totals_follow() {
        let pool = pool().await;
        let now = 20_000 * 86_400 + 100;
        // Nothing flushed yet and no party: a run without a model call adds nothing.
        assert!(!flush(&pool, "r1", None, totals(5, 1, 0), now).await);
        assert!(rows(&pool).await.is_empty());
        // First flush during a model call, then a command's processor time, then the final
        // flush at the save without a party (taken from the run's mark).
        assert!(
            flush(
                &pool,
                "r1",
                Some(&party("m1", true)),
                totals(100, 10, 400),
                now
            )
            .await
        );
        record_command_on(&pool, "visitor", "r1", 20_000, 2_500)
            .await
            .unwrap();
        assert!(flush(&pool, "r1", None, totals(160, 30, 900), now + 70).await);
        // Saving the finished run again adds nothing; a stale, lower copy adds nothing either.
        assert!(!flush(&pool, "r1", None, totals(160, 30, 900), now + 80).await);
        assert!(!flush(&pool, "r1", None, totals(150, 20, 800), now + 90).await);
        assert_eq!(
            rows(&pool).await,
            vec![
                (
                    "visitor".into(),
                    "m1".into(),
                    true,
                    100,
                    50,
                    10,
                    0,
                    Some(400)
                ),
                (
                    "visitor".into(),
                    "m1".into(),
                    true,
                    60,
                    30,
                    20,
                    2_500,
                    Some(500)
                ),
            ]
        );
        let owner: Vec<(String, i64, i64, i64, i64)> = sqlx::query_as(
            "SELECT period,number,input_tokens,compute_ms,cost_micros FROM usage_owner WHERE owner_id='owner' ORDER BY period",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            owner,
            vec![
                ("day".into(), 20_000, 160, 2_500, 900),
                ("month".into(), month_of_day(20_000), 160, 2_500, 900),
            ]
        );
        // A run whose provider reports no cost carries none (null), never a made-up zero.
        let mut unreported = totals(10, 1, 0);
        unreported.cost_reported = false;
        assert!(flush(&pool, "r2", Some(&party("own", false)), unreported, now).await);
        assert_eq!(rows(&pool).await[2].7, None);
        assert!(!rows(&pool).await[2].2);
    }

    #[test]
    fn the_party_names_the_owner_and_whether_the_model_is_the_owners_own() {
        let run = |connection_owner: Option<&str>| {
            let mut candidate = serde_json::json!({"upstream_model":"deepseek-flash"});
            if let Some(owner) = connection_owner {
                candidate["connection_owner_id"] = serde_json::json!(owner);
            }
            serde_json::json!({"failover_state":{"candidate_index":0},
                "selection_plan":{"candidates":[candidate]}})
        };
        let own = RunParty::for_run("owner", "agent", "visitor", &run(Some("owner")));
        assert_eq!((own.model.as_str(), own.catalog), ("deepseek-flash", false));
        for other in [Some("admin"), Some(""), None] {
            assert!(RunParty::for_run("owner", "agent", "visitor", &run(other)).catalog);
        }
        let totals = RunTotals::of(&serde_json::json!({"usage":{"input_tokens":9,
            "cached_input_tokens":4,"output_tokens":2,"known_cost_usd":0.0125,
            "reported_calls":2,"unreported_calls":0,"unknown_cost_calls":1}}));
        assert_eq!(
            totals,
            RunTotals {
                input_tokens: 9,
                cached_input_tokens: 4,
                output_tokens: 2,
                cost_micros: 12_500,
                cost_reported: true
            }
        );
        assert!(
            !RunTotals::of(
                &serde_json::json!({"usage":{"reported_calls":1,"unknown_cost_calls":1}})
            )
            .cost_reported
        );
    }

    #[tokio::test]
    async fn the_feed_keeps_rows_until_billing_has_them_and_a_week_or_35_days_have_passed() {
        let pool = pool().await;
        let now = 20_000 * 86_400;
        let day = 86_400;
        for (run, at) in [
            ("old", now - 36 * day),
            ("acked", now - 8 * day),
            ("unacked", now - 8 * day),
            ("recent", now - day),
        ] {
            flush(&pool, run, Some(&party("m", true)), totals(1, 1, 1), at).await;
        }
        sqlx::query("INSERT INTO usage_owner(owner_id,period,number) VALUES('owner','day',?),('owner','day',?)")
            .bind(20_000 - 401)
            .bind(20_000 - 399)
            .execute(&pool)
            .await
            .unwrap();
        // Billing has acknowledged the first two rows (old, acked).
        sqlx::query("INSERT INTO billing_state(key,value) VALUES('usage_cursor',2)")
            .execute(&pool)
            .await
            .unwrap();
        let mut tx = pool.begin().await.unwrap();
        prune_feed(&mut tx, now).await.unwrap();
        tx.commit().await.unwrap();
        let left: Vec<i64> = sqlx::query_scalar("SELECT seq FROM usage_turns ORDER BY seq")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(left, vec![3, 4]);
        let days: Vec<i64> =
            sqlx::query_scalar("SELECT number FROM usage_owner WHERE period='day' ORDER BY number")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(!days.contains(&(20_000 - 401)) && days.contains(&(20_000 - 399)));
        // The old run's mark went with it; a run flushed a day ago keeps its mark.
        let marks: Vec<String> =
            sqlx::query_scalar("SELECT run_id FROM usage_marks ORDER BY run_id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(marks, vec!["acked", "recent", "unacked"]);
    }

    #[test]
    fn months_and_flush_timing() {
        assert_eq!(month_of_day(0), 1970 * 12);
        assert_eq!(month_of_day(31), 1970 * 12 + 1);
        // 2024-02-29 and 2024-03-01.
        assert_eq!(month_of_day(19_782), 2024 * 12 + 1);
        assert_eq!(month_of_day(19_783), 2024 * 12 + 2);
        assert!(flush_due("timing-run", 1_000));
        flushed("timing-run", 1_000, false);
        assert!(!flush_due("timing-run", 1_059));
        assert!(flush_due("timing-run", 1_060));
        flushed("timing-run", 0, true);
        assert!(flush_due("timing-run", 1_001));
        let paused = serde_json::json!({"status":"paused"});
        assert!(flushes_when_saved(&paused));
        assert!(flushes_when_saved(
            &serde_json::json!({"status":"waiting_approval","parked":true})
        ));
        assert!(!flushes_when_saved(
            &serde_json::json!({"status":"running"})
        ));
    }
}
