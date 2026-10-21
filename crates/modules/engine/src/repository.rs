//! Engine-owned run checkpoints, generation/lease fencing and durable dispatcher protocol.
//! All foreign-owner effects use the caller's existing SQL transaction.
use anyhow::{anyhow,Result};
use serde_json::{json,Value};
use sqlx::{Row,SqlitePool};
use crate::ports::{Records,ChatEffects,AdmissionEffects,NotificationEffects,UsageEffects};

pub struct Repository<'a,R,H,Q,N,U> {
 pub(crate) root:&'a std::path::Path,
 pub(crate) pool:&'a SqlitePool,
 pub(crate) app_pool:&'a SqlitePool,
 pub(crate) agent_id:&'a str,
 pub(crate) records:&'a R,
 pub(crate) chat:&'a H,
 pub(crate) admission:&'a Q,
 pub(crate) notifications:&'a N,
 pub(crate) usage:&'a U,
}
impl<'a,R:Records,H:ChatEffects,Q:AdmissionEffects,N:NotificationEffects,U:UsageEffects> Repository<'a,R,H,Q,N,U> {
 pub fn new(root:&'a std::path::Path,pool:&'a SqlitePool,app_pool:&'a SqlitePool,agent_id:&'a str,records:&'a R,chat:&'a H,admission:&'a Q,notifications:&'a N,usage:&'a U)->Self {
  Self{root,pool,app_pool,agent_id,records,chat,admission,notifications,usage}
 }
 pub(crate) fn current_user_id(&self)->&str {self.records.current_user_id()}
 pub async fn read_run(&self,rid:&str)->Result<Option<Value>> {
  let mut tx=self.pool.begin().await?;
  let raw:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(format!("run:{rid}")).fetch_optional(&mut *tx).await?;
  let value=match raw {Some(raw)=>Some(self.records.decode_document(&mut tx,&raw).await?),None=>None};
  tx.commit().await?;Ok(value)
 }
    pub fn index_run<'s,'tx>(
        &'s self,
        tx: &'s mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        run: &'s Value,
    ) -> impl std::future::Future<Output=Result<()>>+Send+'s+use<'s,'tx,R,H,Q,N,U> where 'tx:'s {async move {
        crate::send_future(sqlx::query("INSERT INTO run_index(id,session_id,status,updated_at,enqueue_seq) VALUES(?,?,?,?,(SELECT COALESCE(MAX(enqueue_seq),0)+1 FROM run_index)) ON CONFLICT(id) DO UPDATE SET status=excluded.status,updated_at=excluded.updated_at,enqueue_seq=COALESCE(run_index.enqueue_seq,excluded.enqueue_seq)")
            .bind(run["id"].as_str().ok_or_else(||anyhow!("Invalid run"))?).bind(run["session_id"].as_str().unwrap_or(""))
            .bind(run["status"].as_str().unwrap_or("failed")).bind(run["updated_at"].as_i64().unwrap_or(0)).execute(&mut **tx)).await?;
        // Session totals advance in the same transaction as the run. Re-saving
        // a run contributes only its delta, so retries and recovery cannot double-count.
        let new_usage=crate::send_future(self.usage.stage_run_usage_in(tx,run)).await?;
        let retrieval = bounded_code_index_run_fields(run);
        let view = serde_json::json!({"usage":new_usage,"model":crate::output::public_run_model(run),"attachments":run["attachments"],"run_revision":run["run_revision"],"context_assembly_report":run["context_assembly_report"],"context_controls":run["context_controls"],"compactions":run["compactions"],"dispatch_mode":run["dispatch_mode"],"chat_model_revision":run["chat_model_revision"],"id":run["id"],"session_id":run["session_id"],"status":run["status"],"error":run["error"],
            "code_index_retrieval_attempted":retrieval["attempted"],"code_index_retrieval_status":retrieval["status"],"code_index_retrieval_stage":retrieval["stage"],
            "pending":run["pending"],"uncertain":run["uncertain"],"turns":run["turns"],"phase":run["phase"],"updated_at":run["updated_at"]});
        crate::send_future(sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(format!("run_view:{}",run["id"].as_str().unwrap())).bind(self.records.encode_user_record(&view)?).execute(&mut **tx)).await?;
        Ok(())
    }
    }

    pub async fn active_runs(&self) -> Result<Vec<Value>> {
        let rows=sqlx::query("SELECT d.value FROM run_index r JOIN docs d ON d.key='run_view:'||r.id WHERE r.status NOT IN ('completed','cancelled','failed') ORDER BY COALESCE(r.enqueue_seq,r.rowid) LIMIT 200")
            .fetch_all(self.pool).await?;
        rows.iter()
            .map(|r| self.records.unseal(&r.get::<Vec<u8>, _>("value")))
            .collect()
    }

    pub async fn recent_runs(&self, sid: Option<&str>) -> Result<Vec<Value>> {
        let rows = if let Some(sid) = sid {
            sqlx::query("SELECT d.value FROM run_index r JOIN docs d ON d.key='run_view:'||r.id WHERE r.session_id=? ORDER BY r.updated_at DESC LIMIT 100").bind(sid).fetch_all(self.pool).await?
        } else {
            sqlx::query("SELECT d.value FROM run_index r JOIN docs d ON d.key='run_view:'||r.id WHERE r.status='failed' ORDER BY r.updated_at DESC LIMIT 50").fetch_all(self.pool).await?
        };
        rows.iter()
            .map(|r| self.records.unseal(&r.get::<Vec<u8>, _>("value")))
            .collect()
    }

    pub async fn save_run(&self, run: &mut Value, kind: &str, payload: &Value) -> Result<()> {
        self.prepare_context_assets_operation(run).await?;
        self.save_run_inner(run,kind,payload,false).await
    }

    pub async fn save_run_override(&self, run:&mut Value, kind:&str, payload:&Value) -> Result<()> {
        if matches!(run["status"].as_str(),Some("cancelled"|"stopped"|"failed"|"recovery")) {
            // Stop/revocation must not wait for an unavailable pure planner.
            // Work on a clone so a rejected run fence retains caller staging for inspection.
            let mut closing=run.clone();
            closing.as_object_mut().ok_or_else(||anyhow!("Invalid run document"))?.remove("_context_assets_operation");
            closing.as_object_mut().unwrap().remove("_context_assets_write");
            self.save_run_inner(&mut closing,kind,payload,true).await?;
            *run=closing;
            return Ok(());
        }
        self.prepare_context_assets_operation(run).await?;
        self.save_run_inner(run,kind,payload,true).await
    }

    fn save_run_inner<'s>(&'s self, run:&'s mut Value, kind:&'s str, payload:&'s Value,
        allow_expired:bool) -> impl std::future::Future<Output=Result<()>>+Send+'s {async move {
        let rid = run["id"].as_str().ok_or_else(||anyhow!("Run has no id"))?.to_owned();
        let _context_guard = if kind == "context_compacted" {
            Some(crate::send_future(self.records.context_gate().write()).await)
        } else {
            None
        };
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE docs SET key=key WHERE key=?").bind(format!("run:{rid}")).execute(&mut *tx).await?;
        crate::send_future(self.chat.require_active_session(&mut tx, run["session_id"].as_str().ok_or_else(||anyhow!("Run has no session"))?)).await?;
        if !matches!(kind,"run_status") || !matches!(run["status"].as_str(),Some("cancelled"|"stopped"|"recovery"|"failed")) {
            crate::send_future(self.chat.require_participation(&mut tx,run)).await?;
        }
        let expected=run["run_revision"].as_u64().unwrap_or(0);
        if let Some(raw)=sqlx::query_scalar::<_,Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(format!("run:{rid}")).fetch_optional(&mut *tx).await? {
            // RECOVERY-FENCE-METADATA: active transitions compare only durable
            // metadata. Do not hydrate history merely to validate ownership.
            // Terminal idempotency compares the whole document, so retain the
            // corruption-checking reconstruction on that path. write_document
            // still verifies retained items before committing any input delta.
            let metadata = self.records.unseal(&raw)?;
            if metadata["history_removed"] == true { return Err(anyhow!("Run removed by Undo")); }
            let previous = if matches!(metadata["status"].as_str(),
                Some("completed" | "cancelled" | "failed")) {
                crate::send_future(self.records.decode_document(&mut tx, &raw)).await?
            } else { metadata };
            if previous["generation"] != run["generation"]
                || previous["lease_epoch"] != run["lease_epoch"]
                || previous["run_revision"].as_u64().unwrap_or(0)!=expected
                || (matches!(previous["status"].as_str(),Some("completed"|"cancelled"|"failed"))
                    && previous!=*run
                    // A late tool error may add validated evidence only. Terminal
                    // state/results stay immutable and every existing fence still applies.
                    && !(kind=="run_status" && crate::tools::failure::diagnostic_only_update(&previous,run)))
                || (previous["status"]!="running" && run["status"]=="running"
                    && previous["lease_epoch"].as_i64().unwrap_or(0)>0)
            {
                return Err(anyhow!("Run fence rejected stale transition"));
            }
            if previous==*run && run["status"]=="completed" {
                return Ok(());
            }
            if !allow_expired && previous["status"]=="running"
                && previous["lease_epoch"].as_i64().unwrap_or(0)>0 {
                let lease=sqlx::query("SELECT lease_epoch,lease_owner,lease_expires_at FROM run_index WHERE id=?")
                    .bind(&rid).fetch_one(&mut *tx).await?;
                let until:i64=lease.get("lease_expires_at");
                let lease_epoch:i64=lease.get("lease_epoch");
                let lease_owner:Option<String>=lease.get("lease_owner");
                let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
                if until<now || lease_epoch!=run["lease_epoch"].as_i64().unwrap_or(0)
                    || lease_owner.as_deref()!=run["lease_owner"].as_str() {
                    return Err(anyhow!("Run lease expired or changed"));
                }
            }
        } else if expected!=0 {
            return Err(anyhow!("Run revision missing"));
        }
        if run["status"]=="completed" {
            crate::send_future(self.chat.followup_publication_guard(&mut tx,run["session_id"].as_str().unwrap_or(""))).await?;
        }
        // Stage computation happened before writer admission. Ledger CAS and the
        // successful result checkpoint now share every existing run ownership fence.
        crate::send_future(self.apply_context_assets_write(&mut tx,run)).await?;
        if kind=="context_compacted" {
            crate::send_future(self.reset_context_asset_layouts(&mut tx,run["session_id"].as_str().unwrap())).await?;
        }
        let mut persisted=run.clone();
        if let Some(revision)=run["_context_assets_write"]["state"]["revision"].as_u64() {
            persisted["context_assets_revision"]=json!(revision);
        }
        persisted.as_object_mut().unwrap().remove("_context_assets_write");
        persisted.as_object_mut().unwrap().remove("_context_assets_operation");
        if let Some(id)=run["input"].as_array().and_then(|items|items.last()).filter(|item|item["type"]=="function_call_output").and_then(|item|item["call_id"].as_str()) {
            if let Some(map)=persisted["_tool_execution_metadata"].as_object_mut(){map.remove(id);}
        }
        persisted["run_revision"]=json!(expected+1);
        crate::send_future(self.records.write_document(&mut tx,&format!("run:{rid}"),&persisted)).await?;
        // Large model and tool outputs live in interactions and the run checkpoint.
        if kind != "model_output" && kind != "tool_result" {
            let event_payload = if kind == "context_compacted" {
                json!({"run_id":rid,"segment_id":payload["segment_id"],
                    "source_digest":payload["source_digest"],
                    "dropped_items":payload["dropped_items"]})
            } else { payload.clone() };
            sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,?,?)")
                .bind(run["session_id"].as_str().unwrap())
                .bind(kind)
                .bind(self.records.encode_user_record(&event_payload)?)
                .execute(&mut *tx)
                .await?;
        }
        let sid = run["session_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Run has no session"))?;
        let rid = run["id"].as_str().ok_or_else(|| anyhow!("Run has no id"))?;
        if let Some(item) = run["input"].as_array().and_then(|items| items.last()) {
            if item["type"] == "function_call_output" {
                let call_id = item["call_id"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Tool output has no call ID"))?;
                crate::send_future(self.chat.insert_interaction(&mut tx, sid, rid, &format!("tool:{call_id}"), item))
                    .await?;
                if let Some(metadata)=run["_tool_execution_metadata"].get(call_id) {
                    crate::send_future(self.chat.attach_execution_metadata(&mut tx,sid,rid,call_id,metadata)).await?;
                }
            }
        }
        if kind == "working_message" && run["module_silent"]!=true {
            // The private control result and public bubble commit together.
            let message_id = payload["id"].as_str()
                .ok_or_else(|| anyhow!("Working message has no ID"))?;
            let text = payload["text"].as_str()
                .ok_or_else(|| anyhow!("Working message has no text"))?;
            crate::send_future(self.chat.insert_working_message(&mut tx,sid,rid,message_id,text)).await?;
        }
        if kind == "model_output" {
            let output = payload["output"]
                .as_array()
                .ok_or_else(|| anyhow!("Model output is missing"))?;
            let turn = run["turns"]
                .as_u64()
                .ok_or_else(|| anyhow!("Run turn is missing"))?;
            for (ordinal, item) in output.iter().enumerate() {
                crate::send_future(self.chat.insert_interaction(
                    &mut tx,
                    sid,
                    rid,
                    &format!("model:{turn}:{ordinal}"),
                    item,
                ))
                .await?;
                if run["module_silent"]!=true && payload["auto_promote"] == true && item["type"] == "message" {
                    if run["status"] == "completed" {
                        return Err(anyhow!("Final reply cannot be auto promoted"));
                    }
                    let text=item["content"].as_array()
                        .map(|parts|parts.iter().filter_map(|part|part["text"].as_str())
                            .collect::<Vec<_>>().join(""))
                        .unwrap_or_default();
                    if !text.trim().is_empty() {
                        let message_id=format!("working:{rid}:auto:{turn}:{ordinal}");
                        // The interaction and its public bubble share this transaction.
                        crate::send_future(self.chat.insert_working_message(&mut tx,sid,rid,&message_id,&text)).await?;
                    }
                }
            }
            if run["status"] == "completed" {
                let text = payload["terminal_text"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Terminal reply is missing"))?;
                if text.trim().is_empty() {
                    return Err(anyhow!("Terminal reply is empty"));
                }
                if run["module_silent"]!=true {crate::send_future(self.chat.insert_chat(&mut tx, sid, rid, "assistant", &json!(text)))
                    .await?;}
                // Naming is queued with terminal publication, never a detached
                // after-commit hook that could be lost during restart.
                if run["module_silent"]!=true {crate::send_future(self.chat.queue_session_naming(&mut tx, sid, run)).await?;}
            }
        }
        crate::send_future(self.background_claim_delivery(&mut tx,run)).await?;
        if kind == "context_compacted" {
            let session_key = format!("session:{sid}");
            let row = sqlx::query("SELECT value FROM docs WHERE key=?")
                .bind(&session_key)
                .fetch_one(&mut *tx)
                .await?;
            let mut session = self.records.unseal(&row.get::<Vec<u8>, _>("value"))?;
            let through = sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(MAX(seq),0) FROM interactions WHERE session_id=?",
            )
            .bind(sid)
            .fetch_one(&mut *tx)
            .await?;
            session["checkpoint_input"] = run["input"].clone();
            session["checkpoint_through"] = json!(through);
            // The original source and the new checkpoint share this user-plane
            // transaction. A recall segment cannot appear without its summary.
            let source=payload["source"].as_array()
                .ok_or_else(|| anyhow!("Compaction source missing"))?;
            let source_bytes=serde_json::to_vec(source)?;
            let source_digest=payload["source_digest"].as_str()
                .ok_or_else(|| anyhow!("Compaction source digest missing"))?;
            let segment_id=payload["segment_id"].as_str()
                .ok_or_else(|| anyhow!("Compaction segment identity missing"))?;
            if source.is_empty() || source.len()!=payload["dropped_items"].as_u64().unwrap_or(0) as usize
                || source_bytes.len()>262_144 || crate::digest(source)!=source_digest
                || segment_id!=format!("{}-{}",rid,run["compactions"].as_u64().unwrap_or(0)) {
                return Err(anyhow!("Invalid compaction segment provenance"));
            }
            let created_at=crate::now();
            // The discarded prefix is kept only in memory. Recall and events
            // persist no reasoning; a durable hash-only job cleans older copies.
            let clean=crate::context_compaction::recall_source(source);
            let clean_digest=crate::digest(&clean);
            let hashes=self.records.reasoning_hashes(source)?;
            let segment_bytes_avoided=source_bytes.len().saturating_sub(serde_json::to_vec(&clean)?.len());
            let event_bytes_avoided=serde_json::to_vec(payload)?.len().saturating_sub(
                serde_json::to_vec(&json!({"run_id":rid,"segment_id":segment_id,
                    "source_digest":source_digest,"dropped_items":source.len()}))?.len());
            let segment=json!({"schema_version":2,"session_id":sid,"segment_id":segment_id,
                "source_digest":clean_digest,"original_source_digest":source_digest,
                "source":clean,"created_at":created_at});
            sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
                .bind(format!("compaction_segment:{sid}:{segment_id}"))
                .bind(self.records.encode_user_record(&segment)?).execute(&mut *tx).await?;
            if !hashes.is_empty() {
                let job=json!({"session_id":sid,"segment_id":segment_id,
                    "source_digest":source_digest,"hashes":hashes,
                    "dropped_items":source.len(),
                    "segment_bytes_avoided":segment_bytes_avoided,
                    "event_bytes_avoided":event_bytes_avoided});
                sqlx::query("INSERT INTO reasoning_cleanup(segment_id,session_id,payload) VALUES(?,?,?)")
                    .bind(segment_id).bind(sid).bind(self.records.encode_user_record(&job)?)
                    .execute(&mut *tx).await?;
            }
            let mut entries=session["compaction_segments"].as_array().cloned().unwrap_or_default();
            if entries.len()>=32 {entries.remove(0);}
            entries.push(json!({"segment_id":segment_id,"source_digest":clean_digest,
                "item_count":clean.len(),"created_at":created_at}));
            session["compaction_segments"]=json!(entries);
            sqlx::query("UPDATE docs SET value=? WHERE key=?")
                .bind(self.records.encode_user_record(&session)?)
                .bind(&session_key)
                .execute(&mut *tx)
                .await?;
        }
        // Projection assignments share the canonical run lease/history transaction, never the tool journal.
        if let Some(state)=run.get("tool_result_projection") {
            let session_key=format!("session:{sid}");
            if let Some(raw)=sqlx::query_scalar::<_,Vec<u8>>("SELECT value FROM docs WHERE key=?")
                .bind(&session_key).fetch_optional(&mut *tx).await? {
                let mut session=self.records.unseal(&raw)?;
                if state["revision"].as_u64().unwrap_or(0)>=session["tool_result_projection"]["revision"].as_u64().unwrap_or(0) {
                    session["tool_result_projection"]=state.clone();
                    sqlx::query("UPDATE docs SET value=? WHERE key=?").bind(self.records.encode_user_record(&session)?)
                        .bind(&session_key).execute(&mut *tx).await?;
                }
            }
        }
        // Session metadata shares the existing run lease/revision transaction.
        if let Some(state)=run.get("tool_context_state") {
            let session_key=format!("session:{sid}");
            if let Some(raw)=sqlx::query_scalar::<_,Vec<u8>>("SELECT value FROM docs WHERE key=?")
                .bind(&session_key).fetch_optional(&mut *tx).await? {
                let mut session=self.records.unseal(&raw)?;
                if state["clock"].as_u64().unwrap_or(0)>=session["tool_context_state"]["clock"].as_u64().unwrap_or(0) {
                    session["tool_context_state"]=state.clone();
                    sqlx::query("UPDATE docs SET value=? WHERE key=?").bind(self.records.encode_user_record(&session)?)
                        .bind(&session_key).execute(&mut *tx).await?;
                }
            }
        }
        crate::send_future(self.notifications.stage_run_notification(&mut tx,&persisted)).await?;
        crate::send_future(self.index_run(&mut tx, &persisted)).await?;
        sqlx::query("INSERT INTO loop_events(run_id,schema_version,kind,payload,created_at) VALUES(?,1,?,?,strftime('%s','now'))")
            .bind(rid).bind(kind).bind(self.records.encode_user_record(&json!({"schema_version":1,"run_id":rid,
                "generation":run["generation"],"lease_epoch":run["lease_epoch"],
                "status":run["status"],"kind":kind}))?)
            .execute(&mut *tx).await?;
        if run["status"]=="completed" {
            sqlx::query("INSERT OR IGNORE INTO publication_outbox(run_id,schema_version,payload,created_at) VALUES(?,1,?,strftime('%s','now'))")
                .bind(rid).bind(self.records.encode_user_record(&json!({"schema_version":1,"run_id":rid,
                    "generation":run["generation"],"status":"completed",
                    "notification_intent":run["closing"]}))?)
                .execute(&mut *tx).await?;
        }
        tx.commit().await?;
        self.records.invalidate(&[
            format!("run:{}", run["id"].as_str().unwrap()),
            format!("session:{}", run["session_id"].as_str().unwrap()),
        ]);
        if kind == "context_compacted" {
            self.records.invalidate_context(sid);
        }
        // Retain staging after any failure; clear only after the complete transaction commits.
        if let Some(revision)=persisted.get("context_assets_revision") {run["context_assets_revision"]=revision.clone();}
        run.as_object_mut().unwrap().remove("_context_assets_write");
        run.as_object_mut().unwrap().remove("_context_assets_operation");
        if let Some(map)=persisted.get("_tool_execution_metadata"){run["_tool_execution_metadata"]=map.clone();}
        run["run_revision"]=json!(expected+1);
        if matches!(run["status"].as_str(),Some("completed"|"cancelled"|"failed")) {crate::send_future(self.admission.reconcile_requester_slots()).await?;}
        Ok(())
    }
    }

    pub fn claim_run<'s>(&'s self, run: &'s mut Value, owner: &'s str, generation: &'s str) -> impl std::future::Future<Output=Result<bool>>+Send+'s {async move {
        let rid=run["id"].as_str().ok_or_else(||anyhow!("Run has no id"))?.to_owned();
        let Some(saved)=crate::send_future(self.read_run(&rid)).await? else {return Ok(false)};
        if saved["status"]!="queued" {return Ok(false);}
        let mut lifecycle=self.pool.begin().await?;
        crate::send_future(self.chat.require_active_session(&mut lifecycle,saved["session_id"].as_str().ok_or_else(||anyhow!("Run session missing"))?)).await?;
        lifecycle.commit().await?;
        if !crate::send_future(self.admission.requester_claim_ready(&saved)).await? {return Ok(false);}
        let mut tx=self.pool.begin().await?;
        let raw:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(format!("run:{rid}")).fetch_optional(&mut *tx).await?;
        let Some(raw)=raw else {return Ok(false)};
        let previous=crate::send_future(self.records.decode_document(&mut tx,&raw)).await?;
        // Backoff belongs to the durable claim, so restarts or competing loop
        // processes cannot bypass a queued provider retry's cooldown.
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
        if previous["provider_retry_at"].as_u64().is_some_and(|due| due > now) { return Ok(false); }
        crate::send_future(self.chat.require_active_session(&mut tx,previous["session_id"].as_str().ok_or_else(||anyhow!("Run session missing"))?)).await?;
        if crate::send_future(self.chat.require_participation(&mut tx,&previous)).await.is_err() {return Ok(false);}
        if previous["status"]!="queued" || previous["generation"]!=run["generation"]
            || previous["lease_epoch"]!=run["lease_epoch"]
            || previous["run_revision"]!=run["run_revision"] {return Ok(false);}
        let epoch:i64=sqlx::query_scalar("SELECT lease_epoch FROM run_index WHERE id=?")
            .bind(&rid).fetch_one(&mut *tx).await?;
        let mut claimed=run.clone();
        claimed["lease_epoch"]=json!(epoch+1);
        claimed["lease_owner"]=json!(owner);
        claimed["generation"]=json!(generation);
        claimed["status"]=json!("running");
        claimed["run_revision"]=json!(previous["run_revision"].as_u64().unwrap_or(0)+1);
        let until=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64+30;
        crate::send_future(self.records.write_document(&mut tx,&format!("run:{rid}"),&claimed)).await?;
        self.index_run(&mut tx,&claimed).await?;
        crate::send_future(self.notifications.stage_notification(&mut tx,&format!("started:{rid}"),"run.started",claimed["session_id"].as_str().unwrap(),Some(&rid),Some("running"))).await?;
        sqlx::query("UPDATE run_index SET lease_epoch=?,lease_owner=?,lease_expires_at=? WHERE id=?")
            .bind(epoch+1).bind(owner).bind(until).bind(&rid).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO loop_events(run_id,schema_version,kind,payload,created_at) VALUES(?,1,'claimed',?,strftime('%s','now'))")
            .bind(&rid).bind(self.records.encode_user_record(&json!({"schema_version":1,"run_id":rid,
                "generation":generation,"lease_epoch":epoch+1,"kind":"claimed"}))?)
            .execute(&mut *tx).await?;
        tx.commit().await?;
        *run=claimed;
        self.records.invalidate(&[format!("run:{rid}")]);
        Ok(true)
    }
    }

    pub async fn renew_run_lease(&self, rid:&str, generation:&str, owner:&str, epoch:i64) -> Result<bool> {
        let mut tx=self.pool.begin().await?;
        // Reserve the SQLite writer before reading authority. Otherwise a queue
        // commit can stale this WAL snapshot and its later UPDATE fails with
        // BUSY_SNAPSHOT, which the lease watcher treats as lost ownership.
        sqlx::query("UPDATE run_index SET lease_expires_at=lease_expires_at WHERE id=?")
            .bind(rid).execute(&mut *tx).await?;
        let raw:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(format!("run:{rid}")).fetch_optional(&mut *tx).await?;
        let Some(raw)=raw else {return Ok(false)};
        let run=self.records.unseal(&raw)?;
        if run["generation"]!=generation || run["status"]!="running"
            || run["lease_epoch"]!=epoch {return Ok(false);}
        let changed=sqlx::query("UPDATE run_index SET lease_expires_at=? WHERE id=? AND lease_owner=? AND lease_epoch=?")
            .bind(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64+30).bind(rid).bind(owner).bind(epoch)
            .execute(&mut *tx).await?.rows_affected();
        tx.commit().await?;
        Ok(changed==1)
    }

    pub async fn write_loop_heartbeat(&self, owner:&str) -> Result<()> {
        sqlx::query("INSERT INTO loop_heartbeat(singleton,owner,protocol_version,updated_at) VALUES(1,?,1,strftime('%s','now')) ON CONFLICT(singleton) DO UPDATE SET owner=excluded.owner,protocol_version=1,updated_at=excluded.updated_at")
            .bind(owner).execute(self.app_pool).await?;
        Ok(())
    }

    pub async fn loop_fresh(&self, seconds:i64) -> Result<bool> {
        let age:Option<i64>=sqlx::query_scalar(
            "SELECT CAST(strftime('%s','now') AS INTEGER)-updated_at FROM loop_heartbeat WHERE singleton=1 AND protocol_version=1")
            .fetch_optional(self.app_pool).await?;
        Ok(age.is_some_and(|age|age>=0 && age<=seconds))
    }

    pub async fn clear_loop_heartbeat(&self) -> Result<()> {
        sqlx::query("UPDATE loop_heartbeat SET updated_at=0 WHERE singleton=1")
            .execute(self.app_pool).await?;
        Ok(())
    }

    pub async fn acknowledge_loop_commands(&self) -> Result<usize> {
        let mut tx=self.pool.begin().await?;
        let rows=sqlx::query("SELECT seq,run_id,schema_version,kind,payload FROM loop_commands WHERE acknowledged_at IS NULL ORDER BY seq LIMIT 128")
            .fetch_all(&mut *tx).await?;
        for row in &rows {
            let seq:i64=row.get("seq");
            let run_id:String=row.get("run_id");
            let version:i64=row.get("schema_version");
            let kind:String=row.get("kind");
            let payload=self.records.unseal(&row.get::<Vec<u8>,_>("payload"))?;
            if version!=1 || payload["schema_version"]!=1 || payload["run_id"]!=run_id
                || payload["kind"]!=kind {
                return Err(anyhow!("Unsupported or malformed loop command"));
            }
            sqlx::query("UPDATE loop_commands SET acknowledged_at=strftime('%s','now') WHERE seq=? AND acknowledged_at IS NULL")
                .bind(seq).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO loop_events(run_id,schema_version,kind,payload,created_at) VALUES(?,1,'command_ack',?,strftime('%s','now'))")
                .bind(&run_id).bind(self.records.encode_user_record(&json!({"schema_version":1,
                    "run_id":run_id,"command_seq":seq,"kind":"command_ack"}))?)
                .execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(rows.len())
    }
}

fn bounded_code_index_run_fields(run: &Value) -> Value {
    let attempted = run["code_index_retrieval_attempted"] == true;
    let status = if !attempted {
        "not_attempted"
    } else {
        match run["code_index_retrieval_status"].as_str() {
            Some("attached") => "attached",
            Some("attached_candidates") => "attached_candidates",
            Some("no_matches" | "disabled_or_no_matches") => "disabled_or_no_matches",
            Some("access_changed") => "access_changed",
            Some("timeout") => "timeout",
            Some("unavailable") => "unavailable",
            _ => "unavailable",
        }
    };
    let stage = if !attempted {
        "not_started"
    } else {
        match run["code_index_retrieval_stage"].as_str() {
            Some("preflight") => "preflight",
            Some("lookup") => "lookup",
            Some("pre_jev") => "pre_jev",
            Some("jev_parse") => "jev_parse",
            Some("snippet") => "snippet",
            Some("authorization") => "authorization",
            Some("complete") => "complete",
            Some("unknown") => "unknown",
            _ => "unknown",
        }
    };
    json!({"attempted":attempted,"status":status,"stage":stage})
}


mod background;
mod background_followup;
