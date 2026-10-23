//! Metadata-only lifecycle notifications. This is distinct from private chat events.
use super::*;
pub(crate) const RETENTION_SECONDS: i64 = 30 * 24 * 60 * 60;
const MAX_OUTBOX_ROWS: i64 = 1_000_000;
const MAX_STAGED_ROWS: i64 = 10_000;

pub(crate) async fn schema(pool: &SqlitePool, user: bool) -> Result<()> {
    if user {
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
    }
    Ok(())
}

impl Store {
    // Called only from the lifecycle owner's transaction. No caller payload is copied.
    pub(crate) async fn stage_notification(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        event_key: &str,
        kind: &str,
        sid: &str,
        rid: Option<&str>,
        status: Option<&str>,
    ) -> Result<()> {
        if !matches!(
            kind,
            "session.created"
                | "webhook.received"
                | "main_notes.updated"
                | "message.received"
                | "run.started"
                | "run.completed"
                | "run.failed"
                | "run.cancelled"
                | "run.stopped"
                | "run.waiting_approval"
                | "session.help_requested"
                | "session.help_cancelled"
                | "session.help_resolved"
        ) {
            return Err(anyhow!("Unsupported notification type"));
        }
        let event_id = format!("{}:{event_key}", self.user_id);
        // Keep a compact deduplication marker for the originating resource lifetime.
        let inserted =
            sqlx::query("INSERT OR IGNORE INTO notification_emitted(event_id) VALUES(?)")
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
        let agent=if kind=="main_notes.updated" {
            self.agent_id.clone()
        } else {
            let raw:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
                .bind(format!("session:{sid}")).fetch_one(&mut **tx).await?;
            self.unseal(&raw)?["agent_id"].as_str().unwrap_or("shared_default").to_owned()
        };
        let actor = if kind.starts_with("run.") {
            "runtime".to_owned()
        } else {
            self.user_id.clone()
        };
        let payload = json!({"schema_version":1,"status":status});
        sqlx::query("INSERT INTO notification_stage(event_id,type,session_id,run_id,agent_id,actor,payload,created_at)
            VALUES(?,?,?,?,?,?,?,?)")
            .bind(event_id).bind(kind).bind(sid).bind(rid).bind(agent).bind(actor)
            .bind(payload.to_string()).bind(crate::now() as i64).execute(&mut **tx).await?;
        Ok(())
    }

    pub(crate) async fn stage_run_notification(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        run: &Value,
    ) -> Result<()> {
        let sid = run["session_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Run has no session"))?;
        let rid = run["id"].as_str().ok_or_else(|| anyhow!("Run has no id"))?;
        let status = run["status"].as_str().unwrap_or("");
        let (kind, suffix) = match status {
            "completed" => ("run.completed", "terminal".to_owned()),
            "failed" => ("run.failed", "terminal".to_owned()),
            "cancelled" => ("run.cancelled", "terminal".to_owned()),
            "stopped" => ("run.stopped", "settled:stopped".to_owned()),
            "waiting_approval" => {
                let pending = run["pending"]["id"].clone();
                let batch: Vec<Value> = run["batch"]
                    .as_array()
                    .map(|rows| {
                        rows.iter()
                            .filter(|row| row["state"] == "waiting")
                            .map(|row| row["pending"]["id"].clone())
                            .collect()
                    })
                    .unwrap_or_default();
                // Checkpoint IDs identify repeated saves of the same wait. No tool data is exported.
                (
                    "run.waiting_approval",
                    crate::digest(&json!([pending, batch])),
                )
            }
            _ => return Ok(()),
        };
        self.stage_notification(
            tx,
            &format!("run:{rid}:{suffix}"),
            kind,
            sid,
            Some(rid),
            Some(status),
        )
        .await
    }

