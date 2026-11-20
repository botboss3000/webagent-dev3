//! Trim stored tool calls to each tool's `retention` (`tool.json`) once the conversation has
//! moved past them: rows behind the compaction fold, and every row of a session idle for the
//! storage policy's `trim_idle_days`. A read whose path a later call in the same pass wrote is
//! stale and trimmed as disposable. Model context ahead of the fold is never changed; an idle
//! session's next run rebuilds from the trimmed rows. `retention_marks` records how far each
//! session is trimmed, so a pass only reads new rows.
use super::*;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use webagent_abilities::{
    tool_definitions,
    tool_retention::{self, OutputLevel},
};

/// Each account is swept at most this often; the loop's cleanup worker calls every few seconds.
const SWEEP_INTERVAL: Duration = Duration::from_secs(600);
/// Rows read in one pass, so one pass holds the write lock briefly.
const ROWS_PER_PASS: usize = 256;
const DAY_SECONDS: i64 = 86_400;

fn last_sweeps() -> &'static Mutex<HashMap<String, Instant>> {
    static SWEEPS: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    SWEEPS.get_or_init(Default::default)
}

/// A tool call seen in the rows: its tool and the path a read or write names.
struct Call {
    name: String,
    path: Option<String>,
}

impl Store {
    /// Run the account's sweep when its interval has passed; returns the rows trimmed.
    pub(crate) async fn trim_history_when_due(&self) -> Result<u64> {
        {
            let mut sweeps = last_sweeps().lock().unwrap_or_else(|e| e.into_inner());
            if sweeps
                .get(&self.user_id)
                .is_some_and(|at| at.elapsed() < SWEEP_INTERVAL)
            {
                return Ok(0);
            }
            sweeps.insert(self.user_id.clone(), Instant::now());
        }
        let (trim_days, compress_days) = webagent_files::storage_policy::StoragePolicy::live()
            .map(|policy| (policy.trim_idle_days, policy.compress_idle_days))
            .unwrap_or((7, 7));
        let now = webagent_host_core::now() as i64;
        let trimmed = self.trim_history(trim_days, now).await?;
        // Repack after trimming, so trimmed rows are packed once.
        self.compression_upkeep(compress_days, now).await?;
        Ok(trimmed)
    }

    /// Trim every session's rows past its mark, up to its fold or, when idle, its last row.
    pub(crate) async fn trim_history(&self, idle_days: u32, now: i64) -> Result<u64> {
        let sessions = sqlx::query(
            "SELECT i.session_id AS sid,MAX(i.seq) AS last,MAX(i.created_at) AS active,
             COALESCE(m.through,0) AS mark FROM interactions i
             LEFT JOIN retention_marks m ON m.session_id=i.session_id
             GROUP BY i.session_id HAVING MAX(i.seq)>COALESCE(m.through,0)",
        )
        .fetch_all(&self.pool)
        .await?;
        let idle_before = now - i64::from(idle_days) * DAY_SECONDS;
        let mut trimmed = 0;
        for row in sessions {
            let sid: String = row.get("sid");
            let mut mark: i64 = row.get("mark");
            let idle = row.get::<i64, _>("active") <= idle_before;
            let target = if idle {
                row.get::<i64, _>("last")
            } else {
                match self.get(&format!("session:{sid}")).await? {
                    Some(session) => session["checkpoint_through"].as_i64().unwrap_or(0),
                    None => continue,
                }
            };
            while mark < target {
                let (rows, through) = self.trim_rows(&sid, mark, target).await?;
                trimmed += rows;
                mark = through;
            }
            if idle {
                self.context_cache
                    .lock()
                    .unwrap()
                    .invalidate(&self.context_cache_key(&sid));
            }
        }
        Ok(trimmed)
    }

