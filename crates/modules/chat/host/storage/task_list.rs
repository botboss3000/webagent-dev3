//! Session task list storage: the current list under `base_tasks:{sid}` and every revision
//! under `base_tasks:{sid}:r{revision}`, each an encrypted, compressed user record carrying
//! its `session_id`, so the session's deletion erases them. Writes, the agent's and the
//! user's, are revision-fenced and a retried request returns its first result through its
//! receipt. The user's changes wait in the current document until the agent receives them.
use super::*;

/// Encoded size limit of one list revision.
const MAX_LIST_BYTES: usize = 64 * 1024;
/// The user's changes the current document keeps for the agent; older ones drop first.
const MAX_USER_CHANGES: usize = 20;
/// Revisions one history read returns, newest first.
const HISTORY_LIMIT: i64 = 100;

fn list_key(sid: &str) -> Result<String> {
    if sid.is_empty()
        || sid.len() > 128
        || !sid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(anyhow!("Invalid task list session"));
    }
    Ok(format!("base_tasks:{sid}"))
}

/// Session `sid`'s counted user requests (`u1`, `u2`, ...), the task gate's input. Not
/// revisioned with the list, so a new message never makes the agent's write stale.
fn requests_key(sid: &str) -> Result<String> {
    Ok(format!("{}:labels", list_key(sid)?))
}

/// The requests record in `tx`, or an empty one.
async fn requests_in(
    store: &Store,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    sid: &str,
) -> Result<webagent_chat::task_list::Requests> {
    let raw = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
        .bind(requests_key(sid)?)
        .fetch_optional(&mut **tx)
        .await?;
    match raw.map(|raw| store.unseal(&raw)).transpose()? {
        Some(document) if document["session_id"] == sid => {
            Ok(serde_json::from_value(document["requests"].clone())?)
        }
        _ => Ok(webagent_chat::task_list::Requests::default()),
    }
}

/// The current task list in `tx`: its document (null before the first write) and list.
async fn list_in(
    store: &Store,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    sid: &str,
) -> Result<(Value, webagent_chat::task_list::List)> {
    let document = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
        .bind(list_key(sid)?)
        .fetch_optional(&mut **tx)
        .await?
        .map(|raw| store.unseal(&raw))
        .transpose()?
        .filter(|document| document["session_id"] == sid)
        .unwrap_or(Value::Null);
    let list = if document.is_null() {
        webagent_chat::task_list::List::default()
    } else {
        serde_json::from_value(document["list"].clone())?
    };
    Ok((document, list))
}

/// What the task gate did with one call.
pub(crate) enum TaskGate {
    Pass,
    /// It opened a section (this revision) and the call goes ahead.
    Opened(u64),
    /// The call is refused with this message.
    Refused(String),
}

/// Whether stored user messages become task gate requests: on in the app. Runtime test
/// fixtures switch it off because most tests count model calls exactly, and an unfiled
/// request adds the end-of-turn reminder's call.
pub(crate) static RECORD_MESSAGE_REQUESTS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

impl Store {
    /// [`Store::record_task_request`] for a user message as it is stored (see
    /// [`RECORD_MESSAGE_REQUESTS`]).
    pub(crate) async fn record_message_request(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        sid: &str,
        message_id: &str,
        text: &str,
    ) -> Result<()> {
        if !RECORD_MESSAGE_REQUESTS.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
        self.record_task_request(tx, sid, message_id, text).await
    }

