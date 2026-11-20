//! Per-account app notifications: short messages in the user's own database that the
//! Notifications page lists for every signed-in user, such as storage nearly full or what an
//! automatic cleanup removed. Kept 30 days, at most 200; the page marks them read.
use super::*;

const KEEP_SECONDS: i64 = 30 * 86_400;
const KEEP_ROWS: i64 = 200;
const MAX_MESSAGE_CHARS: usize = 2_000;
pub(crate) const SEVERITIES: [&str; 3] = ["info", "warning", "critical"];

impl Store {
    /// Add one notification; older ones past the retention window or row cap are dropped.
    pub(crate) async fn add_notice(&self, kind: &str, severity: &str, message: &str) -> Result<()> {
        if !SEVERITIES.contains(&severity) || kind.is_empty() || kind.len() > 64 {
            bail!("Invalid notification");
        }
        let message: String = message.chars().take(MAX_MESSAGE_CHARS).collect();
        let now = webagent_host_core::now() as i64;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("INSERT INTO user_notices(kind,severity,body,created_at) VALUES(?,?,?,?)")
            .bind(kind)
            .bind(severity)
            .bind(self.encode_user_record(&json!({"message": message}))?)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "DELETE FROM user_notices WHERE created_at<? OR id NOT IN
             (SELECT id FROM user_notices ORDER BY id DESC LIMIT ?)",
        )
        .bind(now - KEEP_SECONDS)
        .bind(KEEP_ROWS)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Newest notifications first, with the unread count.
    pub(crate) async fn notices(&self) -> Result<Value> {
        let rows = sqlx::query(
            "SELECT id,kind,severity,body,created_at,read_at FROM user_notices ORDER BY id DESC LIMIT ?",
        )
        .bind(KEEP_ROWS)
        .fetch_all(&self.pool)
        .await?;
        let mut notices = Vec::with_capacity(rows.len());
        let mut unread = 0;
        for row in rows {
            let read_at: Option<i64> = row.get("read_at");
            unread += usize::from(read_at.is_none());
            let body = self.unseal(&row.get::<Vec<u8>, _>("body"))?;
            notices.push(
                json!({"id":row.get::<i64,_>("id"),"kind":row.get::<String,_>("kind"),
                "severity":row.get::<String,_>("severity"),"message":body["message"],
                "created_at":row.get::<i64,_>("created_at"),"read_at":read_at}),
            );
        }
        Ok(json!({"notices": notices, "unread": unread}))
    }

    /// Mark one notification, or every unread one when `id` is `None`, as read.
    pub(crate) async fn mark_notices_read(&self, id: Option<i64>) -> Result<u64> {
        let now = webagent_host_core::now() as i64;
        let result = match id {
            Some(id) => {
                sqlx::query("UPDATE user_notices SET read_at=? WHERE id=? AND read_at IS NULL")
                    .bind(now)
                    .bind(id)
                    .execute(&self.pool)
                    .await?
            }
            None => {
                sqlx::query("UPDATE user_notices SET read_at=? WHERE read_at IS NULL")
                    .bind(now)
                    .execute(&self.pool)
                    .await?
            }
        };
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn notices_are_listed_newest_first_and_marked_read() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .add_notice("storage", "warning", "Storage 80% full")
            .await
            .unwrap();
        store
            .add_notice("storage", "critical", "Storage full")
            .await
            .unwrap();
        assert!(store.add_notice("storage", "loud", "x").await.is_err());
        let page = store.notices().await.unwrap();
        assert_eq!(page["unread"], 2);
        assert_eq!(page["notices"][0]["message"], "Storage full");
        let first = page["notices"][1]["id"].as_i64().unwrap();
        assert_eq!(store.mark_notices_read(Some(first)).await.unwrap(), 1);
        assert_eq!(store.notices().await.unwrap()["unread"], 1);
        assert_eq!(store.mark_notices_read(None).await.unwrap(), 1);
        assert_eq!(store.notices().await.unwrap()["unread"], 0);
    }
}
