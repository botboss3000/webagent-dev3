//! Staging, relay, retention and outbox reads over borrowed pools and transactions.
//!
//! Staging writes run inside the lifecycle owner's transaction on its user plane. The relay
//! moves committed staging rows into the application outbox; its durable watermark and the
//! insertions share one transaction, so a crash before the staging rows are acknowledged
//! cannot recreate events below the watermark, including pruned ones.
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

use crate::now;
use crate::staging::{self, Notice};

/// How long relayed notifications stay readable.
pub const RETENTION_SECONDS: i64 = 30 * 24 * 60 * 60;
const MAX_OUTBOX_ROWS: i64 = 1_000_000;
const MAX_STAGED_ROWS: i64 = 10_000;

/// The storage plane whose notification tables [`schema`] creates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plane {
    /// A user plane: staging rows and deduplication markers.
    User,
    /// The application plane: outbox, relay watermarks and retention floor.
    Application,
}

/// The staging identity of one user-plane handle.
#[derive(Clone, Copy, Debug)]
pub struct Source<'a> {
    /// User whose plane stages the notification.
    pub user_id: &'a str,
    /// The handle's agent; owner of plane-wide `main_notes.updated` events.
    pub agent_id: &'a str,
}

/// Create the notification tables of `plane` in `pool`.
pub async fn schema(pool: &SqlitePool, plane: Plane) -> Result<()> {
    if plane == Plane::User {
        sqlx::raw_sql("CREATE TABLE IF NOT EXISTS notification_stage(
            source_seq INTEGER PRIMARY KEY AUTOINCREMENT,event_id TEXT NOT NULL UNIQUE,
            type TEXT NOT NULL,session_id TEXT NOT NULL,run_id TEXT,agent_id TEXT NOT NULL,
            actor TEXT NOT NULL,payload TEXT NOT NULL CHECK(length(payload)<=2048),created_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS notification_emitted(event_id TEXT PRIMARY KEY);")
            .execute(pool).await?;
    } else {
        sqlx::raw_sql("CREATE TABLE IF NOT EXISTS notification_outbox(
            seq INTEGER PRIMARY KEY AUTOINCREMENT,event_id TEXT NOT NULL UNIQUE,
            type TEXT NOT NULL,session_id TEXT NOT NULL,run_id TEXT,agent_id TEXT NOT NULL,
            actor TEXT NOT NULL,payload TEXT NOT NULL CHECK(length(payload)<=2048),created_at INTEGER NOT NULL,
            source_user TEXT NOT NULL,source_seq INTEGER NOT NULL,retained_at INTEGER NOT NULL,UNIQUE(source_user,source_seq));
            CREATE INDEX IF NOT EXISTS notification_created ON notification_outbox(retained_at,seq);
            CREATE TABLE IF NOT EXISTS notification_relay(source_user TEXT PRIMARY KEY,through_seq INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS notification_retention(singleton INTEGER PRIMARY KEY CHECK(singleton=1),pruned_through INTEGER NOT NULL);
            INSERT OR IGNORE INTO notification_retention VALUES(1,0);")
            .execute(pool).await?;
        crate::alerts::schema(pool).await?;
    }
    Ok(())
}

/// Stage `notice` inside the lifecycle owner's transaction `tx`.
///
/// A repeated event key is a no-op. Unless the event is plane-wide, the agent is read from the
/// session record, which `unseal` decodes. No caller payload is copied.
pub async fn stage<F>(
    tx: &mut Transaction<'_, Sqlite>,
    source: &Source<'_>,
    notice: &Notice<'_>,
    unseal: F,
) -> Result<()>
where
    F: FnOnce(&[u8]) -> Result<Value>,
{
    let kind = notice.kind;
    if !staging::supported(kind) {
        return Err(anyhow!("Unsupported notification type"));
    }
    let event_id = staging::event_id(source.user_id, notice.event_key);
    // Keep a compact deduplication marker for the originating resource lifetime.
    let inserted = sqlx::query("INSERT OR IGNORE INTO notification_emitted(event_id) VALUES(?)")
        .bind(&event_id)
        .execute(&mut **tx)
        .await?;
    if inserted.rows_affected() == 0 {
        return Ok(());
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_stage")
        .fetch_one(&mut **tx)
        .await?;
    if count >= MAX_STAGED_ROWS {
        return Err(anyhow!("Notification staging capacity exhausted"));
    }
    let agent = if kind == "main_notes.updated" {
        source.agent_id.to_owned()
    } else {
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(format!("session:{}", notice.session_id))
            .fetch_one(&mut **tx)
            .await?;
        unseal(&raw)?["agent_id"]
            .as_str()
            .unwrap_or("shared_default")
            .to_owned()
    };
    let actor = staging::actor(kind, source.user_id);
    let payload = staging::payload(notice.status);
    sqlx::query(
        "INSERT INTO notification_stage(event_id,type,session_id,run_id,agent_id,actor,payload,created_at)
            VALUES(?,?,?,?,?,?,?,?)",
    )
    .bind(event_id)
    .bind(kind)
    .bind(notice.session_id)
    .bind(notice.run_id)
    .bind(agent)
    .bind(actor)
    .bind(payload.to_string())
    .bind(now() as i64)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Stage the notification a persisted `run` checkpoint produces, if its status emits one.
pub async fn stage_run<F>(
    tx: &mut Transaction<'_, Sqlite>,
    source: &Source<'_>,
    run: &Value,
    unseal: F,
) -> Result<()>
where
    F: FnOnce(&[u8]) -> Result<Value>,
{
    let Some(run) = staging::run_notice(run)? else {
        return Ok(());
    };
    let notice = Notice {
        event_key: &run.event_key,
        kind: run.kind,
        session_id: &run.session_id,
        run_id: Some(&run.run_id),
        status: Some(&run.status),
    };
    stage(tx, source, &notice, unseal).await
}

/// Move up to 200 committed staging rows of `user_id` from `user_pool` into the outbox in
/// `app_pool`, prune the outbox, then acknowledge the moved rows. Returns the rows moved.
pub async fn relay(app_pool: &SqlitePool, user_pool: &SqlitePool, user_id: &str) -> Result<usize> {
    let mut tx = app_pool.begin().await?;
    sqlx::query("INSERT OR IGNORE INTO notification_relay VALUES(?,0)")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE notification_relay SET through_seq=through_seq WHERE source_user=?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    let mut through: i64 =
        sqlx::query_scalar("SELECT through_seq FROM notification_relay WHERE source_user=?")
            .bind(user_id)
            .fetch_one(&mut *tx)
            .await?;
    let rows = sqlx::query(
        "SELECT * FROM notification_stage WHERE source_seq>? ORDER BY source_seq LIMIT 200",
    )
    .bind(through)
    .fetch_all(user_pool)
    .await?;
    for row in &rows {
        sqlx::query(
            "INSERT INTO notification_outbox(event_id,type,session_id,run_id,agent_id,actor,payload,created_at,source_user,source_seq,retained_at)
                VALUES(?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(row.get::<String, _>("event_id"))
        .bind(row.get::<String, _>("type"))
        .bind(row.get::<String, _>("session_id"))
        .bind(row.get::<Option<String>, _>("run_id"))
        .bind(row.get::<String, _>("agent_id"))
        .bind(row.get::<String, _>("actor"))
        .bind(row.get::<String, _>("payload"))
        .bind(row.get::<i64, _>("created_at"))
        .bind(user_id)
        .bind(row.get::<i64, _>("source_seq"))
        .bind(now() as i64)
        .execute(&mut *tx)
        .await?;
        through = row.get("source_seq");
    }
    sqlx::query("UPDATE notification_relay SET through_seq=? WHERE source_user=?")
        .bind(through)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    prune_tx(&mut tx, now() as i64).await?;
    tx.commit().await?;
    sqlx::query("DELETE FROM notification_stage WHERE source_seq<=?")
        .bind(through)
        .execute(user_pool)
        .await?;
    Ok(rows.len())
}

/// Delete expired and overflowing outbox rows and raise the retention floor over them.
async fn prune_tx(tx: &mut Transaction<'_, Sqlite>, now: i64) -> Result<usize> {
    let expired: Option<i64> =
        sqlx::query_scalar("SELECT max(seq) FROM notification_outbox WHERE retained_at<?")
            .bind(now - RETENTION_SECONDS)
            .fetch_one(&mut **tx)
            .await?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_outbox")
        .fetch_one(&mut **tx)
        .await?;
    let overflow = if count > MAX_OUTBOX_ROWS {
        sqlx::query_scalar::<_, i64>(
            "SELECT seq FROM notification_outbox ORDER BY seq LIMIT 1 OFFSET ?",
        )
        .bind(count - MAX_OUTBOX_ROWS - 1)
        .fetch_optional(&mut **tx)
        .await?
    } else {
        None
    };
    if let Some(through) = expired.into_iter().chain(overflow).max() {
        let deleted = sqlx::query("DELETE FROM notification_outbox WHERE seq<=?")
            .bind(through)
            .execute(&mut **tx)
            .await?;
        sqlx::query(
            "UPDATE notification_retention SET pruned_through=max(pruned_through,?) WHERE singleton=1",
        )
        .bind(through)
        .execute(&mut **tx)
        .await?;
        return Ok(deleted.rows_affected() as usize);
    }
    Ok(0)
}

/// Apply retention to the outbox in `app_pool` as of `now`. Returns the rows removed.
pub async fn prune(app_pool: &SqlitePool, now: i64) -> Result<usize> {
    let mut tx = app_pool.begin().await?;
    sqlx::query(
        "UPDATE notification_retention SET pruned_through=pruned_through WHERE singleton=1",
    )
    .execute(&mut *tx)
    .await?;
    let removed = prune_tx(&mut tx, now).await?;
    tx.commit().await?;
    Ok(removed)
}

/// Up to `limit` outbox events after `since`, with cursor bounds, or a `cursor_expired`
/// document when `since` is below the retention floor.
pub async fn page(app_pool: &SqlitePool, since: i64, limit: i64) -> Result<Value> {
    let mut tx = app_pool.begin().await?;
    let floor: i64 =
        sqlx::query_scalar("SELECT pruned_through FROM notification_retention WHERE singleton=1")
            .fetch_one(&mut *tx)
            .await?;
    let latest: i64 = sqlx::query_scalar(
        "SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name='notification_outbox'),0)",
    )
    .fetch_one(&mut *tx)
    .await?;
    let earliest: Option<i64> = sqlx::query_scalar("SELECT min(seq) FROM notification_outbox")
        .fetch_one(&mut *tx)
        .await?;
    if since < floor {
        return Ok(
            json!({"error":"cursor_expired","pruned_through":floor,"earliest_seq":earliest,
            "latest_seq":latest,"resume_since":floor,"retention_seconds":RETENTION_SECONDS}),
        );
    }
    let rows = sqlx::query("SELECT * FROM notification_outbox WHERE seq>? ORDER BY seq LIMIT ?")
        .bind(since)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        let payload: Value = serde_json::from_str(&row.get::<String, _>("payload"))?;
        events.push(json!({"seq":row.get::<i64,_>("seq"),"event_id":row.get::<String,_>("event_id"),
            "type":row.get::<String,_>("type"),"session_id":row.get::<String,_>("session_id"),
            "run_id":row.get::<Option<String>,_>("run_id"),"agent_id":row.get::<String,_>("agent_id"),
            "actor":row.get::<String,_>("actor"),"payload":payload,"created_at":row.get::<i64,_>("created_at")}));
    }
    tx.commit().await?;
    let next = events
        .last()
        .and_then(|v| v["seq"].as_i64())
        .unwrap_or(since);
    Ok(
        json!({"events":events,"next_seq":next,"latest_seq":latest,"earliest_seq":earliest,
        "pruned_through":floor,"has_more":next<latest,"retention_seconds":RETENTION_SECONDS}),
    )
}

#[cfg(test)]
#[path = "outbox_tests.rs"]
mod tests;
