//! Daily usage ledger in the shared `app.db`: per requester and per agent, per UTC day, the
//! model turns, tokens and known provider cost (numbers and ids only, no content). Daily limits
//! (account levels' `daily_turns`/`daily_cost_usd`, the policy's `run_limits.agent_daily_*`) are
//! checked against it at message admission and before each model turn. Rows older than
//! [`RETENTION_DAYS`] are deleted by the first write of each day.
use anyhow::Result;
use sqlx::SqlitePool;
use std::sync::atomic::{AtomicI64, Ordering};
use webagent_access::authority::LiveAccess;
use webagent_access::run_limits::{daily_refusal, DailyUsage};

/// Usage of one requester (the account that sent the run's message).
pub const REQUESTER: &str = "requester";
/// Usage of one agent by non-administrators.
pub const AGENT: &str = "agent";
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

/// Create the table; idempotent.
pub async fn schema(pool: &SqlitePool) -> Result<()> {
    sqlx::query("CREATE TABLE IF NOT EXISTS usage_daily(scope TEXT NOT NULL,id TEXT NOT NULL,day INTEGER NOT NULL,turns INTEGER NOT NULL DEFAULT 0,input_tokens INTEGER NOT NULL DEFAULT 0,output_tokens INTEGER NOT NULL DEFAULT 0,cost_micros INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(scope,id,day))")
        .execute(pool)
        .await?;
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
    tx.commit().await?;
    Ok(())
}

/// Add one model call made for `access` (its requester and agent) today.
pub async fn record(pool: &SqlitePool, access: &LiveAccess, turn: Turn) -> Result<()> {
    let agent = (!access.principal.administrator()).then_some(access.agent_id.as_str());
    record_on(pool, &access.principal.user_id, agent, today(), turn).await
}

/// One scope's usage on `day` (zero when no row exists).
pub async fn used(pool: &SqlitePool, scope: &str, id: &str, day: i64) -> Result<DailyUsage> {
    let row: Option<(i64, i64)> = sqlx::query_as(
        "SELECT turns,cost_micros FROM usage_daily WHERE scope=? AND id=? AND day=?",
    )
    .bind(scope)
    .bind(id)
    .bind(day)
    .fetch_optional(pool)
    .await?;
    Ok(
        row.map_or_else(DailyUsage::default, |(turns, cost)| DailyUsage {
            turns: u64::try_from(turns).unwrap_or(0),
            cost_micros: u64::try_from(cost).unwrap_or(0),
        }),
    )
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
    let tier = &access.tier;
    if tier.daily_turns.is_some() || tier.daily_cost_usd.is_some() {
        let mine = used(pool, REQUESTER, &access.principal.user_id, day).await?;
        if let Some(refusal) = daily_refusal(
            mine,
            tier.daily_turns,
            tier.daily_cost_usd,
            "your account level",
        ) {
            return Ok(Some(refusal));
        }
    }
    let limits = access.run_limits;
    if limits.agent_daily_turns.is_some() || limits.agent_daily_cost_usd.is_some() {
        let agent = used(pool, AGENT, &access.agent_id, day).await?;
        return Ok(daily_refusal(
            agent,
            limits.agent_daily_turns,
            limits.agent_daily_cost_usd,
            "this agent",
        ));
    }
    Ok(None)
}

/// [`refusal_on`] for today.
pub async fn refusal(pool: &SqlitePool, access: &LiveAccess) -> Result<Option<String>> {
    refusal_on(pool, access, today()).await
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
        assert_eq!(
            used(&pool, REQUESTER, "u1", 100).await.unwrap(),
            DailyUsage {
                turns: 2,
                cost_micros: 1000
            }
        );
        // An administrator's turn is counted for them, not for the agent.
        assert_eq!(used(&pool, AGENT, "a1", 100).await.unwrap().turns, 2);
        assert_eq!(used(&pool, REQUESTER, "admin", 100).await.unwrap().turns, 1);
        // The next day starts from zero and its first write prunes days past retention.
        assert_eq!(used(&pool, REQUESTER, "u1", 101).await.unwrap().turns, 0);
        record_on(&pool, "u1", None, 100 + RETENTION_DAYS, turn)
            .await
            .unwrap();
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM usage_daily WHERE day=100")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
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
}
