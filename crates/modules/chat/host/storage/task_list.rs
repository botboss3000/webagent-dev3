//! Session task list storage: the current list under `base_tasks:{sid}` and every revision
//! under `base_tasks:{sid}:r{revision}`, each an encrypted, compressed user record carrying
//! its `session_id`, so the session's deletion erases them. Writes are revision-fenced and a
//! retried call returns its first result through the call's receipt.
use super::*;

/// Encoded size limit of one list revision.
const MAX_LIST_BYTES: usize = 64 * 1024;

fn list_key(sid: &str) -> Result<String> {
    if sid.is_empty()
        || sid.len() > 128
        || !sid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(anyhow!("Invalid task list session"));
    }
    Ok(format!("base_tasks:{sid}"))
}

impl Store {
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
    ) -> Result<Value> {
        let key = list_key(sid)?;
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
        let revision = expected_revision
            .checked_add(1)
            .filter(|next| list["revision"] == json!(next))
            .ok_or_else(|| anyhow!("Task list revision does not follow the expected one"))?;
        let receipt_key = format!("receipt:base:{sid}:{rid}:{pending_id}:{receipt}");
        let fingerprint = crate::digest(&json!([expected_revision, list, changes]));
        let _guard = self.context_gate.write().await;
        let mut tx = self.pool.begin().await?;
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
        let stored = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&key)
            .fetch_optional(&mut *tx)
            .await?
            .map(|raw| self.unseal(&raw))
            .transpose()?
            .and_then(|document| document["revision"].as_u64())
            .unwrap_or(0);
        if stored != expected_revision {
            return Err(anyhow!(
                "The task list changed (now revision {stored}); read it and write again"
            ));
        }
        let document = json!({"schema_version":1,"session_id":sid,"revision":revision,
            "updated_by_run":rid,"updated_at":crate::now(),"changes":changes,"list":list});
        let sealed = self.encode_user_record(&document)?;
        if sealed.len() > MAX_LIST_BYTES {
            return Err(anyhow!("The task list would exceed 64 KiB"));
        }
        for key in [key.clone(), format!("{key}:r{revision:010}")] {
            sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
                .bind(key)
                .bind(&sealed)
                .execute(&mut *tx)
                .await?;
        }
        let output = json!({"revision":revision,"changes":changes});
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(&receipt_key)
            .bind(self.encode_user_record(&json!({"fingerprint":fingerprint,"output":output}))?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(output)
    }

    /// Task list context for one model call of `requester` in session `sid` at history
    /// `basis`: the snapshot definition `(id, body)`, frozen until `basis` changes at a rewind
    /// or block merge (later changes reach the history as tool results), and the lines for
    /// the step's guidance message: the current one-line task state, then the reminder to
    /// file work when `calls` tool calls since the user's last message included no list write
    /// (`filed`). Shared chat members get none of it.
    pub(crate) async fn task_list_context(
        &self,
        sid: &str,
        requester: &str,
        basis: &str,
        calls: u64,
        filed: bool,
    ) -> Result<(Vec<(String, String)>, Vec<String>)> {
        if requester != self.current_user_id() {
            return Ok((Vec::new(), Vec::new()));
        }
        let key = list_key(sid)?;
        let list: webagent_chat::task_list::List = match self.get(&key).await? {
            Some(document) if document["session_id"] == sid => {
                serde_json::from_value(document["list"].clone())?
            }
            _ => webagent_chat::task_list::List::default(),
        };
        let texts = webagent_chat::task_list::texts();
        let view_key = format!("{key}:view");
        let saved = self.get(&view_key).await?;
        let view = match saved {
            Some(view) if view["session_id"] == sid && view["history_basis"] == basis => view,
            _ => {
                let view = json!({"session_id":sid,"history_basis":basis,
                    "revision":list.revision,
                    "snapshot":webagent_chat::task_list::render_snapshot(&list, &texts)});
                self.put(&view_key, &view).await?;
                view
            }
        };
        let id = format!("session-tasks-{}", view["revision"].as_u64().unwrap_or(0));
        let snapshot = view["snapshot"].as_str().unwrap_or("").to_owned();
        let lines = [
            webagent_chat::task_list::step_line(&list, &texts),
            webagent_chat::task_list::unfiled_reminder(calls, filed, &texts),
        ];
        Ok((vec![(id, snapshot)], lines.into_iter().flatten().collect()))
    }

    /// The end-of-turn reminder for `requester` in session `sid` when its task list still
    /// has open steps, or `None` (also for shared chat members).
    pub(crate) async fn task_list_end_reminder(
        &self,
        sid: &str,
        requester: &str,
    ) -> Result<Option<String>> {
        if requester != self.current_user_id() {
            return Ok(None);
        }
        let list: webagent_chat::task_list::List = match self.get(&list_key(sid)?).await? {
            Some(document) if document["session_id"] == sid => {
                serde_json::from_value(document["list"].clone())?
            }
            _ => return Ok(None),
        };
        Ok(webagent_chat::task_list::end_reminder(
            &list,
            &webagent_chat::task_list::texts(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            .task_list_write("owned", "r", "call-1", "g", "tasks", 0, &list(1), &changes)
            .await
            .unwrap();
        assert_eq!(first["revision"], 1);
        // A retry of the same call replays its result; different input under it is refused.
        let replay = store
            .task_list_write("owned", "r", "call-1", "g", "tasks", 0, &list(1), &changes)
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
                &json!([])
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
            .task_list_write("owned", "r", "call-2", "g", "tasks", 0, &list(1), &changes)
            .await
            .unwrap_err();
        assert!(stale.to_string().contains("now revision 1"), "{stale}");
        store
            .task_list_write("owned", "r", "call-2", "g", "tasks", 1, &list(2), &changes)
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
        let user = store.current_user_id().to_owned();
        let (first, _) = store
            .task_list_context("owned", &user, "b1", 0, false)
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
            .task_list_write("owned", "r", "call-3", "g", "tasks", 2, &list(3), &changes)
            .await
            .unwrap();
        let (same, _) = store
            .task_list_context("owned", &user, "b1", 0, false)
            .await
            .unwrap();
        assert_eq!(same, first);
        let (rebuilt, _) = store
            .task_list_context("owned", &user, "b2", 0, false)
            .await
            .unwrap();
        assert_eq!(rebuilt[0].0, "session-tasks-3");
        let (member, lines) = store
            .task_list_context("owned", "someone-else", "b2", 9, false)
            .await
            .unwrap();
        assert!(member.is_empty() && lines.is_empty());
        let (_, lines) = store
            .task_list_context("owned", &user, "b2", 9, false)
            .await
            .unwrap();
        assert_eq!(
            lines.len(),
            1,
            "an empty list still asks to file work: {lines:?}"
        );
        assert!(store
            .task_list_end_reminder("owned", &user)
            .await
            .unwrap()
            .is_none());
    }
}
