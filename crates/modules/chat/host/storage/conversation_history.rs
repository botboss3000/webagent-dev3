//! Transactional transcript rewind and independent history snapshots.
//! Removed rows remain in the encrypted request receipt; executable runs are never cloned.
use super::session::SessionPatchOutcome;
use super::*;
use std::collections::{HashMap, HashSet};

pub(crate) use webagent_chat::history::{remap_attachment_references, HistoryRequest};
impl Store {
    /// Atomically fork a bounded independent history or archive Undo removals, with receipt replay and stale-history fences.
    pub(crate) async fn conversation_history(
        &self,
        sid: &str,
        body: &HistoryRequest,
        fork: Option<(Value, Value, usize)>,
    ) -> Result<SessionPatchOutcome> {
        use SessionPatchOutcome::*;
        if body.client_request_id.is_empty()
            || body.client_request_id.len() > 128
            || body.client_request_id.trim() != body.client_request_id
            || body.client_request_id.chars().any(char::is_control)
            || body.message_id.is_empty()
            || body.message_id.len() > 256
            || body.expected_last_seq < 1
        {
            return Ok(Invalid("Invalid history request"));
        }
        let forking = fork.is_some();
        if !forking {
            self.background_index().await?;
        }
        let session_key = format!("session:{sid}");
        let receipt_key = format!("receipt:session:history:{sid}:{}", body.client_request_id);
        let fingerprint = crate::digest(&json!({"body":body,"fork":forking}));
        let _guard = self.context_gate.write().await;
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE docs SET key=key WHERE key=?")
            .bind(&session_key)
            .execute(&mut *tx)
            .await?;
        if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&receipt_key)
            .fetch_optional(&mut *tx)
            .await?
        {
            let receipt = self.unseal(&raw)?;
            if receipt["digest"] != fingerprint {
                return Ok(Conflict("Request identifier reused with different input"));
            }
            return Ok(Saved(receipt["result"].clone()));
        }
        let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&session_key)
            .fetch_optional(&mut *tx)
            .await?
        else {
            return Ok(NotFound);
        };
        let mut session = self.unseal(&raw)?;
        if session["status"] != "active" {
            return Ok(Conflict("Session is recycled"));
        }
        let last: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(seq),0) FROM chat WHERE session_id=?")
                .bind(sid)
                .fetch_one(&mut *tx)
                .await?;
        if last != body.expected_last_seq {
            return Ok(Conflict("Conversation changed; reload before retrying"));
        }
        let Some(target) =
            sqlx::query("SELECT seq,run_id,role,content FROM chat WHERE session_id=? AND id=?")
                .bind(sid)
                .bind(&body.message_id)
                .fetch_optional(&mut *tx)
                .await?
        else {
            return Ok(NotFound);
        };
        let target_seq: i64 = target.get("seq");
        let target_run: String = target.get("run_id");
        if target.get::<String, _>("role") != if forking { "assistant" } else { "user" } {
            return Ok(Invalid(
                "Undo requires a user message; fork requires an assistant message",
            ));
        }
        let mut source_queue = self.queue_read_tx(&mut tx, sid).await?;
        if !forking {
            if source_queue["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| {
                    matches!(
                        entry["status"].as_str(),
                        Some("pending" | "reviewing" | "held")
                    )
                })
            {
                return Ok(Conflict("Cancel or deliver queued messages before undoing"));
            }
            if source_queue["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["message_id"] == body.message_id)
            {
                return Ok(Conflict(
                    "A followup belongs to a combined delivery; undo its original message instead",
                ));
            }
            let unresolved:i64=sqlx::query_scalar("SELECT COUNT(*) FROM run_index WHERE session_id=? AND status NOT IN ('completed','cancelled','failed')")
                .bind(sid).fetch_one(&mut *tx).await?;
            let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM docs WHERE key=?")
                .bind(format!("manager_pending:{sid}"))
                .fetch_one(&mut *tx)
                .await?;
            let background:i64=sqlx::query_scalar("SELECT COUNT(*) FROM background_task_index b WHERE session_id=? AND (status IN ('queued','running') OR (status='completed' AND NOT EXISTS (SELECT 1 FROM background_task_delivery d WHERE d.id=b.id)))")
                .bind(sid).fetch_one(&mut *tx).await?;
            if unresolved + pending + background > 0 {
                return Ok(Conflict(
                    "Resolve the existing run, message review and background tasks before undoing",
                ));
            }
        }
        let chats=sqlx::query("SELECT seq,id,run_id,role,content,created_at FROM chat WHERE session_id=? ORDER BY seq")
            .bind(sid).fetch_all(&mut *tx).await?;
        let items=sqlx::query("SELECT i.seq,i.run_id,i.step,i.item,i.created_at,b.boundary_item_id FROM interactions i LEFT JOIN activity_boundaries b ON b.interaction_seq=i.seq WHERE i.session_id=? ORDER BY i.seq")
            .bind(sid).fetch_all(&mut *tx).await?;
        let mut archive = json!({});
        let result;
        let mut touched = vec![session_key.clone()];
        if let Some((mut child, mode, max_sessions)) = fork {
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM docs WHERE key LIKE 'session:%'")
                    .fetch_one(&mut *tx)
                    .await?;
            if count as usize >= max_sessions {
                return Ok(Conflict("Session limit reached"));
            }
            let cutoff: Option<i64> = sqlx::query_scalar(
                "SELECT through_seq FROM chat_context_boundaries WHERE message_id=?",
            )
            .bind(&body.message_id)
            .fetch_optional(&mut *tx)
            .await?;
            // Legacy terminal publications have an exact whole-run boundary. Never guess a partial boundary from timestamps.
            let cutoff=match cutoff {
                Some(seq)=>seq,
                None if body.message_id==format!("assistant:{target_run}")=>items.iter().filter(|r|r.get::<String,_>("run_id")==target_run).map(|r|r.get::<i64,_>("seq")).max().unwrap_or(0),
                None=>return Ok(Conflict("This older progress message has no recorded fork boundary; select a final reply")),
            };
            let id = child["id"]
                .as_str()
                .ok_or_else(|| anyhow!("Invalid child session"))?
                .to_owned();
            child["title"] = json!(format!(
                "Fork: {}",
                session["title"].as_str().unwrap_or("New chat")
            )
            .chars()
            .take(120)
            .collect::<String>());
            child["forked_from"] =
                json!({"session_id":sid,"message_id":body.message_id,"through_seq":target_seq});
            child["auto_naming"] = json!({"version":1,"stage":"locked"});
            child["conversation_privacy"]=match session["conversation_privacy"].as_str() {
                Some("operator_reviewable")=>json!("operator_reviewable"),_=>json!("private")
            };
            child.as_object_mut().unwrap().remove("operator_joined");
            let admitted_batches: HashSet<String> = items
                .iter()
                .filter(|row| row.get::<i64, _>("seq") <= cutoff)
                .filter_map(|row| {
                    row.get::<String, _>("step")
                        .strip_prefix("followup:")
                        .map(str::to_owned)
                })
                .collect();
            let mut copied_followups = Vec::new();
            let mut copied_bytes = 0u64;
            let mut attachment_map = HashMap::new();
            let mut run_map = HashMap::new();
            let mut message_map = HashMap::new();
            let mut excluded_runs = HashSet::new();
            for row in chats
                .iter()
                .filter(|r| r.get::<i64, _>("seq") <= target_seq)
            {
                let followup = source_queue["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|entry| entry["message_id"] == row.get::<String, _>("id"));
                if followup.is_some_and(|entry| {
                    entry["status"] != "delivered"
                        || !entry["batch_id"]
                            .as_str()
                            .is_some_and(|batch| admitted_batches.contains(batch))
                }) {
                    continue;
                }
                let old: String = row.get("run_id");
                let rid = run_map
                    .entry(old.clone())
                    .or_insert_with(crate::uuid)
                    .clone();
                let mid = crate::uuid();
                message_map.insert(row.get::<String, _>("id"), mid.clone());
                // Forked history preserves author attribution and permanent context exclusions.
                // No queued message inherits dispatch eligibility in the independent child.
                let old_mid: String = row.get("id");
                for (old_key, new_key) in [
                    (format!("message_meta:{old}"), format!("message_meta:{rid}")),
                    (format!("message_meta:{old_mid}"), format!("message_meta:{mid}")),
                ] {
                    if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
                        .bind(&old_key).fetch_optional(&mut *tx).await? {
                        let mut metadata = self.unseal(&raw)?;
                        if metadata["context_eligible"] == false || metadata["queued"] == true {
                            excluded_runs.insert(old.clone());
                            metadata["context_eligible"] = json!(false);
                        }
                        metadata["queued"] = json!(false);
                        metadata["session_id"] = json!(id);
                        self.write_document(&mut tx, &new_key, &metadata).await?;
                    }
                }
                if let Some(entry) = followup {
                    copied_followups.push((entry.clone(), rid.clone(), mid.clone()));
                }
                sqlx::query("INSERT INTO chat(id,session_id,run_id,role,content,created_at) VALUES(?,?,?,?,?,?)")
                    .bind(&mid).bind(&id).bind(&rid).bind(row.get::<String,_>("role"))
                    .bind(row.get::<Vec<u8>,_>("content")).bind(row.get::<i64,_>("created_at")).execute(&mut *tx).await?;
                if row.get::<String, _>("role") == "user" {
                    if let Some(raw) =
                        sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
                            .bind(format!("message_attachments:{old}"))
                            .fetch_optional(&mut *tx)
                            .await?
                    {
                        let mut files = self.unseal(&raw)?;
                        if let Some(files) = files.as_array_mut() {
                            for file in files {
                                let aid = file["id"]
                                    .as_str()
                                    .ok_or_else(|| anyhow!("Invalid attachment reference"))?;
                                let raw: Vec<u8> =
                                    sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
                                        .bind(format!("attachment:{aid}"))
                                        .fetch_one(&mut *tx)
                                        .await?;
                                let mut record = self.unseal(&raw)?;
                                if record["session_id"] != sid {
                                    return Err(anyhow!("Attachment session mismatch"));
                                }
                                copied_bytes = copied_bytes.checked_add(record["bytes"].as_u64().unwrap_or(0)).ok_or_else(|| anyhow!("Attachment byte count exceeds machine accounting range"))?;
                                let old_aid = aid.to_owned();
                                let aid = crate::uuid();
                                attachment_map.insert(old_aid, aid.clone());
                                record["id"] = json!(aid);
                                record["session_id"] = json!(id);
                                record["run_id"] = json!(rid);
                                file["id"] = json!(aid);
                                self.write_document(&mut tx, &format!("attachment:{aid}"), &record)
                                    .await?;
                            }
                        }
                        self.write_document(&mut tx, &format!("message_attachments:{rid}"), &files)
                            .await?;
                    }
                }
            }
            if copied_bytes > 0 {
                let used = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
                    .bind("attachment_quota")
                    .fetch_optional(&mut *tx)
                    .await?
                    .map(|raw| self.unseal(&raw))
                    .transpose()?
                    .and_then(|value| value["bytes"].as_u64())
                    .unwrap_or(0);
                let total = used.checked_add(copied_bytes).ok_or_else(|| {
                    anyhow!("Attachment byte count exceeds machine accounting range")
                })?;
                self.write_document(&mut tx, "attachment_quota", &json!({"bytes":total}))
                    .await?;
                touched.push("attachment_quota".into());
            }
            let selected: Vec<_> = items
                .iter()
                .filter(|r| r.get::<i64, _>("seq") <= cutoff)
                .filter(|r| !excluded_runs.contains(&r.get::<String, _>("run_id")))
                .collect();
            let outputs: HashSet<String> = selected
                .iter()
                .map(|r| self.unseal(&r.get::<Vec<u8>, _>("item")))
                .collect::<Result<Vec<_>>>()?
                .iter()
                .filter(|v| v["type"] == "function_call_output")
                .filter_map(|v| v["call_id"].as_str().map(str::to_owned))
                .collect();
            let mut boundaries = HashMap::new();
            for row in selected {
                let mut item = self.unseal(&row.get::<Vec<u8>, _>("item"))?;
                remap_attachment_references(&mut item, &attachment_map);
                // A fork never inherits an outstanding tool dispatch or an approval.
                if item["type"] == "function_call"
                    && !outputs.contains(item["call_id"].as_str().unwrap_or(""))
                {
                    continue;
                }
                let old: String = row.get("run_id");
                let rid = run_map.entry(old).or_insert_with(crate::uuid);
                self.insert_interaction(&mut tx, &id, rid, &row.get::<String, _>("step"), &item)
                    .await?;
                let seq: i64 =
                    sqlx::query_scalar("SELECT MAX(seq) FROM interactions WHERE session_id=?")
                        .bind(&id)
                        .fetch_one(&mut *tx)
                        .await?;
                if let Some(anchor) = row
                    .get::<Option<String>, _>("boundary_item_id")
                    .and_then(|anchor| message_map.get(&anchor))
                {
                    sqlx::query(
                        "UPDATE activity_boundaries SET boundary_item_id=? WHERE interaction_seq=?",
                    )
                    .bind(anchor)
                    .bind(seq)
                    .execute(&mut *tx)
                    .await?;
                } else {
                    sqlx::query("DELETE FROM activity_boundaries WHERE interaction_seq=?")
                        .bind(seq)
                        .execute(&mut *tx)
                        .await?;
                }
                boundaries.insert(row.get::<i64, _>("seq"), seq);
            }
            for (old, mid) in message_map {
                let old_boundary: Option<i64> = sqlx::query_scalar(
                    "SELECT through_seq FROM chat_context_boundaries WHERE message_id=?",
                )
                .bind(&old)
                .fetch_optional(&mut *tx)
                .await?;
                let seq = match old_boundary {
                    Some(seq) => seq.min(cutoff),
                    None => {
                        let old_run = chats
                            .iter()
                            .find(|row| row.get::<String, _>("id") == old)
                            .map(|row| row.get::<String, _>("run_id"))
                            .ok_or_else(|| anyhow!("Missing copied message"))?;
                        // Preserve each legacy final's own cut, not the selected later reply's cut.
                        // Unknown legacy progress remains unavailable for a nested fork.
                        if old != format!("assistant:{old_run}") {
                            continue;
                        }
                        items
                            .iter()
                            .filter(|row| row.get::<String, _>("run_id") == old_run)
                            .map(|row| row.get::<i64, _>("seq"))
                            .max()
                            .unwrap_or(0)
                            .min(cutoff)
                    }
                };
                let through = boundaries
                    .iter()
                    .filter(|(old, _)| **old <= seq)
                    .max_by_key(|(old, _)| **old)
                    .map(|(_, seq)| *seq)
                    .unwrap_or(0);
                sqlx::query(
                    "INSERT INTO chat_context_boundaries(message_id,through_seq) VALUES(?,?)",
                )
                .bind(mid)
                .bind(through)
                .execute(&mut *tx)
                .await?;
            }
            // Historical delivery status is copied without review templates or executable authority.
            if !copied_followups.is_empty() {
                let mut child_queue = json!({"schema_version":1,"session_id":id,"revision":0,
                    "held":false,"reason":null,"entries":[]});
                for (entry, rid, mid) in copied_followups {
                    let mut context = entry["context_content"].clone();
                    remap_attachment_references(&mut context, &attachment_map);
                    let files =
                        sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
                            .bind(format!("message_attachments:{rid}"))
                            .fetch_optional(&mut *tx)
                            .await?
                            .map(|raw| self.unseal(&raw))
                            .transpose()?
                            .unwrap_or_else(|| json!([]));
                    let consumer = entry["consuming_run_id"]
                        .as_str()
                        .and_then(|run| run_map.get(run))
                        .cloned();
                    child_queue["entries"].as_array_mut().unwrap().push(json!({
                        "id":rid,"message_id":mid,"session_id":id,"agent_id":child["agent_id"],
                        "revision":0,"choice":"queue","status":"delivered","review_status":"acceptable",
                        "content":entry["content"],"context_content":context,"attachments":files,"agent_message":entry["agent_message"],
                        "consuming_run_id":consumer,"batch_id":entry["batch_id"]}));
                }
                self.queue_write_tx(&mut tx, &mut child_queue).await?;
                touched.push(format!("followup_queue:{id}"));
            }
            self.write_document(&mut tx, &format!("session:{id}"), &child)
                .await?;
            self.write_document(&mut tx, &format!("session_mode:{id}"), &mode)
                .await?;
            touched.push(format!("session:{id}"));
            self.stage_notification(
                &mut tx,
                &format!("created:{id}"),
                "session.created",
                &id,
                None,
                None,
            )
            .await?;
            result = json!({"session_id":id,"title":child["title"],"agent_id":child["agent_id"]});
        } else {
            let mut removed: HashSet<String> = chats
                .iter()
                .filter(|r| r.get::<i64, _>("seq") >= target_seq)
                .map(|r| r.get("run_id"))
                .collect();
            // Continue and failed runs can add private context without publishing a chat row.
            // Rewind those later runs too, otherwise the next model call still sees undone work.
            let cutoff = items
                .iter()
                .filter(|row| removed.contains(&row.get::<String, _>("run_id")))
                .map(|row| row.get::<i64, _>("seq"))
                .min();
            if let Some(cutoff) = cutoff {
                removed.extend(
                    items
                        .iter()
                        .filter(|row| row.get::<i64, _>("seq") >= cutoff)
                        .map(|row| row.get::<String, _>("run_id")),
                );
            }
            let later: Vec<String> = sqlx::query_scalar("SELECT id FROM run_index WHERE session_id=? AND enqueue_seq >= (SELECT enqueue_seq FROM run_index WHERE id=? AND session_id=?)")
                .bind(sid).bind(&target_run).bind(sid).fetch_all(&mut *tx).await?;
            removed.extend(later);
            let mut archived_chat = Vec::new();
            let mut archived_items = Vec::new();
            for row in chats
                .iter()
                .filter(|r| r.get::<i64, _>("seq") >= target_seq)
            {
                archived_chat.push(json!({"seq":row.get::<i64,_>("seq"),"id":row.get::<String,_>("id"),"run_id":row.get::<String,_>("run_id"),"role":row.get::<String,_>("role"),"content":self.unseal(&row.get::<Vec<u8>,_>("content"))?,"created_at":row.get::<i64,_>("created_at")}));
            }
            for row in items
                .iter()
                .filter(|r| removed.contains(&r.get::<String, _>("run_id")))
            {
                archived_items.push(json!({"seq":row.get::<i64,_>("seq"),"run_id":row.get::<String,_>("run_id"),"step":row.get::<String,_>("step"),"item":self.unseal(&row.get::<Vec<u8>,_>("item"))?,"created_at":row.get::<i64,_>("created_at")}));
            }
            for rid in removed {
                let key = format!("run:{rid}");
                if let Some(raw) =
                    sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
                        .bind(&key)
                        .fetch_optional(&mut *tx)
                        .await?
                {
                    let mut run = self.decode_document(&mut tx, &raw).await?;
                    run["history_removed"] = json!(true);
                    run["history_removed_status"] = run["status"].clone();
                    if run["status"] == "failed" {
                        run["status"] = json!("cancelled");
                    }
                    run["run_revision"] = json!(run["run_revision"]
                        .as_u64()
                        .unwrap_or(0)
                        .checked_add(1)
                        .ok_or_else(|| anyhow!("Run revision exhausted"))?);
                    self.write_document(&mut tx, &key, &run).await?;
                    touched.push(key);
                }
                sqlx::query("DELETE FROM run_index WHERE id=? AND session_id=?")
                    .bind(&rid)
                    .bind(sid)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("DELETE FROM activity_boundaries WHERE interaction_seq IN (SELECT seq FROM interactions WHERE session_id=? AND run_id=?)").bind(sid).bind(&rid).execute(&mut *tx).await?;
                sqlx::query("DELETE FROM interactions WHERE session_id=? AND run_id=?")
                    .bind(sid)
                    .bind(&rid)
                    .execute(&mut *tx)
                    .await?;
            }
            sqlx::query("DELETE FROM chat_context_boundaries WHERE message_id IN (SELECT id FROM chat WHERE session_id=? AND seq>=?)").bind(sid).bind(target_seq).execute(&mut *tx).await?;
            sqlx::query("DELETE FROM chat WHERE session_id=? AND seq>=?")
                .bind(sid)
                .bind(target_seq)
                .execute(&mut *tx)
                .await?;
            // The old compaction checkpoint may contain undone text. Rebuild from retained durable interactions.
            session["checkpoint_input"] = json!([]);
            session["checkpoint_through"] = json!(0);
            session["tool_context_state"] = Value::Null;
            session["tool_result_projection"] = Value::Null;
            let retained: i64 =
                sqlx::query_scalar("SELECT COALESCE(MAX(seq),0) FROM chat WHERE session_id=?")
                    .bind(sid)
                    .fetch_one(&mut *tx)
                    .await?;
            session["read_through_seq"] = json!(session["read_through_seq"]
                .as_u64()
                .unwrap_or(0)
                .min(retained as u64));
            session["history_revision"] = json!(session["history_revision"]
                .as_u64()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| anyhow!("History revision exhausted"))?);
            session["metadata_revision"] = json!(session["metadata_revision"]
                .as_u64()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| anyhow!("Session metadata revision exhausted"))?);
            session["auto_naming"] = json!({"version":1,"stage":"locked"});
            sqlx::query("DELETE FROM docs WHERE key=?")
                .bind(format!("session_naming:{sid}"))
                .execute(&mut *tx)
                .await?;
            // Undo changes history identity without unloading permanent selections.
            self.reset_context_asset_layouts(&mut tx,sid).await?;
            self.write_document(&mut tx, &session_key, &session).await?;
            let retained_messages: HashSet<String> = chats
                .iter()
                .filter(|row| row.get::<i64, _>("seq") < target_seq)
                .map(|row| row.get("id"))
                .collect();
            let removed_followups: Vec<Value> = source_queue["entries"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|entry| {
                    !entry["message_id"]
                        .as_str()
                        .is_some_and(|mid| retained_messages.contains(mid))
                })
                .cloned()
                .collect();
            if !removed_followups.is_empty() {
                source_queue["entries"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|entry| {
                        entry["message_id"]
                            .as_str()
                            .is_some_and(|mid| retained_messages.contains(mid))
                    });
                self.queue_write_tx(&mut tx, &mut source_queue).await?;
            }
            archive = json!({"chat":archived_chat,"interactions":archived_items,"followups":removed_followups});
            result = json!({"session_id":sid,"text":self.unseal(&target.get::<Vec<u8>,_>("content"))?,"messages_removed":archive["chat"].as_array().map(Vec::len)});
        }
        self.write_document(
            &mut tx,
            &receipt_key,
            &json!({"session_id":sid,"digest":fingerprint,"result":result,"archive":archive}),
        )
        .await?;
        sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,?,?)")
            .bind(sid)
            .bind(if forking {
                "session_forked"
            } else {
                "session_updated"
            })
            .bind(self.encode_user_record(&result)?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.cache.lock().unwrap().invalidate(&touched);
        self.context_cache
            .lock()
            .unwrap()
            .invalidate(&self.context_cache_key(sid));
        Ok(Saved(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Create two completed public/private exchanges in isolated encrypted storage for history lifecycle tests.
    async fn fixture() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store.create_admin_account("root","long-hash-placeholder","generation").await.unwrap();
        store.put("session:s",&json!({"id":"s","title":"Original","agent_id":"shared_default","checkpoint_input":[],"checkpoint_through":0})).await.unwrap();
        for rid in ["r1", "r2"] {
            store.put(&format!("run:{rid}"),&json!({"id":rid,"session_id":"s","status":"completed","run_revision":0,"input":[]})).await.unwrap();
            let mut tx = store.pool.begin().await.unwrap();
            store
                .insert_chat(&mut tx, "s", rid, "user", &json!(format!("question {rid}")))
                .await
                .unwrap();
            store
                .insert_interaction(
                    &mut tx,
                    "s",
                    rid,
                    "user",
                    &json!({"role":"user","content":format!("question {rid}")}),
                )
                .await
                .unwrap();
            store
                .insert_interaction(
                    &mut tx,
                    "s",
                    rid,
                    "model:1:0",
                    &json!({"type":"message","role":"assistant","content":format!("answer {rid}")}),
                )
                .await
                .unwrap();
            store
                .insert_chat(
                    &mut tx,
                    "s",
                    rid,
                    "assistant",
                    &json!(format!("answer {rid}")),
                )
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }
        (root, store)
    }
    /// Bind a history-test request to the current public-chat tail sequence.
    async fn request(store: &Store, id: &str, key: &str) -> HistoryRequest {
        HistoryRequest {
            message_id: id.into(),
            client_request_id: key.into(),
            expected_last_seq: store.chat_snapshot("s").await.unwrap().1,
        }
    }
    /// Require a successful history outcome and return its payload to focused assertions.
    fn saved(outcome: SessionPatchOutcome) -> Value {
        match outcome {
            SessionPatchOutcome::Saved(v) => v,
            _ => panic!("expected saved outcome"),
        }
    }
    /// Verify fork preserves authorship and excludes inactive or formerly queued messages from every model-context view.
    #[tokio::test]
    async fn fork_preserves_inactive_and_queued_exclusions_with_authorship() {
        for queued in [false, true] {
            let (_root, store) = fixture().await;
            store.put("message_meta:r1",&json!({"author_id":"alice","context_eligible":queued,"queued":queued})).await.unwrap();
            store.put("message_meta:user:r1",&json!({"author_id":"alice","author_name":"Alice","context_eligible":false,"queued":queued})).await.unwrap();
            let selected = request(&store,"assistant:r2","fork-excluded").await;
            let child = json!({"id":"excluded-child","agent_id":"shared_default","title":"Fork","participation":{"active":true,"revision":0,"generation":0}});
            saved(store.conversation_history("s",&selected,Some((child,json!({}),100))).await.unwrap());
            // Discard warmed source/child caches and rebuild durable model inputs.
            store.context_cache.lock().unwrap().invalidate(&store.context_cache_key("excluded-child"));
            let public = store.chat_snapshot("excluded-child").await.unwrap().0;
            assert_eq!(public.len(),4);
            let first = &public[0];
            let rid = first["run_id"].as_str().unwrap();
            let meta = store.get(&format!("message_meta:{rid}")).await.unwrap().unwrap();
            assert_eq!(meta["author_id"],"alice");
            assert_eq!(meta["context_eligible"],false);
            assert_eq!(meta["queued"],false);
            let meta = store.get(&format!("message_meta:{}",first["id"].as_str().unwrap())).await.unwrap().unwrap();
            assert_eq!(meta["author_name"],"Alice");
            let context = store.context("excluded-child").await.unwrap();
            assert!(!json!(context).to_string().contains("question r1"));
            assert!(!json!(context).to_string().contains("answer r1"));
            let shared = store.shared_model_context("excluded-child").await.unwrap();
            assert!(!json!(shared).to_string().contains("question r1"));
            let page = store.model_chat_page("excluded-child",0,20).await.unwrap();
            assert!(!page.to_string().contains("question r1"));
            assert!(!store.model_search_hit_allowed(&json!({"session_id":"excluded-child","seq":first["seq"]})).await.unwrap());
        }
    }
    /// Verify nested forks preserve legacy final boundaries and refuse ambiguous progress cutoffs.
    #[tokio::test]
    async fn nested_fork_preserves_legacy_final_cuts_and_rejects_unknown_progress() {
        for progress in [false, true] {
            let (_root, store) = fixture().await;
            sqlx::query("DELETE FROM chat_context_boundaries")
                .execute(&store.pool)
                .await
                .unwrap();
            if progress {
                sqlx::query("UPDATE chat SET id=? WHERE id=?")
                    .bind("legacy-progress")
                    .bind("assistant:r1")
                    .execute(&store.pool)
                    .await
                    .unwrap();
            }
            let selected = request(&store, "assistant:r2", "outer").await;
            saved(
                store
                    .conversation_history(
                        "s",
                        &selected,
                        Some((
                            json!({"id":"outer","agent_id":"shared_default"}),
                            json!({}),
                            100,
                        )),
                    )
                    .await
                    .unwrap(),
            );
            let (messages, last) = store.chat_snapshot("outer").await.unwrap();
            assert_eq!(store.context("outer").await.unwrap().len(), 4);
            let selected = HistoryRequest {
                client_request_id: "inner".into(),
                message_id: messages[1]["id"].as_str().unwrap().into(),
                expected_last_seq: last,
            };
            let result = store
                .conversation_history(
                    "outer",
                    &selected,
                    Some((
                        json!({"id":"inner","agent_id":"shared_default"}),
                        json!({}),
                        100,
                    )),
                )
                .await
                .unwrap();
            if progress {
                assert!(matches!(result, SessionPatchOutcome::Conflict(_)));
                assert!(store.get("session:inner").await.unwrap().is_none());
            } else {
                saved(result);
                assert_eq!(store.chat_snapshot("inner").await.unwrap().0.len(), 2);
                let context = store.context("inner").await.unwrap();
                assert_eq!(context.len(), 2);
                assert!(!json!(context).to_string().contains("r2"));
            }
        }
    }
    /// Undo retains selections but resets layouts; a fork starts with no dynamic selections.
    #[tokio::test]
    async fn context_assets_survive_undo_without_being_inherited_by_fork() {
        for forking in [false,true] {
            let (_root,store)=fixture().await;
            let state=webagent_assets_contracts::State{revision:3,
                selectors:vec![json!({"type":"tool","tool_name":"file_read"})],
                placements:vec![webagent_assets_contracts::Placement{id:"main".into(),after_input:0,anchor:None}],
                schema_order:vec!["file_read".into()],history_basis:"before".into(),..Default::default()};
            let mut scopes=serde_json::Map::new();
            scopes.insert(super::super::context_assets::scope_key("shared_default",store.current_user_id()),json!(state));
            store.put("session_context_assets:s",&json!({"version":1,"session_id":"s","scopes":scopes})).await.unwrap();
            if forking {
                let selected=request(&store,"assistant:r1","assets-fork").await;
                saved(store.conversation_history("s",&selected,Some((
                    json!({"id":"assets-child","agent_id":"shared_default"}),json!({}),100))).await.unwrap());
                assert!(store.get("session_context_assets:assets-child").await.unwrap().is_none());
                assert_eq!(store.context_assets("assets-child","shared_default",store.current_user_id()).await.unwrap(),
                    webagent_assets_contracts::State::default());
                assert_eq!(store.context_assets("s","shared_default",store.current_user_id()).await.unwrap(),state);
            } else {
                let selected=request(&store,"user:r2","assets-undo").await;
                saved(store.conversation_history("s",&selected,None).await.unwrap());
                let current=store.context_assets("s","shared_default",store.current_user_id()).await.unwrap();
                assert_eq!(current.revision,4);assert_eq!(current.selectors,state.selectors);
                assert!(current.placements.is_empty()&&current.schema_order.is_empty()&&current.history_basis.is_empty());
            }
        }
    }

    /// Verify Undo revision fences prevent stale checkpoint restoration and clamp the public read cursor.
    #[tokio::test]
    async fn undo_fences_stale_session_checkpoints_and_clamps_read_cursor() {
        let (_root, store) = fixture().await;
        let mut stale = store.get("session:s").await.unwrap().unwrap();
        stale["checkpoint_input"] = json!([{"role":"user","content":"discarded r2 checkpoint"}]);
        stale["checkpoint_through"] = json!(999);
        stale["read_through_seq"] = json!(999);
        store.put("session:s", &stale).await.unwrap();
        let selected = request(&store, "user:r2", "undo-stale-checkpoint").await;
        saved(
            store
                .conversation_history("s", &selected, None)
                .await
                .unwrap(),
        );
        let last = store.chat_snapshot("s").await.unwrap().1;
        let session = store.get("session:s").await.unwrap().unwrap();
        assert_eq!(session["read_through_seq"], last);
        assert_eq!(session["history_revision"], 1);
        store.put("session:s", &stale).await.unwrap();
        let session = store.get("session:s").await.unwrap().unwrap();
        assert_eq!(session["history_revision"], 1);
        assert_eq!(session["checkpoint_through"], 0);
        assert_eq!(store.context("s").await.unwrap().len(), 2);
    }
    /// Verify Undo also removes later failed/private work without a public message anchor.
    #[tokio::test]
    async fn undo_also_removes_later_private_only_runs() {
        let (_root, store) = fixture().await;
        store
            .put(
                "run:private",
                &json!({"id":"private","session_id":"s","status":"failed"}),
            )
            .await
            .unwrap();
        let mut tx = store.pool.begin().await.unwrap();
        store
            .insert_interaction(
                &mut tx,
                "s",
                "private",
                "model:1:0",
                &json!({"type":"message","role":"assistant","content":"later private-only work"}),
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let selected = request(&store, "user:r2", "rewind-private").await;
        saved(
            store
                .conversation_history("s", &selected, None)
                .await
                .unwrap(),
        );
        assert_eq!(store.context("s").await.unwrap().len(), 2);
        assert_eq!(
            store.get("run:private").await.unwrap().unwrap()["history_removed"],
            true
        );
    }
    /// Verify source purge removes archived recovery and receipts while preserving independent forked history.
    #[tokio::test]
    async fn purge_removes_undo_archive_and_fork_receipt_without_deleting_child() {
        let (_root, store) = fixture().await;
        let selected = request(&store, "assistant:r1", "fork-before-purge").await;
        let child = json!({"id":"child","agent_id":"shared_default","title":"Fork"});
        saved(
            store
                .conversation_history("s", &selected, Some((child, json!({}), 100)))
                .await
                .unwrap(),
        );
        store.put("run:r2", &json!({"id":"r2","session_id":"s","status":"completed","input":[{"role":"user","content":"private recovery"}]})).await.unwrap();
        let selected = request(&store, "user:r2", "undo-before-purge").await;
        saved(
            store
                .conversation_history("s", &selected, None)
                .await
                .unwrap(),
        );
        let session = store.get("session:s").await.unwrap().unwrap();
        let recycle = super::super::session::SessionLifecycle {
            client_request_id: "recycle".into(),
            expected_revision: session["metadata_revision"].as_u64().unwrap(),
        };
        saved(store.lifecycle_session("s", &recycle, false).await.unwrap());
        let purge = super::super::session::SessionLifecycle {
            client_request_id: "purge".into(),
            expected_revision: recycle.expected_revision + 1,
        };
        saved(store.purge_session("s", &purge).await.unwrap());
        assert!(store
            .get("receipt:session:history:s:undo-before-purge")
            .await
            .unwrap()
            .is_none());
        assert!(store
            .get("receipt:session:history:s:fork-before-purge")
            .await
            .unwrap()
            .is_none());
        assert!(store.get("run:r2").await.unwrap().is_none());
        assert_eq!(store.chat_snapshot("child").await.unwrap().0.len(), 2);
        let orphaned: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_context_boundaries WHERE message_id NOT IN (SELECT id FROM chat)")
            .fetch_one(&store.pool).await.unwrap();
        assert_eq!(orphaned, 0);
        let recovery: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM recovery_input WHERE run_id=?")
                .bind("r2")
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert_eq!(recovery, 0);
    }
    /// Verify Undo archives removed interactions, fences removed runs and replays its exact receipt after reopen.
    #[tokio::test]
    async fn undo_rewinds_context_archives_and_fences_run_with_idempotent_retry() {
        let (root, store) = fixture().await;
        let request = request(&store, "user:r2", "undo").await;
        let _ = store.context("s").await.unwrap();
        store.put("session:s",&json!({"id":"s","checkpoint_input":[{"role":"user","content":"compacted r2"}],"checkpoint_through":999})).await.unwrap();
        let result = saved(
            store
                .conversation_history("s", &request, None)
                .await
                .unwrap(),
        );
        assert_eq!(result["text"], "question r2");
        assert_eq!(result["messages_removed"], 2);
        assert_eq!(store.chat_snapshot("s").await.unwrap().0.len(), 2);
        let context = store.context("s").await.unwrap();
        assert_eq!(context.len(), 2);
        assert!(!json!(context).to_string().contains("r2"));
        assert_eq!(
            store.get("run:r2").await.unwrap().unwrap()["history_removed"],
            true
        );
        assert_eq!(
            saved(
                store
                    .conversation_history("s", &request, None)
                    .await
                    .unwrap()
            ),
            result
        );
        let receipt = store
            .get("receipt:session:history:s:undo")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            receipt["archive"]["interactions"].as_array().unwrap().len(),
            2
        );
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(reopened.context("s").await.unwrap(), context);
        let next = request_for(&reopened, "user:r1", "undo-again").await;
        saved(
            reopened
                .conversation_history("s", &next, None)
                .await
                .unwrap(),
        );
        assert!(reopened.context("s").await.unwrap().is_empty());
    }
    /// Reuse the history request builder when assertions need a fresh post-operation cursor.
    async fn request_for(store: &Store, id: &str, key: &str) -> HistoryRequest {
        request(store, id, key).await
    }
    /// Verify forks copy only the selected prefix with remapped identities/attachments and no executable run state.
    #[tokio::test]
    async fn fork_copies_only_prefix_new_identifiers_attachments_and_no_executable_runs() {
        let (_root, store) = fixture().await;
        store.put("attachment:old",&json!({"id":"old","session_id":"s","run_id":"r1","name":"file.txt","text":"attachment evidence","bytes":19})).await.unwrap();
        store
            .put(
                "message_attachments:r1",
                &json!([{"id":"old","name":"file.txt"}]),
            )
            .await
            .unwrap();
        store
            .put("attachment_quota", &json!({"bytes":19}))
            .await
            .unwrap();
        let mut source=store.get("session:s").await.unwrap().unwrap();
        source["conversation_privacy"]=json!("private");store.put("session:s",&source).await.unwrap();
        let request = request(&store, "assistant:r1", "fork").await;
        let child = json!({"id":"child","title":"Fork","agent_id":"shared_default","checkpoint_input":[],"checkpoint_through":0,"conversation_privacy":"operator_reviewable","operator_joined":{"operator":true}});
        let result = saved(
            store
                .conversation_history(
                    "s",
                    &request,
                    Some((child.clone(), json!({"mode":"ask","revision":0}), 100)),
                )
                .await
                .unwrap(),
        );
        assert_eq!(result["title"], "Fork: Original");
        let forked=store.get("session:child").await.unwrap().unwrap();
        assert_eq!(forked["conversation_privacy"],"private");assert!(forked.get("operator_joined").is_none());
        assert_eq!(
            store.get("attachment_quota").await.unwrap().unwrap()["bytes"],
            38
        );
        let (messages, _) = store.chat_snapshot("child").await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_ne!(messages[0]["id"], "user:r1");
        assert_ne!(messages[0]["run_id"], "r1");
        assert_eq!(store.context("child").await.unwrap().len(), 2);
        assert!(store.recent_runs(Some("child")).await.unwrap().is_empty());
        assert!(store
            .get("session_ability_state:child")
            .await
            .unwrap()
            .is_none());
        let files = store
            .get(&format!(
                "message_attachments:{}",
                messages[0]["run_id"].as_str().unwrap()
            ))
            .await
            .unwrap()
            .unwrap();
        assert_ne!(files[0]["id"], "old");
        let record = store
            .get(&format!("attachment:{}", files[0]["id"].as_str().unwrap()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record["session_id"], "child");
        let before = store.context("s").await.unwrap();
        let undo = request_for(&store, messages[0]["id"].as_str().unwrap(), "child-undo").await;
        let undo = HistoryRequest {
            expected_last_seq: store.chat_snapshot("child").await.unwrap().1,
            ..undo
        };
        saved(
            store
                .conversation_history("child", &undo, None)
                .await
                .unwrap(),
        );
        assert!(store.context("child").await.unwrap().is_empty());
        assert_eq!(store.context("s").await.unwrap(), before);
        assert_eq!(
            saved(
                store
                    .conversation_history("s", &request, Some((child, json!({}), 0)))
                    .await
                    .unwrap()
            ),
            result
        );
    }
    /// Verify history guards reject stale cursors, wrong message roles, missing sessions and unresolved runs without mutation.
    #[tokio::test]
    async fn rejects_stale_wrong_role_cross_session_and_unresolved_work() {
        let (_root, store) = fixture().await;
        let mut body = request(&store, "user:r2", "blocked").await;
        body.expected_last_seq -= 1;
        assert!(matches!(
            store.conversation_history("s", &body, None).await.unwrap(),
            SessionPatchOutcome::Conflict(_)
        ));
        body.expected_last_seq += 1;
        body.message_id = "assistant:r2".into();
        assert!(matches!(
            store.conversation_history("s", &body, None).await.unwrap(),
            SessionPatchOutcome::Invalid(_)
        ));
        body.message_id = "user:r2".into();
        assert!(matches!(
            store
                .conversation_history("missing", &body, None)
                .await
                .unwrap(),
            SessionPatchOutcome::NotFound
        ));
        sqlx::query("UPDATE run_index SET status='recovery' WHERE id='r2'")
            .execute(&store.pool)
            .await
            .unwrap();
        assert!(matches!(
            store.conversation_history("s", &body, None).await.unwrap(),
            SessionPatchOutcome::Conflict(_)
        ));
        assert_eq!(store.chat_snapshot("s").await.unwrap().0.len(), 4);
    }
    /// Verify durable history revision invalidates another Store process context cache after Undo.
    #[tokio::test]
    async fn undo_invalidates_an_independent_process_cache_even_at_the_same_checkpoint() {
        let (root, store) = fixture().await;
        let observer = Store::open(root.path()).await.unwrap();
        assert_eq!(observer.context("s").await.unwrap().len(), 4);
        let body = request(&store, "user:r2", "remote-cache").await;
        saved(store.conversation_history("s", &body, None).await.unwrap());
        let remaining = observer.context("s").await.unwrap();
        assert_eq!(remaining.len(), 2);
        assert!(!json!(remaining).to_string().contains("r2"));
    }
    /// Verify progress forks exclude later interactions and outstanding tool dispatches.
    #[tokio::test]
    async fn progress_fork_has_exact_boundary_and_drops_unfinished_tool_calls() {
        let (_root, store) = fixture().await;
        let mut tx = store.pool.begin().await.unwrap();
        store
            .insert_interaction(
                &mut tx,
                "s",
                "r3",
                "user",
                &json!({"role":"user","content":"new question"}),
            )
            .await
            .unwrap();
        store.insert_interaction(&mut tx,"s","r3","model:1:0",&json!({"type":"function_call","call_id":"unanswered","name":"file_write","arguments":"{}"})).await.unwrap();
        store
            .insert_working_message(&mut tx, "s", "r3", "working:r3:progress", "Progress")
            .await
            .unwrap();
        store
            .insert_interaction(
                &mut tx,
                "s",
                "r3",
                "model:2:0",
                &json!({"type":"message","role":"assistant","content":"later hidden"}),
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let body = request(&store, "working:r3:progress", "progress-fork").await;
        let child = json!({"id":"partial","title":"Fork","agent_id":"shared_default","checkpoint_input":[],"checkpoint_through":0});
        saved(
            store
                .conversation_history("s", &body, Some((child, json!({}), 100)))
                .await
                .unwrap(),
        );
        let context = store.context("partial").await.unwrap();
        assert!(!json!(context).to_string().contains("unanswered"));
        assert!(!json!(context).to_string().contains("later hidden"));
        assert_eq!(
            store
                .chat_snapshot("partial")
                .await
                .unwrap()
                .0
                .last()
                .unwrap()["content"],
            "Progress"
        );
    }
    /// Race independent Undo requests and verify one commit plus rejection of changed input under a winning receipt ID.
    #[tokio::test]
    async fn concurrent_undo_has_one_commit_and_rejects_changed_retry_input() {
        let (_root, store) = fixture().await;
        let first = request(&store, "user:r2", "first").await;
        let second = request(&store, "user:r2", "second").await;
        let (a, b) = tokio::join!(
            store.conversation_history("s", &first, None),
            store.conversation_history("s", &second, None)
        );
        assert_eq!(
            [a.unwrap(), b.unwrap()]
                .iter()
                .filter(|v| matches!(v, SessionPatchOutcome::Saved(_)))
                .count(),
            1
        );
        let mut body = request_for(&store, "user:r1", "first").await;
        // If the other writer won, use its exact receipt identity.
        if store
            .get("receipt:session:history:s:first")
            .await
            .unwrap()
            .is_none()
        {
            body.client_request_id = "second".into();
        }
        assert!(matches!(
            store.conversation_history("s", &body, None).await.unwrap(),
            SessionPatchOutcome::Conflict(_)
        ));
    }
    /// Inject receipt failure and verify chat, private context and removed-run flags roll back together.
    #[tokio::test]
    async fn failed_receipt_rolls_back_all_history_mutations() {
        let (_root, store) = fixture().await;
        let body = request(&store, "user:r2", "atomic").await;
        sqlx::query("CREATE TRIGGER reject_history BEFORE INSERT ON docs WHEN NEW.key LIKE 'receipt:session:history:%' BEGIN SELECT RAISE(ABORT,'fixture failure'); END").execute(&store.pool).await.unwrap();
        match store.conversation_history("s", &body, None).await {
            Err(error) => assert!(error.to_string().contains("fixture failure")),
            _ => panic!("expected the injected receipt failure"),
        }
        assert_eq!(store.chat_snapshot("s").await.unwrap().0.len(), 4);
        assert_eq!(store.context("s").await.unwrap().len(), 4);
        assert_ne!(
            store.get("run:r2").await.unwrap().unwrap()["history_removed"],
            true
        );
    }
    /// Submit a real queued followup with optional prepared attachment evidence for history tests.
    async fn queue_history_message(store: &Store, id: &str, text: &str, file: bool) -> Value {
        let attachments = if file {
            Some(
                json!([{"name":format!("{id}.txt"),"media_type":"text/plain","text":format!("evidence {id}")}]),
            )
        } else {
            None
        };
        let prepared = crate::attachments::prepare(attachments.as_ref(), "s", id).unwrap();
        let context = prepared.context_content(text).unwrap();
        let files = json!(prepared.metadata.clone());
        let entry = json!({"id":id,"message_id":format!("user:{id}"),"session_id":"s",
            "agent_id":"shared_default","revision":0,"choice":"queue","status":"pending",
            "review_status":"acceptable","content":text,"context_content":context,
            "attachments":files,"created_at":1,"consuming_run_id":null});
        let mut docs = prepared.records;
        docs.push((format!("message_attachments:{id}"), files));
        store
            .followup_submit("s", entry, docs, id, id, &json!({"submission_id":id}))
            .await
            .unwrap();
        store.followup_queue("s").await.unwrap()["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == id)
            .unwrap()
            .clone()
    }
    /// Build an unclaimed executable fixture for consuming queued history followups.
    fn queue_history_run(id: &str) -> Value {
        json!({"id":id,"session_id":"s","agent_id":"shared_default","status":"queued",
            "generation":"","run_revision":0,"input":[],"updated_at":1})
    }
    /// Verify every unresolved queue mode blocks Undo without changing history, queue or receipts.
    #[tokio::test]
    async fn undo_blocks_every_undelivered_followup_state_transactionally() {
        for state in ["pending", "held", "reviewing", "interrupt"] {
            let (_root, store) = fixture().await;
            queue_history_message(&store, "followup", state, false).await;
            match state {
                "held" | "reviewing" => {
                    assert!(store
                        .followup_review_cas("s", "followup", 0, state, None)
                        .await
                        .unwrap());
                }
                "interrupt" => {
                    store
                        .followup_choice(
                            "s",
                            Some("followup"),
                            0,
                            "interrupt-choice",
                            &json!({"choice":"interrupt"}),
                        )
                        .await
                        .unwrap();
                }
                _ => {}
            }
            let before_queue = store.followup_queue("s").await.unwrap();
            let before_chat = store.chat_snapshot("s").await.unwrap();
            let before_context = store.context("s").await.unwrap();
            let selected = request(&store, "user:r2", "blocked-undo").await;
            assert!(
                matches!(
                    store
                        .conversation_history("s", &selected, None)
                        .await
                        .unwrap(),
                    SessionPatchOutcome::Conflict(_)
                ),
                "{state}"
            );
            assert_eq!(store.followup_queue("s").await.unwrap(), before_queue);
            assert_eq!(store.chat_snapshot("s").await.unwrap(), before_chat);
            assert_eq!(store.context("s").await.unwrap(), before_context);
            assert!(store
                .get("receipt:session:history:s:blocked-undo")
                .await
                .unwrap()
                .is_none());
        }
    }
    /// Verify a grouped steering bubble cannot be undone independently of its consuming run.
    #[tokio::test]
    async fn undo_of_delivered_steering_bubble_preserves_original_run_context() {
        let (_root, store) = fixture().await;
        let mut run = store.get("run:r2").await.unwrap().unwrap();
        run["status"] = json!("running");
        run["input"] = json!([{"role":"user","content":"original active run context"}]);
        store.put("run:r2", &run).await.unwrap();
        queue_history_message(&store, "steering", "new direction", false).await;
        store
            .followup_choice(
                "s",
                Some("steering"),
                0,
                "steer-choice",
                &json!({"choice":"steer"}),
            )
            .await
            .unwrap();
        assert!(store
            .followup_consume("s", "steer", &mut run, None)
            .await
            .unwrap());
        run["status"] = json!("completed");
        store.put("run:r2", &run).await.unwrap();
        let context = store.context("s").await.unwrap();
        assert!(json!(&context).to_string().contains("question r2"));
        assert!(json!(&context).to_string().contains("new direction"));
        let chat = store.chat_snapshot("s").await.unwrap();
        let selected = request(&store, "user:steering", "reject-grouped-undo").await;
        assert!(matches!(
            store
                .conversation_history("s", &selected, None)
                .await
                .unwrap(),
            SessionPatchOutcome::Conflict(_)
        ));
        assert_eq!(store.context("s").await.unwrap(), context);
        assert_eq!(store.chat_snapshot("s").await.unwrap(), chat);
        assert_eq!(
            store.followup_queue("s").await.unwrap()["entries"][0]["status"],
            "delivered"
        );
    }
    /// Verify Undo archives removed delivered followups and prevents their later redispatch after reopen.
    #[tokio::test]
    async fn ordinary_undo_archives_and_prunes_removed_terminal_followups() {
        let (root, store) = fixture().await;
        queue_history_message(&store, "after-r2", "completed followup", false).await;
        let mut run = queue_history_run("followup-run");
        assert!(store
            .followup_consume("s", "queue", &mut run, None)
            .await
            .unwrap());
        run["status"] = json!("completed");
        store.put("run:followup-run", &run).await.unwrap();
        let selected = request(&store, "user:r2", "undo-original").await;
        saved(
            store
                .conversation_history("s", &selected, None)
                .await
                .unwrap(),
        );
        let reopened = Store::open(root.path()).await.unwrap();
        assert!(reopened.followup_queue("s").await.unwrap()["entries"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(reopened.chat_snapshot("s").await.unwrap().0.len(), 2);
        assert_eq!(reopened.context("s").await.unwrap().len(), 2);
        let archive = reopened
            .get("receipt:session:history:s:undo-original")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(archive["archive"]["followups"][0]["id"], "after-r2");
        assert_eq!(archive["archive"]["followups"][0]["status"], "delivered");
        let mut retry = queue_history_run("must-not-deliver");
        assert!(!reopened
            .followup_consume("s", "queue", &mut retry, None)
            .await
            .unwrap());
    }
    /// Verify an earlier fork cutoff excludes pending and subsequently admitted followups without changing the source queue.
    #[tokio::test]
    async fn fork_cutoff_omits_unconsumed_and_later_admitted_followups() {
        let (_root, store) = fixture().await;
        queue_history_message(&store, "pending", "must remain queued only in source", true).await;
        queue_history_message(&store, "later", "admitted after chosen reply", true).await;
        let mut tx = store.pool.begin().await.unwrap();
        store
            .insert_working_message(
                &mut tx,
                "s",
                "r2",
                "working:r2:fork-cutoff",
                "Earlier progress",
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        store
            .followup_choice(
                "s",
                Some("later"),
                0,
                "later-interrupt",
                &json!({"choice":"interrupt"}),
            )
            .await
            .unwrap();
        let mut later_run = queue_history_run("later-run");
        assert!(store
            .followup_consume("s", "interrupt", &mut later_run, None)
            .await
            .unwrap());
        let before = store.followup_queue("s").await.unwrap();
        let selected = request(&store, "working:r2:fork-cutoff", "fork-earlier").await;
        saved(
            store
                .conversation_history(
                    "s",
                    &selected,
                    Some((
                        json!({"id":"earlier-child","agent_id":"shared_default","title":"Fork"}),
                        json!({}),
                        100,
                    )),
                )
                .await
                .unwrap(),
        );
        assert_eq!(store.followup_queue("s").await.unwrap(), before);
        assert!(
            store.followup_queue("earlier-child").await.unwrap()["entries"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let chats = store.chat_snapshot("earlier-child").await.unwrap().0;
        assert_eq!(chats.len(), 5);
        assert!(!json!(chats).to_string().contains("must remain queued"));
        assert!(!json!(store.context("earlier-child").await.unwrap())
            .to_string()
            .contains("admitted after chosen reply"));
        let copied_attachments: i64 =
            sqlx::query_scalar("SELECT count(*) FROM docs WHERE key LIKE 'attachment:%'")
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert_eq!(copied_attachments, 2);
    }
    /// Verify combined fork attachments are remapped everywhere while copied deliveries retain no executable queue authority.
    #[tokio::test]
    async fn fork_combined_attachments_remaps_every_section_without_queue_authority() {
        let (root, store) = fixture().await;
        let first = queue_history_message(&store, "first-file", "first input", true).await;
        let second = queue_history_message(&store, "second-file", "second input", true).await;
        let mut batch = queue_history_run("combined-run");
        assert!(store
            .followup_consume("s", "queue", &mut batch, None)
            .await
            .unwrap());
        let mut tx = store.pool.begin().await.unwrap();
        store
            .insert_interaction(
                &mut tx,
                "s",
                "combined-run",
                "model:1:0",
                &json!({"type":"message","role":"assistant","content":"Combined answer"}),
            )
            .await
            .unwrap();
        store
            .insert_chat(
                &mut tx,
                "s",
                "combined-run",
                "assistant",
                &json!("Combined answer"),
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        batch["status"] = json!("completed");
        store.put("run:combined-run", &batch).await.unwrap();
        let source_queue = store.followup_queue("s").await.unwrap();
        let selected = request(&store, "assistant:combined-run", "fork-combined").await;
        saved(
            store
                .conversation_history(
                    "s",
                    &selected,
                    Some((
                        json!({"id":"combined-child","agent_id":"shared_default","title":"Fork"}),
                        json!({}),
                        100,
                    )),
                )
                .await
                .unwrap(),
        );
        assert_eq!(store.followup_queue("s").await.unwrap(), source_queue);
        let reopened = Store::open(root.path()).await.unwrap();
        let q = reopened.followup_queue("combined-child").await.unwrap();
        assert_eq!(q["entries"].as_array().unwrap().len(), 2);
        let child_context = json!(reopened.context("combined-child").await.unwrap()).to_string();
        for old in [&first, &second] {
            assert!(!child_context.contains(old["attachments"][0]["id"].as_str().unwrap()));
        }
        for e in q["entries"].as_array().unwrap() {
            assert_eq!(e["status"], "delivered");
            for key in ["template", "review", "interrupt_committed", "target_run_id"] {
                assert!(e.get(key).is_none(), "{key}");
            }
            let aid = e["attachments"][0]["id"].as_str().unwrap();
            assert!(child_context.contains(aid));
            assert!(e["context_content"].as_str().unwrap().contains(aid));
            let attachment = reopened
                .get(&format!("attachment:{aid}"))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(attachment["session_id"], "combined-child");
            assert_eq!(attachment["run_id"], e["id"]);
            assert!(reopened
                .followup_choice(
                    "combined-child",
                    e["id"].as_str(),
                    0,
                    "cannot-requeue",
                    &json!({"choice":"interrupt"})
                )
                .await
                .is_err());
        }
        assert_eq!(
            reopened
                .chat_snapshot("combined-child")
                .await
                .unwrap()
                .0
                .len(),
            7
        );
        let mut forbidden =
            json!({"id":"must-not-run","session_id":"combined-child","status":"queued","input":[]});
        assert!(!reopened
            .followup_consume("combined-child", "queue", &mut forbidden, None)
            .await
            .unwrap());
    }
}
