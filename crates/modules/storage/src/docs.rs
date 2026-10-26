//! Raw document and event SQL shared by every plane. Callers own the encoding,
//! the transaction boundary and any domain guard around these statements.
use anyhow::Result;
use sqlx::{Row, SqliteConnection, SqliteExecutor, SqlitePool};

/// Read one encoded document.
pub async fn read(connection: &mut SqliteConnection, key: &str) -> Result<Option<Vec<u8>>> {
    Ok(sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(key)
        .fetch_optional(connection)
        .await?)
}

/// Insert or replace one encoded document.
pub async fn upsert(connection: &mut SqliteConnection, key: &str, value: &[u8]) -> Result<()> {
    sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
        .bind(key).bind(value).execute(connection).await?;
    Ok(())
}

/// Delete one document. The split input is owned by its recovery record, not the
/// transcript, so a run's rows go with it and a recreated run cannot inherit them.
pub async fn remove(connection: &mut SqliteConnection, key: &str) -> Result<()> {
    sqlx::query("DELETE FROM docs WHERE key=?")
        .bind(key)
        .execute(&mut *connection)
        .await?;
    if let Some(rid) = key.strip_prefix("run:") {
        sqlx::query("DELETE FROM recovery_input WHERE run_id=?")
            .bind(rid)
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
}

/// Read every encoded document whose key starts with `prefix`, in key order.
pub async fn scan(connection: &mut SqliteConnection, prefix: &str) -> Result<Vec<Vec<u8>>> {
    Ok(
        sqlx::query_scalar("SELECT value FROM docs WHERE key LIKE ? ORDER BY key")
            .bind(format!("{prefix}%"))
            .fetch_all(connection)
            .await?,
    )
}

/// Append one encoded event to a plane's log.
pub async fn append_event<'c, E: SqliteExecutor<'c>>(
    executor: E,
    sid: &str,
    kind: &str,
    payload: &[u8],
) -> Result<()> {
    sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,?,?)")
        .bind(sid)
        .bind(kind)
        .bind(payload)
        .execute(executor)
        .await?;
    Ok(())
}

/// Read a bounded session-event delta after a sequence cursor: `(seq, kind, payload)`.
pub async fn session_events(
    pool: &SqlitePool,
    sid: &str,
    after: i64,
) -> Result<Vec<(i64, String, Vec<u8>)>> {
    let rows = sqlx::query(
        "SELECT seq,kind,payload FROM events WHERE session_id=? AND seq>? ORDER BY seq LIMIT 200",
    )
    .bind(sid)
    .bind(after)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| (r.get("seq"), r.get("kind"), r.get("payload")))
        .collect())
}