    // The durable application watermark and insertion share a transaction.
    // If acknowledgement crashes, retries below the watermark cannot recreate pruned events.
    pub(crate) async fn relay_notifications(&self) -> Result<usize> {
        let mut tx = self.app_pool.begin().await?;
        sqlx::query("INSERT OR IGNORE INTO notification_relay VALUES(?,0)")
            .bind(&self.user_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE notification_relay SET through_seq=through_seq WHERE source_user=?")
            .bind(&self.user_id)
            .execute(&mut *tx)
            .await?;
        let mut through: i64 =
            sqlx::query_scalar("SELECT through_seq FROM notification_relay WHERE source_user=?")
                .bind(&self.user_id)
                .fetch_one(&mut *tx)
                .await?;
        let rows = sqlx::query(
            "SELECT * FROM notification_stage WHERE source_seq>? ORDER BY source_seq LIMIT 200",
        )
        .bind(through)
        .fetch_all(&self.pool)
        .await?;
        for row in &rows {
            sqlx::query("INSERT INTO notification_outbox(event_id,type,session_id,run_id,agent_id,actor,payload,created_at,source_user,source_seq,retained_at)
                VALUES(?,?,?,?,?,?,?,?,?,?,?)")
                .bind(row.get::<String,_>("event_id")).bind(row.get::<String,_>("type"))
                .bind(row.get::<String,_>("session_id")).bind(row.get::<Option<String>,_>("run_id"))
                .bind(row.get::<String,_>("agent_id")).bind(row.get::<String,_>("actor"))
                .bind(row.get::<String,_>("payload")).bind(row.get::<i64,_>("created_at"))
                .bind(&self.user_id).bind(row.get::<i64,_>("source_seq")).bind(crate::now() as i64).execute(&mut *tx).await?;
            through = row.get("source_seq");
        }
        sqlx::query("UPDATE notification_relay SET through_seq=? WHERE source_user=?")
            .bind(through)
            .bind(&self.user_id)
            .execute(&mut *tx)
            .await?;
        self.prune_notification_tx(&mut tx, crate::now() as i64)
            .await?;
        tx.commit().await?;
        sqlx::query("DELETE FROM notification_stage WHERE source_seq<=?")
            .bind(through)
            .execute(&self.pool)
            .await?;
        Ok(rows.len())
    }

    async fn prune_notification_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        now: i64,
    ) -> Result<usize> {
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
            sqlx::query("UPDATE notification_retention SET pruned_through=max(pruned_through,?) WHERE singleton=1")
                .bind(through).execute(&mut **tx).await?;
            return Ok(deleted.rows_affected() as usize);
        }
        Ok(0)
    }

    pub(crate) async fn prune_notifications(&self, now: i64) -> Result<usize> {
        let mut tx = self.app_pool.begin().await?;
        sqlx::query(
            "UPDATE notification_retention SET pruned_through=pruned_through WHERE singleton=1",
        )
        .execute(&mut *tx)
        .await?;
        let removed = self.prune_notification_tx(&mut tx, now).await?;
        tx.commit().await?;
        Ok(removed)
    }

    pub(crate) async fn notification_page(&self, since: i64, limit: i64) -> Result<Value> {
        let mut tx = self.app_pool.begin().await?;
        let floor: i64 = sqlx::query_scalar(
            "SELECT pruned_through FROM notification_retention WHERE singleton=1",
        )
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
        let rows =
            sqlx::query("SELECT * FROM notification_outbox WHERE seq>? ORDER BY seq LIMIT ?")
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
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn source() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .put("session:s", &json!({"id":"s","agent_id":"shared_default"}))
            .await
            .unwrap();
        (root, store)
    }
    #[tokio::test]
    async fn stopped_and_cancelled_are_committed_once_with_no_payload_text() {
        let (_root, store) = source().await;
        for status in ["stopped", "cancelled"] {
            let run = json!({"id":"settled","session_id":"s","status":status,"error":"private","input":"private"});
            let mut tx = store.pool.begin().await.unwrap();
            store.stage_run_notification(&mut tx, &run).await.unwrap();
            tx.rollback().await.unwrap();
            for _ in 0..2 {
                let mut tx = store.pool.begin().await.unwrap();
                store.stage_run_notification(&mut tx, &run).await.unwrap();
                tx.commit().await.unwrap();
            }
        }
        store.relay_notifications().await.unwrap();
        let page = store.notification_page(0, 20).await.unwrap();
        let rows = page["events"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["type"], "run.stopped");
        assert_eq!(rows[1]["type"], "run.cancelled");
        assert!(!page.to_string().contains("private"));
    }