    /// Trim up to `ROWS_PER_PASS` rows after `mark` through `target` in one transaction and
    /// move the mark; returns the rows changed and the new mark.
    async fn trim_rows(&self, sid: &str, mark: i64, target: i64) -> Result<(u64, i64)> {
        // Read then write: take the write lock first, as the other cleanups do.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let rows = sqlx::query(
            "SELECT seq,run_id,item FROM interactions WHERE session_id=? AND seq>? AND seq<=? ORDER BY seq LIMIT ?",
        )
        .bind(sid)
        .bind(mark)
        .bind(target)
        .bind(ROWS_PER_PASS as i64)
        .fetch_all(&mut *tx)
        .await?;
        let through = rows.last().map_or(target, |row| row.get::<i64, _>("seq"));
        let mut items = Vec::with_capacity(rows.len());
        for row in &rows {
            items.push((
                row.get::<i64, _>("seq"),
                row.get::<String, _>("run_id"),
                self.unseal(&row.get::<Vec<u8>, _>("item"))?,
            ));
        }
        // Calls in this pass by id, and reads a later write in this pass made stale.
        let mut calls = HashMap::new();
        let mut reads_by_path: HashMap<String, Vec<String>> = HashMap::new();
        let mut stale = HashSet::new();
        for (_, _, item) in &items {
            if item["type"] != "function_call" {
                continue;
            }
            let (Some(id), Some(name)) = (item["call_id"].as_str(), item["name"].as_str()) else {
                continue;
            };
            let path = tool_retention::call_path(&item["arguments"]);
            if let (Some(path), Some(definition)) = (&path, tool_definitions::get(name)) {
                if definition.is_read() {
                    reads_by_path
                        .entry(path.clone())
                        .or_default()
                        .push(id.to_owned());
                } else if let Some(reads) = reads_by_path.remove(path) {
                    stale.extend(reads);
                }
            }
            calls.insert(
                id.to_owned(),
                Call {
                    name: name.to_owned(),
                    path,
                },
            );
        }
        let mut changed = 0u64;
        for (seq, run_id, mut item) in items {
            let Some(id) = item["call_id"].as_str().map(str::to_owned) else {
                continue;
            };
            let replaced = match item["type"].as_str() {
                Some("function_call") => {
                    let retention = calls
                        .get(&id)
                        .and_then(|call| tool_definitions::get(&call.name))
                        .map(|definition| &definition.retention);
                    retention.and_then(|retention| {
                        tool_retention::trimmed_arguments(retention, &item["arguments"])
                            .map(|arguments| ("arguments", arguments))
                    })
                }
                Some("function_call_output") => {
                    if !calls.contains_key(&id) {
                        // The call was trimmed in an earlier pass: find it in its run.
                        if let Some(call) = self.earlier_call(&mut tx, sid, &run_id, &id).await? {
                            calls.insert(id.clone(), call);
                        }
                    }
                    let definition = calls
                        .get(&id)
                        .and_then(|call| tool_definitions::get(&call.name));
                    definition.and_then(|definition| {
                        let stale_read = definition.is_read()
                            && calls.get(&id).is_some_and(|call| call.path.is_some())
                            && stale.contains(&id);
                        let level = if stale_read {
                            OutputLevel::Disposable
                        } else {
                            definition.retention.output
                        };
                        tool_retention::trimmed_output(
                            &definition.retention,
                            level,
                            &item["output"],
                        )
                        .map(|output| ("output", output))
                    })
                }
                _ => None,
            };
            let Some((field, value)) = replaced else {
                continue;
            };
            item[field] = value;
            sqlx::query("UPDATE interactions SET item=? WHERE seq=? AND session_id=?")
                .bind(self.encode_user_record(&item)?)
                .bind(seq)
                .bind(sid)
                .execute(&mut *tx)
                .await?;
            changed += 1;
        }
        sqlx::query(
            "INSERT INTO retention_marks(session_id,through) VALUES(?,?)
             ON CONFLICT(session_id) DO UPDATE SET through=excluded.through",
        )
        .bind(sid)
        .bind(through)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok((changed, through))
    }

    /// The call `call_id` from its run's earlier rows, when a pass boundary split it from its output.
    async fn earlier_call(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        sid: &str,
        run_id: &str,
        call_id: &str,
    ) -> Result<Option<Call>> {
        let rows = sqlx::query(
            "SELECT item FROM interactions WHERE session_id=? AND run_id=? ORDER BY seq",
        )
        .bind(sid)
        .bind(run_id)
        .fetch_all(&mut **tx)
        .await?;
        for row in rows {
            let item = self.unseal(&row.get::<Vec<u8>, _>("item"))?;
            if item["type"] == "function_call" && item["call_id"] == call_id {
                return Ok(item["name"].as_str().map(|name| Call {
                    name: name.to_owned(),
                    path: tool_retention::call_path(&item["arguments"]),
                }));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn insert(store: &Store, run: &str, step: &str, item: Value) {
        let mut tx = store.pool.begin().await.unwrap();
        store
            .insert_interaction(&mut tx, "s", run, step, &item)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    async fn items(store: &Store) -> Vec<Value> {
        let rows = sqlx::query("SELECT item FROM interactions WHERE session_id='s' ORDER BY seq")
            .fetch_all(&store.pool)
            .await
            .unwrap();
        rows.iter()
            .map(|row| store.unseal(&row.get::<Vec<u8>, _>("item")).unwrap())
            .collect()
    }

    fn call(id: &str, name: &str, arguments: Value) -> Value {
        json!({"type":"function_call","call_id":id,"name":name,"arguments":arguments.to_string()})
    }

    fn output(id: &str, body: Value) -> Value {
        json!({"type":"function_call_output","call_id":id,"output":body.to_string()})
    }

    #[tokio::test]
    async fn rows_behind_the_fold_and_idle_sessions_are_trimmed_once() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        let long = "line\n".repeat(400);
        // file_list is disposable in its tool.json; an unknown tool keeps everything.
        insert(
            &store,
            "r1",
            "a",
            call("c1", "file_list", json!({"path":"/code"})),
        )
        .await;
        insert(
            &store,
            "r1",
            "b",
            output("c1", json!({"status":"ok","output":long})),
        )
        .await;
        insert(&store, "r1", "c", call("c2", "no_such_tool", json!({}))).await;
        insert(
            &store,
            "r1",
            "d",
            output("c2", json!({"status":"ok","output":long})),
        )
        .await;
        insert(
            &store,
            "r2",
            "a",
            call("c3", "file_list", json!({"path":"/code"})),
        )
        .await;
        insert(
            &store,
            "r2",
            "b",
            output("c3", json!({"status":"ok","output":long})),
        )
        .await;
        store
            .put(
                "session:s",
                &json!({"id":"s","checkpoint_input":[],"checkpoint_through":4}),
            )
            .await
            .unwrap();
        let now = webagent_host_core::now() as i64;
        // Active session: only the rows behind the fold.
        assert_eq!(store.trim_history(7, now).await.unwrap(), 1);
        let saved = items(&store).await;
        let first: Value = serde_json::from_str(saved[1]["output"].as_str().unwrap()).unwrap();
        assert_eq!(first["status"], "ok");
        assert!(first.get("output").is_none() && first["retention"].is_string());
        assert!(saved[3]["output"].as_str().unwrap().len() > 2_000);
        assert!(saved[5]["output"].as_str().unwrap().len() > 2_000);
        // A later pass reads nothing new; an idle session is trimmed to its last row.
        assert_eq!(store.trim_history(7, now).await.unwrap(), 0);
        assert_eq!(
            store.trim_history(7, now + 8 * DAY_SECONDS).await.unwrap(),
            1
        );
        let saved = items(&store).await;
        assert!(saved[5]["output"].as_str().unwrap().len() < 200);
        assert_eq!(
            store.trim_history(7, now + 8 * DAY_SECONDS).await.unwrap(),
            0
        );
    }
}
