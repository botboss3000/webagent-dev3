//! Background compression upkeep for one account, run by the loop's cleanup worker with the
//! history trim: train the account's compression dictionary once it has enough records (and
//! again after they grow fourfold), and repack the history, messages and events of sessions
//! idle for the storage policy's `compress_idle_days` at `compression.idle_level` with the
//! current dictionary. `compress_marks` records each session's packing, so a session is
//! repacked again only after new rows or a new dictionary. Reads cost the same at any level.
use super::*;
use webagent_storage::dictionary;

/// Records needed before a first dictionary is trained.
const FIRST_TRAINING_ROWS: i64 = 1_000;
/// Samples read for training, newest first, each cut to this many bytes.
const SAMPLE_ROWS: i64 = 4_000;
const SAMPLE_BYTES: usize = 16 * 1024;
/// Rows read in one pass at most (a session is finished once started), in batches of
/// `BATCH` per transaction.
const PASS_ROWS: usize = 2_000;
const BATCH: i64 = 128;
const DAY_SECONDS: i64 = 86_400;
/// Tables a session's rows live in, with their row id and record column.
const TABLES: [(&str, &str); 3] = [
    ("interactions", "item"),
    ("chat", "content"),
    ("events", "payload"),
];

impl Store {
    fn dictionary_open(&self) -> impl Fn(&[u8]) -> Result<Vec<u8>> + '_ {
        let aad = dictionary::aad(&self.user_id);
        move |sealed| self.codec.unseal_bytes(&aad, sealed)
    }

    /// Train or retrain the account's dictionary when due; true when one was installed.
    pub(crate) async fn train_dictionary_when_due(&self, now: i64) -> Result<bool> {
        // The live policy turns compression on; without it nothing is packed.
        if !webagent_storage::compression::Compression::current().on() {
            return Ok(false);
        }
        let rows: i64 = sqlx::query_scalar(
            "SELECT (SELECT COUNT(*) FROM interactions)+(SELECT COUNT(*) FROM chat)",
        )
        .fetch_one(&self.pool)
        .await?;
        let due = match dictionary::marker(&self.root, &self.user_id) {
            None => rows >= FIRST_TRAINING_ROWS,
            Some(marker) => {
                let trained = marker["rows"].as_i64().unwrap_or(0);
                rows >= trained.saturating_mul(4) && rows - trained >= 2 * FIRST_TRAINING_ROWS
            }
        };
        if !due {
            return Ok(false);
        }
        let mut samples = Vec::new();
        for (sql, share) in [
            ("SELECT item FROM interactions ORDER BY seq DESC LIMIT ?", 3),
            ("SELECT content FROM chat ORDER BY seq DESC LIMIT ?", 1),
        ] {
            let blobs: Vec<Vec<u8>> = sqlx::query_scalar(sql)
                .bind(SAMPLE_ROWS * share / 4)
                .fetch_all(&self.pool)
                .await?;
            for blob in blobs {
                if let Ok(value) = self.unseal(&blob) {
                    let mut bytes = serde_json::to_vec(&value)?;
                    bytes.truncate(SAMPLE_BYTES);
                    samples.push(bytes);
                }
            }
        }
        let raw = tokio::task::spawn_blocking(move || {
            dictionary::train(&samples, dictionary::DICTIONARY_BYTES)
        })
        .await??;
        let aad = dictionary::aad(&self.user_id);
        dictionary::install(
            &self.root,
            &self.user_id,
            raw,
            rows as u64,
            now as u64,
            |bytes| self.codec.seal_bytes(&aad, bytes),
        )?;
        Ok(true)
    }

    /// Repack idle sessions' rows; returns the rows rewritten.
    pub(crate) async fn repack_idle_sessions(&self, idle_days: u32, now: i64) -> Result<u64> {
        let settings = webagent_storage::compression::Compression::current();
        if !settings.on() || self.plaintext_records()? {
            return Ok(0);
        }
        let current = dictionary::current(&self.root, &self.user_id, self.dictionary_open())
            .map(|id| dictionary::hex(&id))
            .unwrap_or_default();
        let idle_before = now - i64::from(idle_days) * DAY_SECONDS;
        let sessions = sqlx::query(
            "SELECT i.session_id AS sid,MAX(i.seq) AS last FROM interactions i
             LEFT JOIN compress_marks m ON m.session_id=i.session_id
             GROUP BY i.session_id
             HAVING MAX(i.created_at)<=? AND (m.session_id IS NULL OR m.dictionary<>? OR m.through<MAX(i.seq))",
        )
        .bind(idle_before)
        .bind(&current)
        .fetch_all(&self.pool)
        .await?;
        let (mut read, mut rewritten) = (0usize, 0u64);
        for row in sessions {
            if read >= PASS_ROWS {
                break;
            }
            let sid: String = row.get("sid");
            let last: i64 = row.get("last");
            for (table, column) in TABLES {
                let (rows, changed) = self.repack_rows(&sid, table, column, &settings).await?;
                read += rows;
                rewritten += changed;
            }
            sqlx::query(
                "INSERT INTO compress_marks(session_id,dictionary,through) VALUES(?,?,?)
                 ON CONFLICT(session_id) DO UPDATE SET dictionary=excluded.dictionary,through=excluded.through",
            )
            .bind(&sid)
            .bind(&current)
            .bind(last)
            .execute(&self.pool)
            .await?;
        }
        Ok(rewritten)
    }

    /// Rewrite one session's rows of `table` at the idle level; values are unchanged.
    /// Returns the rows read and the rows rewritten.
    async fn repack_rows(
        &self,
        sid: &str,
        table: &str,
        column: &str,
        settings: &webagent_storage::compression::Compression,
    ) -> Result<(usize, u64)> {
        let level = settings.idle_level;
        let mut cursor = 0i64;
        let (mut read, mut rewritten) = (0usize, 0u64);
        loop {
            let rows = sqlx::query(&format!(
                "SELECT seq,{column} AS value FROM {table} WHERE session_id=? AND seq>? ORDER BY seq LIMIT ?"
            ))
            .bind(sid)
            .bind(cursor)
            .bind(BATCH)
            .fetch_all(&self.pool)
            .await?;
            let Some(end) = rows.last().map(|row| row.get::<i64, _>("seq")) else {
                break;
            };
            let blobs: Vec<(i64, Vec<u8>)> = rows
                .iter()
                .map(|row| (row.get("seq"), row.get::<Vec<u8>, _>("value")))
                .collect();
            read += blobs.len();
            // Packing at a high level is CPU work: keep it off the async workers.
            let store = self.clone();
            let packed = tokio::task::spawn_blocking(move || {
                blobs
                    .into_iter()
                    .filter_map(|(seq, blob)| {
                        let value = store.unseal(&blob).ok()?;
                        let packed = store.encode_user_record_at(&value, Some(level)).ok()?;
                        (packed.len() < blob.len()).then_some((seq, blob, packed))
                    })
                    .collect::<Vec<_>>()
            })
            .await?;
            let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
            for (seq, old, new) in &packed {
                // A row changed since it was read keeps its newer value.
                rewritten += sqlx::query(&format!(
                    "UPDATE {table} SET {column}=? WHERE seq=? AND {column}=?"
                ))
                .bind(new)
                .bind(seq)
                .bind(old)
                .execute(&mut *tx)
                .await?
                .rows_affected();
            }
            tx.commit().await?;
            cursor = end;
        }
        Ok((read, rewritten))
    }

    /// Train and repack when due; called with the history trim.
    pub(crate) async fn compression_upkeep(&self, idle_days: u32, now: i64) -> Result<u64> {
        self.train_dictionary_when_due(now).await?;
        self.repack_idle_sessions(idle_days, now).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn idle_sessions_are_repacked_once_with_the_trained_dictionary() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        let mut tx = store.pool.begin().await.unwrap();
        for n in 0..1_100 {
            let item = json!({"type":"function_call_output","call_id":format!("call_{n}"),
                "output":format!("{{\"status\":\"ok\",\"path\":\"/code/src/module_{}.rs\",\"output\":\"{}\"}}", n % 40, "fn main() {}\\n".repeat(30))});
            store
                .insert_interaction(
                    &mut tx,
                    if n < 600 { "old" } else { "new" },
                    "r",
                    &format!("s{n}"),
                    &item,
                )
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
        let before: Vec<u8> = sqlx::query_scalar(
            "SELECT item FROM interactions WHERE session_id='old' ORDER BY seq LIMIT 1",
        )
        .fetch_one(&store.pool)
        .await
        .unwrap();
        let now = webagent_host_core::now() as i64;
        assert!(store.train_dictionary_when_due(now).await.unwrap());
        assert!(!store.train_dictionary_when_due(now).await.unwrap());
        // Nothing is idle yet; a week later both sessions are, and each is packed once.
        assert_eq!(store.repack_idle_sessions(7, now).await.unwrap(), 0);
        assert!(
            store
                .repack_idle_sessions(7, now + 8 * DAY_SECONDS)
                .await
                .unwrap()
                >= 1_000
        );
        assert_eq!(
            store
                .repack_idle_sessions(7, now + 8 * DAY_SECONDS)
                .await
                .unwrap(),
            0
        );
        let after: Vec<u8> = sqlx::query_scalar(
            "SELECT item FROM interactions WHERE session_id='old' ORDER BY seq LIMIT 1",
        )
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert!(after.len() < before.len());
        assert_eq!(
            store.unseal(&after).unwrap(),
            store.unseal(&before).unwrap()
        );
    }
}