    #[tokio::test]
    async fn transactional_relay_deduplication_and_redaction() {
        let (_root, store) = source().await;
        let mut tx = store.pool.begin().await.unwrap();
        store
            .stage_notification(
                &mut tx,
                "message:r",
                "message.received",
                "s",
                Some("r"),
                None,
            )
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        assert_eq!(store.relay_notifications().await.unwrap(), 0);
        for _ in 0..2 {
            let mut tx = store.pool.begin().await.unwrap();
            store
                .stage_notification(
                    &mut tx,
                    "message:r",
                    "message.received",
                    "s",
                    Some("r"),
                    None,
                )
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }
        assert_eq!(store.relay_notifications().await.unwrap(), 1);
        let page = store.notification_page(0, 200).await.unwrap();
        assert_eq!(page["events"].as_array().unwrap().len(), 1);
        assert_eq!(page["events"][0]["type"], "message.received");
        assert_eq!(
            page["events"][0]["payload"],
            json!({"schema_version":1,"status":null})
        );
        // Simulate the crash window after canonical commit and before source acknowledgement.
        sqlx::query("INSERT INTO notification_stage VALUES(1,'admin:message:r','message.received','s','r','shared_default','admin','{}',0)")
            .execute(&store.pool).await.unwrap();
        assert_eq!(store.relay_notifications().await.unwrap(), 0);
    }
    #[tokio::test]
    async fn retention_reopen_and_no_resurrection() {
        let (root, store) = source().await;
        let mut tx = store.pool.begin().await.unwrap();
        store
            .stage_notification(&mut tx, "created:s", "session.created", "s", None, None)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        store.relay_notifications().await.unwrap();
        store
            .prune_notifications(crate::now() as i64 + RETENTION_SECONDS + 1)
            .await
            .unwrap();
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(
            reopened.notification_page(0, 20).await.unwrap()["error"],
            "cursor_expired"
        );
        assert!(reopened.notification_page(1, 20).await.unwrap()["events"]
            .as_array()
            .unwrap()
            .is_empty());
        sqlx::query("INSERT INTO notification_stage VALUES(1,'admin:created:s','session.created','s',NULL,'shared_default','admin','{}',0)")
            .execute(&reopened.pool).await.unwrap();
        assert_eq!(reopened.relay_notifications().await.unwrap(), 0);
        assert_eq!(
            reopened.notification_page(1, 20).await.unwrap()["latest_seq"],
            1
        );
    }
    #[tokio::test]
    async fn approval_checkpoint_and_terminal_deduplication() {
        let (_root, store) = source().await;
        let mut run = json!({"id":"r","session_id":"s","status":"waiting_approval","pending":{"id":"approval-1","arguments":{"secret":"excluded"}}});
        for _ in 0..2 {
            let mut tx = store.pool.begin().await.unwrap();
            store.stage_run_notification(&mut tx, &run).await.unwrap();
            tx.commit().await.unwrap();
        }
        run["pending"]["id"] = json!("approval-2");
        let mut tx = store.pool.begin().await.unwrap();
        store.stage_run_notification(&mut tx, &run).await.unwrap();
        tx.commit().await.unwrap();
        run["status"] = json!("completed");
        for _ in 0..2 {
            let mut tx = store.pool.begin().await.unwrap();
            store.stage_run_notification(&mut tx, &run).await.unwrap();
            tx.commit().await.unwrap();
        }
        store.relay_notifications().await.unwrap();
        let page = store.notification_page(0, 2).await.unwrap();
        assert_eq!(page["events"].as_array().unwrap().len(), 2);
        assert_eq!(page["has_more"], true);
        let tail = store
            .notification_page(page["next_seq"].as_i64().unwrap(), 2)
            .await
            .unwrap();
        assert_eq!(tail["events"][0]["type"], "run.completed");
        assert!(!tail.to_string().contains("secret"));
    }
}