    /// Count message `message_id` of session `sid` as the next user request, inside the
    /// transaction that stores the message; a message already counted is left alone.
    pub(crate) async fn record_task_request(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        sid: &str,
        message_id: &str,
        text: &str,
    ) -> Result<()> {
        let mut requests = requests_in(self, tx, sid).await?;
        if !requests.add(message_id, text, crate::now()) {
            return Ok(());
        }
        let document = json!({"schema_version":1,"session_id":sid,"requests":requests});
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(requests_key(sid)?)
            .bind(self.encode_user_record(&document)?)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    /// Session `sid`'s counted user requests.
    pub(crate) async fn task_requests(
        &self,
        sid: &str,
    ) -> Result<webagent_chat::task_list::Requests> {
        let mut tx = self.pool.begin().await?;
        let requests = requests_in(self, &mut tx, sid).await?;
        tx.commit().await?;
        Ok(requests)
    }

    /// The task gate for tool call `pending_id` of run `rid` (see
    /// [`webagent_chat::task_list::gate`]). `batch_writes` are the arguments of the
    /// `task_list_write` calls made in the same model step: the gate judges the list as
    /// those writes leave it (one that would be refused counts as not made), so a batch
    /// can file its work and make its change in parallel. Opening a section commits a
    /// revision attributed to the gate, which the agent receives with its next model call;
    /// a retried call returns its first answer through its receipt.
    pub(crate) async fn task_list_gate(
        &self,
        sid: &str,
        rid: &str,
        pending_id: &str,
        generation: &str,
        batch_writes: &[Value],
        every_tool: bool,
    ) -> Result<TaskGate> {
        let receipt_key = format!("receipt:base:{sid}:{rid}:{pending_id}:gate_tasks");
        let _guard = self.context_gate.write().await;
        // Read-then-write: take the write lock first, so a concurrent writer makes this wait
        // (busy_timeout) instead of failing with 'database is locked'.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&receipt_key)
            .fetch_optional(&mut *tx)
            .await?
        {
            let receipt = self.unseal(&raw)?;
            tx.commit().await?;
            return Ok(TaskGate::Opened(receipt["revision"].as_u64().unwrap_or(0)));
        }
        let (_, list) = list_in(self, &mut tx, sid).await?;
        let requests = requests_in(self, &mut tx, sid).await?;
        let texts = webagent_chat::task_list::texts();
        let mut judged = list.clone();
        for arguments in batch_writes {
            let mut write = arguments.clone();
            if let Some(fields) = write.as_object_mut() {
                for key in ["expected_revision", "fold_earlier", "fold_focus"] {
                    fields.remove(key);
                }
            }
            let applied = serde_json::from_value::<webagent_chat::task_list::Write>(write)
                .ok()
                .and_then(|write| {
                    webagent_chat::task_list::apply(&judged, write, crate::now()).ok()
                })
                .filter(|(next, _)| {
                    webagent_chat::task_list::unknown_requests(&judged, next, &requests).is_empty()
                });
            if let Some((next, _)) = applied {
                judged = next;
            }
        }
        let write = match webagent_chat::task_list::gate(&judged, &requests, &texts, every_tool) {
            webagent_chat::task_list::Gate::Pass => {
                tx.commit().await?;
                return Ok(TaskGate::Pass);
            }
            webagent_chat::task_list::Gate::Refuse(message) => {
                tx.commit().await?;
                return Ok(TaskGate::Refused(message));
            }
            webagent_chat::task_list::Gate::Open(write) => write,
        };
        self.base_run_fence(&mut tx, sid, rid, None, Some(generation))
            .await?;
        let (next, changes) = webagent_chat::task_list::apply(&list, write, crate::now())?;
        let marks = webagent_chat::task_list::marks(&list, &next);
        let output = self
            .task_list_commit(
                &mut tx,
                sid,
                list.revision,
                json!({"gate":rid}),
                &serde_json::to_value(&next)?,
                &json!(changes),
                &marks,
            )
            .await?;
        let revision = output["revision"].as_u64().unwrap_or(0);
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(&receipt_key)
            .bind(self.encode_user_record(&json!({"revision":revision}))?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(TaskGate::Opened(revision))
    }

    /// Session `sid`'s task list document at `revision` (the current one when `None`), read
    /// for run `rid` of that session; `None` before the first write or for an unknown revision.
    pub(crate) async fn task_list(
        &self,
        sid: &str,
        rid: &str,
        revision: Option<u64>,
    ) -> Result<Option<Value>> {
        let key = list_key(sid)?;
        let key = match revision {
            Some(revision) => format!("{key}:r{revision:010}"),
            None => key,
        };
        let mut tx = self.pool.begin().await?;
        self.base_run_fence(&mut tx, sid, rid, None, None).await?;
        let raw = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(key)
            .fetch_optional(&mut *tx)
            .await?;
        let document = raw.map(|raw| self.unseal(&raw)).transpose()?;
        tx.commit().await?;
        Ok(document)
    }

    /// Save `list` (already at the next revision) as session `sid`'s task list for the
    /// executing tool call `pending_id` of run `rid`, with `changes` naming what the write did.
    /// Fails unless the stored revision is `expected_revision`; `receipt` separates writes of
    /// different tools under one call.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn task_list_write(
        &self,
        sid: &str,
        rid: &str,
        pending_id: &str,
        generation: &str,
        receipt: &str,
        expected_revision: u64,
        list: &Value,
        changes: &Value,
        marks: &Value,
    ) -> Result<Value> {
        list_key(sid)?;
        if generation.is_empty()
            || generation.len() > 128
            || rid.is_empty()
            || rid.len() > 128
            || !rid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || pending_id.is_empty()
            || pending_id.len() > 128
            || !pending_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || !matches!(receipt, "tasks" | "plan_tasks")
        {
            return Err(anyhow!("Invalid task list write"));
        }
        let receipt_key = format!("receipt:base:{sid}:{rid}:{pending_id}:{receipt}");
        let fingerprint = crate::digest(&json!([expected_revision, list, changes, marks]));
        let _guard = self.context_gate.write().await;
        // Read-then-write: take the write lock first, so a concurrent writer makes this wait
        // (busy_timeout) instead of failing with 'database is locked'.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&receipt_key)
            .fetch_optional(&mut *tx)
            .await?
        {
            let receipt = self.unseal(&raw)?;
            if receipt["fingerprint"] != fingerprint {
                return Err(anyhow!("Task list call ID reused with different input"));
            }
            let output = receipt["output"].clone();
            tx.commit().await?;
            return Ok(output);
        }
        self.base_run_fence(&mut tx, sid, rid, Some(pending_id), Some(generation))
            .await?;
        let output = self
            .task_list_commit(
                &mut tx,
                sid,
                expected_revision,
                json!({"run":rid}),
                list,
                changes,
                marks,
            )
            .await?;
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(&receipt_key)
            .bind(self.encode_user_record(&json!({"fingerprint":fingerprint,"output":output}))?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(output)
    }

    /// Save `list` (already at the next revision) as session `sid`'s task list for the user's
    /// edit `request_id`, which the agent receives with its next model call. Fails unless the
    /// stored revision is `expected_revision`; a retried request returns its first result.
    pub(crate) async fn task_list_user_write(
        &self,
        sid: &str,
        request_id: &str,
        expected_revision: u64,
        list: &Value,
        changes: &Value,
        marks: &Value,
    ) -> Result<Value> {
        list_key(sid)?;
        if request_id.is_empty()
            || request_id.len() > 64
            || !request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(anyhow!("Invalid task list request"));
        }
        let receipt_key = format!("receipt:base:{sid}:user:{request_id}:tasks");
        let fingerprint = crate::digest(&json!([expected_revision, list, changes, marks]));
        let _guard = self.context_gate.write().await;
        // Read-then-write: take the write lock first, so a concurrent writer makes this wait
        // (busy_timeout) instead of failing with 'database is locked'.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&receipt_key)
            .fetch_optional(&mut *tx)
            .await?
        {
            let receipt = self.unseal(&raw)?;
            if receipt["fingerprint"] != fingerprint {
                return Err(anyhow!("Task list request ID reused with different input"));
            }
            let output = receipt["output"].clone();
            tx.commit().await?;
            return Ok(output);
        }
        let session = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(format!("session:{sid}"))
            .fetch_optional(&mut *tx)
            .await?
            .map(|raw| self.unseal(&raw))
            .transpose()?;
        if session.is_none_or(|session| session["id"] != sid) {
            return Err(anyhow!("Session unavailable"));
        }
        let output = self
            .task_list_commit(
                &mut tx,
                sid,
                expected_revision,
                json!("user"),
                list,
                changes,
                marks,
            )
            .await?;
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(&receipt_key)
            .bind(self.encode_user_record(&json!({"fingerprint":fingerprint,"output":output}))?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(output)
    }

    /// Write the next revision inside `tx`: the current document and its history copy, the
    /// user's waiting changes carried forward (with this one when `by` is the user), and an
    /// event so open chats re-read the session.
    async fn task_list_commit(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        sid: &str,
        expected_revision: u64,
        by: Value,
        list: &Value,
        changes: &Value,
        marks: &Value,
    ) -> Result<Value> {
        let key = list_key(sid)?;
        let revision = expected_revision
            .checked_add(1)
            .filter(|next| list["revision"] == json!(next))
            .ok_or_else(|| anyhow!("Task list revision does not follow the expected one"))?;
        let stored = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&key)
            .fetch_optional(&mut **tx)
            .await?
            .map(|raw| self.unseal(&raw))
            .transpose()?
            .unwrap_or(Value::Null);
        let current = stored["revision"].as_u64().unwrap_or(0);
        if current != expected_revision {
            return Err(anyhow!(
                "The task list changed (now revision {current}); read it and write again"
            ));
        }
        // A cited or set-aside label must name a counted request.
        let before: webagent_chat::task_list::List = if stored.is_null() {
            webagent_chat::task_list::List::default()
        } else {
            serde_json::from_value(stored["list"].clone())?
        };
        let after: webagent_chat::task_list::List = serde_json::from_value(list.clone())?;
        let requests = requests_in(self, tx, sid).await?;
        let unknown = webagent_chat::task_list::unknown_requests(&before, &after, &requests);
        if !unknown.is_empty() {
            return Err(anyhow!(
                "No request {}: cite only the u-labels the task list names",
                unknown.join(", ")
            ));
        }
        let mut waiting = stored["user_changes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        // The user's changes, and a section the task gate opened, reach the agent once each.
        if by == "user" {
            waiting.push(json!({"revision":revision,"changes":changes}));
        } else if by["gate"].is_string() {
            waiting.push(json!({"revision":revision,"changes":changes,"by":"gate"}));
        }
        let skip = waiting.len().saturating_sub(MAX_USER_CHANGES);
        let waiting: Vec<Value> = waiting.into_iter().skip(skip).collect();
        let updated_at = crate::now();
        let history = json!({"schema_version":1,"session_id":sid,"revision":revision,
            "updated_by":by,"updated_at":updated_at,"changes":changes,"marks":marks,"list":list});
        let mut document = history.clone();
        document["user_changes"] = json!(waiting);
        for (key, document) in [
            (key.clone(), &document),
            (format!("{key}:r{revision:010}"), &history),
        ] {
            let sealed = self.encode_user_record(document)?;
            if sealed.len() > MAX_LIST_BYTES {
                return Err(anyhow!("The task list would exceed 64 KiB"));
            }
            sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
                .bind(key)
                .bind(sealed)
                .execute(&mut **tx)
                .await?;
        }
        // Open chats re-read the session when its component revision moves.
        sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,'base_task_artifact',?)")
            .bind(sid)
            .bind(self.encode_user_record(&json!({"kind":"tasks","revision":revision}))?)
            .execute(&mut **tx)
            .await?;
        // The sections this write created, started or closed travel with its result, so
        // compaction finds task boundaries in the history itself.
        let mut output = json!({"revision":revision,"changes":changes});
        for (key, ids) in marks.as_object().into_iter().flatten() {
            output[key] = ids.clone();
        }
        Ok(output)
    }

    /// Session `sid`'s current task list document, or `None` before the first write.
    pub(crate) async fn task_list_current(&self, sid: &str) -> Result<Option<Value>> {
        Ok(self
            .get(&list_key(sid)?)
            .await?
            .filter(|document| document["session_id"] == sid))
    }

    /// Session `sid`'s task list revisions, newest first: each one's number, time, writer
    /// (`"user"` or the agent's run) and changes.
    pub(crate) async fn task_list_history(&self, sid: &str) -> Result<Vec<Value>> {
        let key = list_key(sid)?;
        let rows =
            sqlx::query("SELECT value FROM docs WHERE key>? AND key<? ORDER BY key DESC LIMIT ?")
                .bind(format!("{key}:r"))
                .bind(format!("{key}:s"))
                .bind(HISTORY_LIMIT)
                .fetch_all(&self.pool)
                .await?;
        let mut history = Vec::with_capacity(rows.len());
        for row in rows {
            let document = self.unseal(&row.get::<Vec<u8>, _>("value"))?;
            if document["session_id"] != sid {
                continue;
            }
            history.push(json!({"revision":document["revision"],
                "updated_at":document["updated_at"],
                "by":if document["updated_by"] == "user" {"user"} else {"agent"},
                "changes":document["changes"]}));
        }
        Ok(history)
    }

    /// Task list context for one model call in session `sid` at history
    /// `basis`: the snapshot definition `(id, body)`, frozen until `basis` changes at a rewind
    /// or block merge (later changes reach the history as tool results), and the lines for
    /// the step's guidance message: the current one-line task state, then the reminder to
    /// file work when `calls` tool calls since the user's last message included no list write
    /// (`filed`). The list is the session's, so every participant's runs get it.
    pub(crate) async fn task_list_context(
        &self,
        sid: &str,
        basis: &str,
        calls: u64,
        filed: bool,
    ) -> Result<(Vec<(String, String)>, Vec<String>, Vec<(u64, String)>)> {
        let key = list_key(sid)?;
        let document = self.task_list_current(sid).await?.unwrap_or(Value::Null);
        let list: webagent_chat::task_list::List = if document.is_null() {
            webagent_chat::task_list::List::default()
        } else {
            serde_json::from_value(document["list"].clone())?
        };
        let texts = webagent_chat::task_list::texts();
        let view_key = format!("{key}:view");
        let saved = self.get(&view_key).await?;
        let mut view = match saved.clone() {
            Some(view) if view["session_id"] == sid && view["history_basis"] == basis => view,
            _ => json!({"session_id":sid,"history_basis":basis,"revision":list.revision,
                "delivered":list.revision,
                "snapshot":webagent_chat::task_list::render_snapshot(&list, &texts)}),
        };
        // The user's changes after the snapshot reach the history once each, in order.
        let delivered = view["delivered"].as_u64().unwrap_or(0);
        let mut updates = Vec::new();
        for change in document["user_changes"].as_array().into_iter().flatten() {
            let revision = change["revision"].as_u64().unwrap_or(0);
            if revision <= delivered {
                continue;
            }
            let lines = change["changes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n");
            let heading_key = if change["by"] == "gate" {
                "gate_change_heading"
            } else {
                "user_change_heading"
            };
            let heading = texts[heading_key].as_str().unwrap_or("");
            updates.push((
                revision,
                format!(
                    "{}\n{lines}",
                    heading.replace("{revision}", &revision.to_string())
                ),
            ));
            view["delivered"] = json!(revision);
        }
        if saved.as_ref() != Some(&view) {
            self.put(&view_key, &view).await?;
        }
        let id = format!("session-tasks-{}", view["revision"].as_u64().unwrap_or(0));
        let snapshot = view["snapshot"].as_str().unwrap_or("").to_owned();
        let requests = self.task_requests(sid).await?;
        let lines = [
            webagent_chat::task_list::step_line(&list, &texts),
            webagent_chat::task_list::unfiled_line(&list, &requests, &texts),
            webagent_chat::task_list::unfiled_reminder(calls, filed, &texts),
        ];
        Ok((
            vec![(id, snapshot)],
            lines.into_iter().flatten().collect(),
            updates,
        ))
    }

    /// The end-of-turn reminder for session `sid` when its task list still has open steps
    /// or user requests are unfiled, or `None`.
    pub(crate) async fn task_list_end_reminder(&self, sid: &str) -> Result<Option<String>> {
        let list: webagent_chat::task_list::List = match self.get(&list_key(sid)?).await? {
            Some(document) if document["session_id"] == sid => {
                serde_json::from_value(document["list"].clone())?
            }
            _ => webagent_chat::task_list::List::default(),
        };
        let texts = webagent_chat::task_list::texts();
        let requests = self.task_requests(sid).await?;
        let lines: Vec<String> = [
            webagent_chat::task_list::end_reminder(&list, &texts),
            webagent_chat::task_list::end_unfiled(&list, &requests, &texts),
        ]
        .into_iter()
        .flatten()
        .collect();
        Ok((!lines.is_empty()).then(|| lines.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Requests get labels with their messages; the gate opens a section once (a retried
    /// call replays), passes while every request is filed, refuses a new unfiled one and
    /// delivers the opened section to the agent once; a citation must name a counted request.
    #[tokio::test]
    async fn the_gate_opens_once_and_refuses_unfiled_requests() {
        let (_root, app) = crate::runtime_tests::fixture_uninitialized().await;
        let store = &app.store;
        store
            .put(
                "session:s",
                &json!({"id":"s","agent_id":"shared_default","status":"active","created_at":1}),
            )
            .await
            .unwrap();
        store
            .put(
                "run:r",
                &json!({"id":"r","session_id":"s","agent_id":"shared_default",
                    "status":"running","generation":"g","lease_epoch":0,
                    "pending":{"id":"call-1","state":"requested"}}),
            )
            .await
            .unwrap();
        let record = |message: &'static str, text: &'static str| async move {
            let mut tx = store.pool.begin().await.unwrap();
            store
                .record_task_request(&mut tx, "s", message, text)
                .await
                .unwrap();
            tx.commit().await.unwrap();
        };
        record("user:a", "Fix the login redirect").await;
        record("user:a", "Fix the login redirect").await;
        assert_eq!(store.task_requests("s").await.unwrap().items.len(), 1);
        // A model call before the gate fixes the snapshot at revision 0, as in a session.
        let (_, lines, _) = store
            .task_list_context("s", "basis", 0, false)
            .await
            .unwrap();
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("Unfiled requests: u1")),
            "{lines:?}"
        );

        let TaskGate::Opened(revision) = store
            .task_list_gate("s", "r", "call-1", "g", &[], false)
            .await
            .unwrap()
        else {
            panic!("no open section opens one");
        };
        assert_eq!(revision, 1);
        assert!(matches!(
            store
                .task_list_gate("s", "r", "call-1", "g", &[], false)
                .await
                .unwrap(),
            TaskGate::Opened(1)
        ));
        assert!(matches!(
            store
                .task_list_gate("s", "r", "call-2", "g", &[], false)
                .await
                .unwrap(),
            TaskGate::Pass
        ));
        let (_, lines, updates) = store
            .task_list_context("s", "basis", 0, false)
            .await
            .unwrap();
        assert!(
            !lines.iter().any(|line| line.starts_with("Unfiled")),
            "{lines:?}"
        );
        assert_eq!(updates.len(), 1);
        assert!(
            updates[0].1.contains("task gate opened"),
            "{}",
            updates[0].1
        );

        record("user:b", "Also check mobile").await;
        let TaskGate::Refused(message) = store
            .task_list_gate("s", "r", "call-3", "g", &[], false)
            .await
            .unwrap()
        else {
            panic!("an unfiled request is refused while a section is open");
        };
        assert!(message.contains("u2 \"Also check mobile\""), "{message}");
        let (_, lines, _) = store
            .task_list_context("s", "basis", 0, false)
            .await
            .unwrap();
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("Unfiled requests: u2")),
            "{lines:?}"
        );
        assert!(store
            .task_list_end_reminder("s")
            .await
            .unwrap()
            .is_some_and(|text| text.contains("u2")));

        // Citing a request that was never counted is refused at commit.
        let document = store.task_list_current("s").await.unwrap().unwrap();
        let list: webagent_chat::task_list::List =
            serde_json::from_value(document["list"].clone()).unwrap();
        let write = |labels: serde_json::Value| -> webagent_chat::task_list::Write {
            serde_json::from_value(json!({"section":{"id":"s1","requests":labels}})).unwrap()
        };
        let (bad, changes) =
            webagent_chat::task_list::apply(&list, write(json!(["u9"])), 2).unwrap();
        let marks = webagent_chat::task_list::marks(&list, &bad);
        assert!(store
            .task_list_user_write("s", "bad-cite", 1, &json!(bad), &json!(changes), &marks)
            .await
            .is_err());
        let (good, changes) =
            webagent_chat::task_list::apply(&list, write(json!(["u2"])), 2).unwrap();
        let marks = webagent_chat::task_list::marks(&list, &good);
        store
            .task_list_user_write("s", "good-cite", 1, &json!(good), &json!(changes), &marks)
            .await
            .unwrap();
        assert!(matches!(
            store
                .task_list_gate("s", "r", "call-4", "g", &[], false)
                .await
                .unwrap(),
            TaskGate::Pass
        ));
    }

    /// A list write in the same model step counts as made, so the change in that step
    /// passes without the gate opening its own section; a write naming an unknown request
    /// or refused by the rules counts as not made.
    #[tokio::test]
    async fn the_gate_counts_the_steps_own_list_write() {
        let (_root, app) = crate::runtime_tests::fixture_uninitialized().await;
        let store = &app.store;
        store
            .put(
                "session:s",
                &json!({"id":"s","agent_id":"shared_default","status":"active","created_at":1}),
            )
            .await
            .unwrap();
        store
            .put(
                "run:r",
                &json!({"id":"r","session_id":"s","agent_id":"shared_default",
                    "status":"running","generation":"g","lease_epoch":0}),
            )
            .await
            .unwrap();
        let mut tx = store.pool.begin().await.unwrap();
        store
            .record_task_request(&mut tx, "s", "user:a", "Fix the login redirect")
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let filing = json!({"expected_revision":0,"section":{"title":"Fix login","requests":["u1"]},
            "steps":[{"label":"Patch the cookie path","status":"in_progress"}]});
        assert!(matches!(
            store
                .task_list_gate("s", "r", "call-1", "g", &[filing], false)
                .await
                .unwrap(),
            TaskGate::Pass
        ));
        assert!(store.task_list_current("s").await.unwrap().is_none());
        let unknown = json!({"expected_revision":0,"section":{"title":"Fix login","requests":["u9"]},
            "steps":[{"label":"Patch the cookie path","status":"in_progress"}]});
        assert!(matches!(
            store
                .task_list_gate(
                    "s",
                    "r",
                    "call-2",
                    "g",
                    &[unknown, json!({"bad":true})],
                    false
                )
                .await
                .unwrap(),
            TaskGate::Opened(1)
        ));
    }

    #[tokio::test]
    async fn writes_are_revision_fenced_replayed_and_kept_per_revision() {
        let (_root, app) = crate::runtime_tests::fixture_uninitialized().await;
        let store = &app.store;
        for sid in ["owned", "foreign"] {
            store
                .put(
                    &format!("session:{sid}"),
                    &json!({"id":sid,"agent_id":"shared_default","status":"active",
                        "created_at":1}),
                )
                .await
                .unwrap();
        }
        store
            .put(
                "run:r",
                &json!({"id":"r","session_id":"owned","agent_id":"shared_default",
                    "status":"running","generation":"g","lease_epoch":0,
                    "pending":{"id":"call-1","state":"executing"}}),
            )
            .await
            .unwrap();
        let list = |revision: u64| json!({"revision":revision,"sections":[],"next_section":0});
        let changes = json!(["s1 added: Work"]);
        let first = store
            .task_list_write(
                "owned",
                "r",
                "call-1",
                "g",
                "tasks",
                0,
                &list(1),
                &changes,
                &json!({}),
            )
            .await
            .unwrap();
        assert_eq!(first["revision"], 1);
        // A retry of the same call replays its result; different input under it is refused.
        let replay = store
            .task_list_write(
                "owned",
                "r",
                "call-1",
                "g",
                "tasks",
                0,
                &list(1),
                &changes,
                &json!({}),
            )
            .await
            .unwrap();
        assert_eq!(replay, first);
        assert!(store
            .task_list_write(
                "owned",
                "r",
                "call-1",
                "g",
                "tasks",
                0,
                &list(1),
                &json!([]),
                &json!({})
            )
            .await
            .is_err());
        // Another call must build on the stored revision.
        store
            .put(
                "run:r",
                &json!({"id":"r","session_id":"owned","agent_id":"shared_default",
                    "status":"running","generation":"g","lease_epoch":0,
                    "pending":{"id":"call-2","state":"executing"}}),
            )
            .await
            .unwrap();
        let stale = store
            .task_list_write(
                "owned",
                "r",
                "call-2",
                "g",
                "tasks",
                0,
                &list(1),
                &changes,
                &json!({}),
            )
            .await
            .unwrap_err();
        assert!(stale.to_string().contains("now revision 1"), "{stale}");
        store
            .task_list_write(
                "owned",
                "r",
                "call-2",
                "g",
                "tasks",
                1,
                &list(2),
                &changes,
                &json!({}),
            )
            .await
            .unwrap();
        let current = store.task_list("owned", "r", None).await.unwrap().unwrap();
        assert_eq!(
            (current["revision"].clone(), current["session_id"].clone()),
            (json!(2), json!("owned"))
        );
        let old = store
            .task_list("owned", "r", Some(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(old["revision"], 1);
        assert!(store
            .task_list("owned", "r", Some(7))
            .await
            .unwrap()
            .is_none());
        // The run belongs to another session there.
        assert!(store.task_list("foreign", "r", None).await.is_err());
        // The snapshot stays as it was until the history basis changes.
        let (first, _, _) = store
            .task_list_context("owned", "b1", 0, false)
            .await
            .unwrap();
        assert_eq!(first[0].0, "session-tasks-2");
        store
            .put(
                "run:r",
                &json!({"id":"r","session_id":"owned","agent_id":"shared_default",
                    "status":"running","generation":"g","lease_epoch":0,
                    "pending":{"id":"call-3","state":"executing"}}),
            )
            .await
            .unwrap();
        store
            .task_list_write(
                "owned",
                "r",
                "call-3",
                "g",
                "tasks",
                2,
                &list(3),
                &changes,
                &json!({}),
            )
            .await
            .unwrap();
        let (same, _, _) = store
            .task_list_context("owned", "b1", 0, false)
            .await
            .unwrap();
        assert_eq!(same, first);
        let (rebuilt, _, _) = store
            .task_list_context("owned", "b2", 0, false)
            .await
            .unwrap();
        assert_eq!(rebuilt[0].0, "session-tasks-3");
        let (_, lines, _) = store
            .task_list_context("owned", "b2", 9, false)
            .await
            .unwrap();
        assert_eq!(
            lines.len(),
            1,
            "an empty list still asks to file work: {lines:?}"
        );
        // A user edit is fenced like the agent's, kept in history and delivered once.
        assert!(store
            .task_list_user_write(
                "owned",
                "edit-1",
                2,
                &list(4),
                &json!(["s1.2 added"]),
                &json!({})
            )
            .await
            .is_err());
        let edit = store
            .task_list_user_write(
                "owned",
                "edit-1",
                3,
                &list(4),
                &json!(["s1.2 added"]),
                &json!({}),
            )
            .await
            .unwrap();
        assert_eq!(edit["revision"], 4);
        let (_, _, updates) = store
            .task_list_context("owned", "b2", 0, false)
            .await
            .unwrap();
        assert_eq!(updates.len(), 1);
        assert!(updates[0].1.ends_with("s1.2 added"), "{:?}", updates[0]);
        let (_, _, again) = store
            .task_list_context("owned", "b2", 0, false)
            .await
            .unwrap();
        assert!(again.is_empty());
        // A panel edit updates only a plan an agent run already saved.
        assert!(store
            .session_artifact_user_write("owned", "edit-1", "plan", "plan", &json!({}))
            .await
            .is_err());
        let history = store.task_list_history("owned").await.unwrap();
        assert_eq!(history[0]["revision"], 4);
        assert_eq!(history[0]["by"], "user");
        assert_eq!(history.last().unwrap()["revision"], 1);
        assert!(store
            .task_list_end_reminder("owned")
            .await
            .unwrap()
            .is_none());
    }

    /// A task list write that reads before it writes must wait for a concurrent writer, not
    /// fail with "database is locked" when that writer commits between its read and write.
    #[tokio::test]
    async fn writes_wait_for_concurrent_writers_instead_of_failing() {
        let (_root, app) = crate::runtime_tests::fixture_uninitialized().await;
        let store = &app.store;
        store
            .put(
                "session:s",
                &json!({"id":"s","agent_id":"shared_default","status":"active","created_at":1}),
            )
            .await
            .unwrap();
        let list = |revision: u64| json!({"revision":revision,"sections":[],"next_section":0});
        let done = std::sync::atomic::AtomicBool::new(false);
        let noise = async {
            let mut n = 0u64;
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                store
                    .put("session_usage:noise", &json!({"n":n}))
                    .await
                    .unwrap();
                n += 1;
                tokio::task::yield_now().await;
            }
        };
        let edits = async {
            for revision in 0..60u64 {
                let request = format!("edit-{revision}");
                store
                    .task_list_user_write(
                        "s",
                        &request,
                        revision,
                        &list(revision + 1),
                        &json!(["s1 added: Work"]),
                        &json!({}),
                    )
                    .await
                    .unwrap();
            }
            done.store(true, std::sync::atomic::Ordering::Relaxed);
        };
        tokio::join!(noise, edits);
    }
}
