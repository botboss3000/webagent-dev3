//! User-owned declarative chat components and Base core records.
use super::*;

const MAX_ARTIFACT_BYTES: usize = 24 * 1024;
/// Undelivered panel notes kept per session; older ones drop first.
const MAX_PANEL_NOTES: usize = 8;

/// The tool call ID of pending call `pending_id` in `run`, alone or in a batch.
fn pending_call_id(run: &Value, pending_id: &str) -> Option<String> {
    let pending = if run["pending"]["id"] == pending_id {
        &run["pending"]
    } else {
        run["batch"]
            .as_array()?
            .iter()
            .map(|item| &item["pending"])
            .find(|pending| pending["id"] == pending_id)?
    };
    pending["call_id"].as_str().map(str::to_owned)
}

fn artifact_key(sid: &str, kind: &str, id: &str) -> Result<String> {
    if sid.is_empty()
        || sid.len() > 128
        || !sid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(anyhow!("Invalid task artifact identifier"));
    }
    match kind {
        "component" => Ok(format!("base_component:{sid}:{id}")),
        "core_profile" => Ok(format!("core_profile:{id}")),
        "core_rating" => Ok(format!("core_rating:{id}")),
        "core_webhook" => Ok(format!("core_webhook:{id}")),
        _ => Err(anyhow!("Invalid task artifact kind")),
    }
}

