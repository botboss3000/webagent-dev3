//! Per-account deadlines in the shared `app.db`: one row per account and kind with the time
//! (Unix seconds) the account next needs a visit. Rows hold only an account id, a kind and a
//! time. They decide only *when* a scan looks; what is done is always decided from the
//! account's own database at that moment, and the consumer that visits an account writes its
//! row again afterwards (a later time, or none).
use anyhow::Result;
use sqlx::SqlitePool;

/// An approval wait's expiry (written by the loop's scheduler pass).
pub const APPROVAL: &str = "approval";
/// The automation package's next tick (written by its module service).
pub const AUTOMATION: &str = "automation";
/// The earliest keep-alive check of the account's saved sessions.
pub const KEEP_ALIVE: &str = "keep_alive";

/// Create the table and its time index; idempotent.
pub async fn schema(pool: &SqlitePool) -> Result<()> {
    sqlx::raw_sql(
        "CREATE TABLE IF NOT EXISTS account_due(user_id TEXT NOT NULL,kind TEXT NOT NULL,next_due_at INTEGER NOT NULL,PRIMARY KEY(user_id,kind));
        CREATE INDEX IF NOT EXISTS account_due_at ON account_due(next_due_at);",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Store (or with `None` remove) one account's deadline of `kind`.
pub async fn set(pool: &SqlitePool, user: &str, kind: &str, at: Option<u64>) -> Result<()> {
    match at {
        Some(at) => {
            sqlx::query("INSERT INTO account_due(user_id,kind,next_due_at) VALUES(?,?,?) ON CONFLICT(user_id,kind) DO UPDATE SET next_due_at=excluded.next_due_at")
                .bind(user)
                .bind(kind)
                .bind(i64::try_from(at).unwrap_or(i64::MAX))
                .execute(pool)
                .await?;
        }
        None => {
            sqlx::query("DELETE FROM account_due WHERE user_id=? AND kind=?")
                .bind(user)
                .bind(kind)
                .execute(pool)
                .await?;
        }
    }
    Ok(())
}

/// Bring one account's deadline of `kind` forward to `at` (never later): for writers that add
/// work, so a deadline another session of the account already holds is kept.
pub async fn earlier(pool: &SqlitePool, user: &str, kind: &str, at: u64) -> Result<()> {
    sqlx::query("INSERT INTO account_due(user_id,kind,next_due_at) VALUES(?,?,?) ON CONFLICT(user_id,kind) DO UPDATE SET next_due_at=min(next_due_at,excluded.next_due_at)")
        .bind(user)
        .bind(kind)
        .bind(i64::try_from(at).unwrap_or(i64::MAX))
        .execute(pool)
        .await?;
    Ok(())
}

/// Accounts whose deadline of `kind` has passed by `now` (one indexed read).
pub async fn passed(pool: &SqlitePool, kind: &str, now: u64) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT user_id FROM account_due WHERE next_due_at<=? AND kind=? ORDER BY next_due_at",
    )
    .bind(i64::try_from(now).unwrap_or(i64::MAX))
    .bind(kind)
    .fetch_all(pool)
    .await?)
}

/// Every stored deadline: `(account, kind, time)`. The table holds one row per account that
/// has a deadline at all, so it stays small.
pub async fn all(pool: &SqlitePool) -> Result<Vec<(String, String, u64)>> {
    let rows: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT user_id,kind,next_due_at FROM account_due")
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .map(|(user, kind, at)| (user, kind, u64::try_from(at).unwrap_or(0)))
        .collect())
}

/// Writes only when a deadline changes, so a consumer that recomputes after every visit does
/// not write the shared database each time. Per process; a restart writes each row once more.
#[derive(Default)]
pub struct Written(
    std::sync::Mutex<std::collections::HashMap<(String, &'static str), Option<u64>>>,
);

impl Written {
    pub async fn set(
        &self,
        pool: &SqlitePool,
        user: &str,
        kind: &'static str,
        at: Option<u64>,
    ) -> Result<()> {
        let key = (user.to_owned(), kind);
        if self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("Deadline registry unavailable"))?
            .get(&key)
            == Some(&at)
        {
            return Ok(());
        }
        set(pool, user, kind, at).await?;
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("Deadline registry unavailable"))?
            .insert(key, at);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn deadlines_are_stored_replaced_and_removed() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        super::schema(&pool).await.unwrap();
        super::schema(&pool).await.unwrap();
        super::set(&pool, "u1", super::APPROVAL, Some(10))
            .await
            .unwrap();
        super::set(&pool, "u1", super::APPROVAL, Some(20))
            .await
            .unwrap();
        super::set(&pool, "u2", super::AUTOMATION, Some(5))
            .await
            .unwrap();
        let mut rows = super::all(&pool).await.unwrap();
        rows.sort();
        assert_eq!(
            rows,
            vec![
                ("u1".into(), "approval".into(), 20),
                ("u2".into(), "automation".into(), 5)
            ]
        );
        super::earlier(&pool, "u2", super::AUTOMATION, 9)
            .await
            .unwrap();
        super::earlier(&pool, "u2", super::AUTOMATION, 3)
            .await
            .unwrap();
        assert_eq!(
            super::passed(&pool, super::AUTOMATION, 4).await.unwrap(),
            vec!["u2"]
        );
        assert!(super::passed(&pool, super::APPROVAL, 4)
            .await
            .unwrap()
            .is_empty());
        super::set(&pool, "u2", super::AUTOMATION, Some(5))
            .await
            .unwrap();
        let written = super::Written::default();
        written
            .set(&pool, "u1", super::APPROVAL, None)
            .await
            .unwrap();
        assert_eq!(super::all(&pool).await.unwrap().len(), 1);
    }
}