impl Store {
    // Stable pending-call receipts make recovery retries harmless even after
    // the run completes. The receipt, artifact and event commit together.
    pub async fn base_write_artifact(
        &self,
        sid: &str,
        rid: &str,
        pending_id: &str,
        generation: &str,
        kind: &str,
        id: &str,
        expected_revision: u64,
        artifact: &Value,
    ) -> Result<Value> {
        let key = artifact_key(sid, kind, id)?;
        if generation.is_empty()
            || generation.len() > 128
            || rid.is_empty()
            || rid.len() > 128
            || pending_id.is_empty()
            || pending_id.len() > 128
            || !rid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || !pending_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || serde_json::to_vec(artifact)?.len() > MAX_ARTIFACT_BYTES
        {
            return Err(anyhow!("Task artifact exceeds bounds"));
        }
        let receipt_key = format!("receipt:base:{sid}:{rid}:{pending_id}");
        let fingerprint = crate::digest(&json!([kind, id, expected_revision, artifact]));
        let _guard = self.context_gate.write().await;
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE docs SET key=key WHERE key=?")
            .bind(format!("session:{sid}"))
            .execute(&mut *tx)
            .await?;
        if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&receipt_key)
            .fetch_optional(&mut *tx)
            .await?
        {
            let receipt = self.unseal(&raw)?;
            if receipt["fingerprint"] != fingerprint {
                return Err(anyhow!("Task call ID reused with different input"));
            }
            let output = receipt["output"].clone();
            tx.commit().await?;
            return Ok(output);
        }
        self.base_run_fence(&mut tx, sid, rid, Some(pending_id), Some(generation))
            .await?;
        let prior = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&key)
            .fetch_optional(&mut *tx)
            .await?
            .map(|raw| self.unseal(&raw))
            .transpose()?;
        let revision = prior
            .as_ref()
            .and_then(|value| value["revision"].as_u64())
            .unwrap_or(0);
        if revision != expected_revision {
            return Err(anyhow!(
                "Task artifact revision changed; reload before editing"
            ));
        }
        let next = revision
            .checked_add(1)
            .ok_or_else(|| anyhow!("Task revision exhausted"))?;
        // The first write's time orders panels as the agent created them.
        let created_at = prior
            .as_ref()
            .and_then(|value| value["created_at"].as_u64())
            .unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_millis() as u64)
            });
        // The panel follows its run's newest published row when it first appeared (the
        // message the panel belongs to), never another run's or the user's row.
        let after_seq = match prior.as_ref().and_then(|value| value["after_seq"].as_i64()) {
            Some(seq) => seq,
            None => {
                sqlx::query_scalar::<_, i64>(
                    "SELECT COALESCE(MAX(seq),0) FROM chat WHERE session_id=? AND run_id=?",
                )
                .bind(sid)
                .bind(rid)
                .fetch_one(&mut *tx)
                .await?
            }
        };
        // A panel stays linked to the call that first drew it: its run and tool call.
        let origin = match prior.as_ref().map(|value| &value["origin"]) {
            Some(origin) if origin.is_object() => origin.clone(),
            _ if kind == "component" => {
                let call_id =
                    sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
                        .bind(format!("run:{rid}"))
                        .fetch_optional(&mut *tx)
                        .await?
                        .map(|raw| self.unseal(&raw))
                        .transpose()?
                        .and_then(|run| pending_call_id(&run, pending_id));
                json!({"run_id":rid,"call_id":call_id})
            }
            _ => Value::Null,
        };
        let document = json!({"schema_version":1,"revision":next,"kind":kind,
            "id":id,"session_id":sid,"updated_by_run":rid,"created_at":created_at,
            "after_seq":after_seq,"origin":origin,"artifact":artifact});
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(&key).bind(if kind=="core_webhook"{self.seal(&document)?}else{self.encode_user_record(&document)?}).execute(&mut *tx).await?;
        let output = json!({"revision":next,"kind":kind,"id":id,"artifact":artifact});
        let receipt = json!({"fingerprint":fingerprint,"output":output});
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(&receipt_key)
            .bind(if kind == "core_webhook" {
                self.seal(&receipt)?
            } else {
                self.encode_user_record(&receipt)?
            })
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,'base_task_artifact',?)")
            .bind(sid)
            .bind(self.encode_user_record(&json!({"kind":kind,"id":id,"revision":next}))?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(output)
    }

    pub(crate) async fn base_component_revision(&self, sid: &str) -> Result<u64> {
        let seq:i64=sqlx::query_scalar("SELECT COALESCE(MAX(seq),0) FROM events WHERE session_id=? AND kind='base_task_artifact'").bind(sid).fetch_one(&self.pool).await?;
        Ok(seq.max(0) as u64)
    }

    /// Reply panel `id` of session `sid` as stored, or `None`.
    pub async fn base_component(&self, sid: &str, id: &str) -> Result<Option<Value>> {
        let key = artifact_key(sid, "component", id)?;
        let document = self.get(&key).await?;
        Ok(document.filter(|document| document["session_id"] == sid))
    }

    /// The stored reply panel `id` marked answered by the message of run `message_run`,
    /// for the caller to write in the same transaction as that message. Fails unless the
    /// panel is still open at `revision` and `answer` fits it; the first answer stands.
    pub async fn base_component_answered(
        &self,
        sid: &str,
        id: &str,
        revision: u64,
        answer: &Value,
        message_run: &str,
    ) -> Result<(String, Value)> {
        let key = artifact_key(sid, "component", id)?;
        if !answer.is_object() || serde_json::to_vec(answer)?.len() > 16 * 1024 {
            return Err(anyhow!("Invalid panel answer"));
        }
        let mut document = self
            .base_component(sid, id)
            .await?
            .ok_or_else(|| anyhow!("Panel not found"))?;
        let card = &document["artifact"];
        if card["authority"] != "user_reply" || card["lifecycle"]["dismissed"] == true {
            return Err(anyhow!("Only open choice and form panels take answers"));
        }
        if card["lifecycle"]["user_state"].is_string() {
            return Err(anyhow!("Panel already answered"));
        }
        if document["revision"].as_u64() != Some(revision) {
            return Err(anyhow!("Panel changed; reload it before answering"));
        }
        webagent_native_tools::task::answer_text(card, answer)?;
        document["revision"] = json!(revision + 1);
        document["artifact"]["lifecycle"] = json!({"user_state":"answered","answer":answer,
            "message_run_id":message_run});
        Ok((key, document))
    }

    /// Announce a panel change written with a message, so open chats redraw it.
    pub async fn base_component_changed(&self, sid: &str, id: &str, revision: u64) -> Result<()> {
        sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,'base_task_artifact',?)")
            .bind(sid)
            .bind(
                self.encode_user_record(&json!({"kind":"component","id":id,"revision":revision}))?,
            )
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Records the dismissal of choice or form panel `id`, or, for a server-built panel
    /// whose answer sends no message, `answer`. The panel stays in the chat, read-only, so
    /// the outcome survives reload, and `note` waits for the agent's next message. Only
    /// `user_reply` panels qualify; returns whether one was found.
    pub async fn base_component_settle(
        &self,
        sid: &str,
        id: &str,
        answer: &Value,
        note: &str,
    ) -> Result<bool> {
        let key = artifact_key(sid, "component", id)?;
        if !(answer.is_null() || answer.is_object()) || serde_json::to_vec(answer)?.len() > 8 * 1024
        {
            return Err(anyhow!("Invalid panel answer"));
        }
        let notes_key = format!("panel_notes:{sid}");
        let _guard = self.context_gate.write().await;
        // Read-then-write: take the write lock first, so a concurrent writer makes this wait
        // (busy_timeout) instead of failing with 'database is locked'.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&key)
            .fetch_optional(&mut *tx)
            .await?
        else {
            return Ok(false);
        };
        let mut document = self.unseal(&raw)?;
        let card = &document["artifact"];
        if document["session_id"] != sid || card["authority"] != "user_reply" {
            return Err(anyhow!("Only choice and form panels can be dismissed"));
        }
        if card["type"] == "choice"
            && !answer.is_null()
            && !card["data"]["options"]
                .as_array()
                .is_some_and(|options| options.iter().any(|o| o["id"] == answer["option_id"]))
        {
            return Err(anyhow!("Unknown panel option"));
        }
        if card["lifecycle"]["user_state"].is_string() {
            return Ok(true); // Already answered or dismissed; the first answer stands.
        }
        if !answer.is_null() && card["server_action"].is_null() {
            return Err(anyhow!("Answers to agent panels are sent as messages"));
        }
        let next = document["revision"].as_u64().unwrap_or(0) + 1;
        document["revision"] = json!(next);
        document["artifact"]["lifecycle"] = if answer.is_null() {
            json!({"user_state":"dismissed"})
        } else {
            json!({"user_state":"answered","answer":answer})
        };
        sqlx::query("UPDATE docs SET value=? WHERE key=?")
            .bind(self.encode_user_record(&document)?)
            .bind(&key)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,'base_task_artifact',?)")
            .bind(sid)
            .bind(self.encode_user_record(&json!({"kind":"component","id":id,"revision":next}))?)
            .execute(&mut *tx)
            .await?;
        // The agent learns of the outcome with the next message it receives.
        let mut notes = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&notes_key)
            .fetch_optional(&mut *tx)
            .await?
            .map(|raw| self.unseal(&raw))
            .transpose()?
            .and_then(|value| value["notes"].as_array().cloned())
            .unwrap_or_default();
        notes.push(json!(note));
        let skip = notes.len().saturating_sub(MAX_PANEL_NOTES);
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(&notes_key)
            .bind(self.encode_user_record(&json!({"notes":notes[skip..]}))?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .invalidate(&[key, notes_key]);
        Ok(true)
    }

    // Safe API projection: only previously validated declarative specs.
    pub async fn base_chat_components(&self, sid: &str) -> Result<Vec<Value>> {
        // Refuse an invalid session ID before it reaches the key pattern below.
        artifact_key(sid, "component", "list")?;
        let mut tx = self.pool.begin().await?;
        let session_raw = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(format!("session:{sid}"))
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| anyhow!("Session unavailable"))?;
        if self.unseal(&session_raw)?["id"] != sid {
            return Err(anyhow!("Session unavailable"));
        }
        let rows = sqlx::query("SELECT value FROM docs WHERE key LIKE ? ORDER BY key LIMIT 65")
            .bind(format!("base_component:{sid}:%"))
            .fetch_all(&mut *tx)
            .await?;
        if rows.len() > 64 {
            return Err(anyhow!("Chat component count exceeds limit"));
        }
        let mut components = Vec::with_capacity(rows.len());
        for row in rows {
            let document = self.unseal(&row.get::<Vec<u8>, _>("value"))?;
            if document["schema_version"] != 1
                || document["session_id"] != sid
                || document["kind"] != "component"
            {
                return Err(anyhow!("Invalid stored chat component"));
            }
            // The run that drew the card anchors it after that run's reply; the server's own
            // action stays private.
            let mut component = document["artifact"].clone();
            if let Some(card) = component.as_object_mut() {
                card.remove("server_action");
            }
            let origin = &document["origin"];
            components.push(json!({"revision":document["revision"],
                "run_id":origin["run_id"].as_str().map_or(&document["updated_by_run"], |_| &origin["run_id"]),
                "call_id":origin["call_id"],"created_at":document["created_at"],
                "after_seq":document["after_seq"],"component":component}));
        }
        tx.commit().await?;
        components.sort_by_key(|row| row["created_at"].as_u64().unwrap_or(0));
        Ok(components)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn artifact_cas_replay_and_run_fences() {
        let (_root, app) = crate::runtime_tests::fixture_uninitialized().await;
        let store = &app.store;
        for sid in ["owned", "foreign"] {
            store
                .put(
                    &format!("session:{sid}"),
                    &json!({"id":sid,"agent_id":"shared_default",
                "status":"active","created_at":1}),
                )
                .await
                .unwrap();
        }
        let running = |sid: &str, state: &str| {
            json!({"id":"r","session_id":sid,
            "agent_id":"shared_default","status":state,"generation":"g",
            "lease_epoch":0,"pending":{"id":"call-1","state":"executing"}})
        };
        store
            .put("run:r", &running("owned", "running"))
            .await
            .unwrap();
        let artifact = json!({"title":"Choose","fields":[]});
        let first = store
            .base_write_artifact(
                "owned",
                "r",
                "call-1",
                "g",
                "component",
                "card",
                0,
                &artifact,
            )
            .await
            .unwrap();
        assert_eq!(first["revision"], 1);
        assert_eq!(
            store
                .base_component("owned", "card")
                .await
                .unwrap()
                .unwrap()["revision"],
            1
        );
        assert_eq!(
            store
                .base_write_artifact(
                    "owned",
                    "r",
                    "call-1",
                    "g",
                    "component",
                    "card",
                    0,
                    &artifact
                )
                .await
                .unwrap(),
            first
        );
        assert!(store
            .base_write_artifact(
                "owned",
                "r",
                "call-1",
                "g",
                "component",
                "card",
                0,
                &json!({"different":true})
            )
            .await
            .is_err());
        // The next dispatched call has a fresh ID but must supply revision 1.
        let mut next_run = running("owned", "running");
        next_run["pending"]["id"] = json!("call-2");
        store.put("run:r", &next_run).await.unwrap();
        assert!(store
            .base_write_artifact(
                "owned",
                "r",
                "call-2",
                "g",
                "component",
                "card",
                0,
                &artifact
            )
            .await
            .is_err());
        assert!(store
            .base_write_artifact(
                "owned",
                "r",
                "call-2",
                "old-generation",
                "component",
                "card",
                1,
                &artifact
            )
            .await
            .is_err());
        store
            .put("run:r", &running("owned", "completed"))
            .await
            .unwrap();
        // A committed write replays after completion; a new write is fenced.
        assert_eq!(
            store
                .base_write_artifact(
                    "owned",
                    "r",
                    "call-1",
                    "g",
                    "component",
                    "card",
                    0,
                    &artifact
                )
                .await
                .unwrap(),
            first
        );
        assert!(store
            .base_write_artifact(
                "owned",
                "r",
                "call-2",
                "g",
                "component",
                "card",
                1,
                &artifact
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn component_projection_is_user_and_session_scoped() {
        let (_root, app) = crate::runtime_tests::fixture_uninitialized().await;
        let owner = app
            .store
            .create_user_account("base-owner", "long-fixture-hash-placeholder")
            .await
            .unwrap();
        let other = app
            .store
            .create_user_account("base-other", "long-fixture-hash-placeholder")
            .await
            .unwrap();
        let first = app
            .store
            .for_user(owner["user_id"].as_str().unwrap())
            .await
            .unwrap();
        let second = app
            .store
            .for_user(other["user_id"].as_str().unwrap())
            .await
            .unwrap();
        for store in [&first, &second] {
            store
                .put(
                    "session:s",
                    &json!({"id":"s","status":"active","agent_id":"shared_default","created_at":1}),
                )
                .await
                .unwrap();
            store.put("run:r",&json!({"id":"r","session_id":"s","agent_id":"shared_default",
                "status":"running","generation":"g","lease_epoch":0,"pending":{"id":"call-1","state":"executing"}})).await.unwrap();
        }
        let display = json!({"id":"plan-overview","type":"status","title":"Plan",
            "data":{"sections":[{"label":"Goal","value":"Private"}]},"authority":"display_only"});
        first
            .base_write_artifact(
                "s",
                "r",
                "call-1",
                "g",
                "component",
                "plan-overview",
                0,
                &display,
            )
            .await
            .unwrap();
        let projected = first.base_chat_components("s").await.unwrap();
        assert_eq!(projected.len(), 1);
        assert_eq!(
            projected[0]["component"]["data"]["sections"][0]["value"],
            "Private"
        );
        assert!(second.base_chat_components("s").await.unwrap().is_empty());
        assert!(first.base_chat_components("missing").await.is_err());
    }
}
