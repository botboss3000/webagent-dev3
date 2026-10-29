//! User-owned Base task progress and declarative chat components.
use super::*;

const MAX_ARTIFACT_BYTES: usize = 24 * 1024;

fn artifact_key(sid: &str, kind: &str, id: &str) -> Result<String> {
    if sid.is_empty() || sid.len() > 128 || !sid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(anyhow!("Invalid task artifact identifier"));
    }
    match kind {
        "checkpoint" if id == "task" => Ok(format!("base_checkpoint:{sid}")),
        "component" => Ok(format!("base_component:{sid}:{id}")),
        "core_tool" => Ok(format!("core_tool:{id}")),
        "core_profile" => Ok(format!("core_profile:{id}")),
        "core_rating" => Ok(format!("core_rating:{id}")),
        "core_webhook" => Ok(format!("core_webhook:{id}")),
        "core_visual" => Ok(format!("core_visual:{id}")),
        _ => Err(anyhow!("Invalid task artifact kind")),
    }
}

impl Store {
    pub async fn base_task_checkpoint(&self, sid: &str, rid: &str) -> Result<Value> {
        let key = artifact_key(sid, "checkpoint", "task")?;
        let mut tx = self.pool.begin().await?;
        self.base_run_fence(&mut tx, sid, rid, None, None).await?;
        let raw = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(key).fetch_optional(&mut *tx).await?;
        let output = match raw {
            Some(raw) => self.unseal(&raw)?,
            None => json!({"schema_version":1,"revision":0,"artifact":null}),
        };
        tx.commit().await?;
        Ok(output)
    }

    // Stable pending-call receipts make recovery retries harmless even after
    // the run completes. The receipt, artifact and event commit together.
    pub async fn base_write_artifact(&self, sid: &str, rid: &str, pending_id: &str,
        generation: &str,
        kind: &str, id: &str, expected_revision: u64, artifact: &Value) -> Result<Value> {
        let key = artifact_key(sid, kind, id)?;
        if generation.is_empty() || generation.len()>128
            || rid.is_empty() || rid.len() > 128 || pending_id.is_empty() || pending_id.len() > 128
            || !rid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || !pending_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || serde_json::to_vec(artifact)?.len() > if kind=="core_visual"{96*1024}else{MAX_ARTIFACT_BYTES} {
            return Err(anyhow!("Task artifact exceeds bounds"));
        }
        let receipt_key = format!("receipt:base:{sid}:{rid}:{pending_id}");
        let fingerprint = crate::digest(&json!([kind,id,expected_revision,artifact]));
        let _guard = self.context_gate.write().await;
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE docs SET key=key WHERE key=?")
            .bind(format!("session:{sid}")).execute(&mut *tx).await?;
        if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&receipt_key).fetch_optional(&mut *tx).await? {
            let receipt = self.unseal(&raw)?;
            if receipt["fingerprint"] != fingerprint { return Err(anyhow!("Task call ID reused with different input")); }
            let output = receipt["output"].clone();
            tx.commit().await?;
            return Ok(output);
        }
        self.base_run_fence(&mut tx, sid, rid, Some(pending_id), Some(generation)).await?;
        let prior = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&key).fetch_optional(&mut *tx).await?
            .map(|raw| self.unseal(&raw)).transpose()?;
        let revision = prior.as_ref().and_then(|value| value["revision"].as_u64()).unwrap_or(0);
        if revision != expected_revision { return Err(anyhow!("Task artifact revision changed; reload before editing")); }
        let next = revision.checked_add(1).ok_or_else(|| anyhow!("Task revision exhausted"))?;
        let document = json!({"schema_version":1,"revision":next,"kind":kind,
            "id":id,"session_id":sid,"updated_by_run":rid,"artifact":artifact});
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(&key).bind(if kind=="core_webhook"{self.seal(&document)?}else{self.encode_user_record(&document)?}).execute(&mut *tx).await?;
        if kind=="core_visual"&&artifact["publish"]==true {
            let component_id=format!("visual-{id}");let component_key=artifact_key(sid,"component",&component_id)?;
            if artifact["deleted"]==true {
                sqlx::query("DELETE FROM docs WHERE key=?").bind(&component_key).execute(&mut *tx).await?;
            }else{
                let exists:i64=sqlx::query_scalar("SELECT count(*) FROM docs WHERE key=?").bind(&component_key).fetch_one(&mut *tx).await?;
                let count:i64=sqlx::query_scalar("SELECT count(*) FROM docs WHERE key LIKE ?").bind(format!("base_component:{sid}:%")).fetch_one(&mut *tx).await?;
                if exists==0&&count>=64{return Err(anyhow!("Conversation display quota reached"));}
                let component=json!({"schema_version":1,"revision":next,"kind":"component","id":component_id,"session_id":sid,"artifact":{"id":component_id,"type":"visual","title":artifact["name"],"placement":"inline","authority":"display_only","data":{"visual_id":id,"revision":next}}});
                sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").bind(component_key).bind(self.encode_user_record(&component)?).execute(&mut *tx).await?;
            }
        }
        let output = json!({"revision":next,"kind":kind,"id":id,"artifact":artifact});
        let receipt = json!({"fingerprint":fingerprint,"output":output});
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(&receipt_key).bind(if kind=="core_webhook"{self.seal(&receipt)?}else{self.encode_user_record(&receipt)?}).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,'base_task_artifact',?)")
            .bind(sid).bind(self.encode_user_record(&json!({"kind":kind,"id":id,"revision":next}))?)
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(output)
    }

    pub(crate) async fn base_component_revision(&self,sid:&str)->Result<u64>{
        let seq:i64=sqlx::query_scalar("SELECT COALESCE(MAX(seq),0) FROM events WHERE session_id=? AND kind='base_task_artifact'").bind(sid).fetch_one(&self.pool).await?;
        Ok(seq.max(0) as u64)
    }

    // Safe API projection: only previously validated declarative specs.
    pub async fn base_chat_components(&self, sid: &str) -> Result<Vec<Value>> {
        artifact_key(sid, "checkpoint", "task")?;
        let mut tx = self.pool.begin().await?;
        let session_raw = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(format!("session:{sid}")).fetch_optional(&mut *tx).await?
            .ok_or_else(|| anyhow!("Session unavailable"))?;
        if self.unseal(&session_raw)?["id"] != sid { return Err(anyhow!("Session unavailable")); }
        let rows = sqlx::query("SELECT value FROM docs WHERE key LIKE ? ORDER BY key LIMIT 65")
            .bind(format!("base_component:{sid}:%")).fetch_all(&mut *tx).await?;
        if rows.len() > 64 { return Err(anyhow!("Chat component count exceeds limit")); }
        let mut components = Vec::with_capacity(rows.len());
        for row in rows {
            let document = self.unseal(&row.get::<Vec<u8>,_>("value"))?;
            if document["schema_version"] != 1 || document["session_id"] != sid
                || document["kind"] != "component" { return Err(anyhow!("Invalid stored chat component")); }
            components.push(json!({"revision":document["revision"],"component":document["artifact"]}));
        }
        tx.commit().await?;
        Ok(components)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn checkpoint_cas_replay_and_run_fences() {
        let (_root,app)=crate::runtime_tests::fixture_uninitialized().await;
        let store=&app.store;
        for sid in ["owned","foreign"] {
            store.put(&format!("session:{sid}"),&json!({"id":sid,"agent_id":"shared_default",
                "status":"active","created_at":1})).await.unwrap();
        }
        let running=|sid:&str,state:&str|json!({"id":"r","session_id":sid,
            "agent_id":"shared_default","status":state,"generation":"g",
            "lease_epoch":0,"pending":{"id":"call-1","state":"executing"}});
        store.put("run:r",&running("owned","running")).await.unwrap();
        let artifact=json!({"summary":"Progress reported by agent","provenance":"agent_claim"});
        let first=store.base_write_artifact("owned","r","call-1","g","checkpoint","task",0,&artifact).await.unwrap();
        assert_eq!(first["revision"],1);
        assert_eq!(store.base_task_checkpoint("owned","r").await.unwrap()["revision"],1);
        assert_eq!(store.base_write_artifact("owned","r","call-1","g","checkpoint","task",0,&artifact).await.unwrap(),first);
        assert!(store.base_write_artifact("owned","r","call-1","g","checkpoint","task",0,&json!({"different":true})).await.is_err());
        // The next dispatched call has a fresh ID but must supply revision 1.
        let mut next_run=running("owned","running");
        next_run["pending"]["id"]=json!("call-2");
        store.put("run:r",&next_run).await.unwrap();
        assert!(store.base_write_artifact("owned","r","call-2","g","checkpoint","task",0,&artifact).await.is_err());
        assert!(store.base_write_artifact("owned","r","call-2","old-generation","checkpoint","task",1,&artifact).await.is_err());
        assert!(store.base_task_checkpoint("foreign","r").await.is_err());
        store.put("run:r",&running("owned","completed")).await.unwrap();
        // A committed write replays after completion; a new write is fenced.
        assert_eq!(store.base_write_artifact("owned","r","call-1","g","checkpoint","task",0,&artifact).await.unwrap(),first);
        assert!(store.base_write_artifact("owned","r","call-2","g","checkpoint","task",1,&artifact).await.is_err());
    }

    #[tokio::test]
    async fn component_projection_is_user_and_session_scoped() {
        let (_root,app)=crate::runtime_tests::fixture_uninitialized().await;
        let owner=app.store.create_user_account("base-owner","long-fixture-hash-placeholder").await.unwrap();
        let other=app.store.create_user_account("base-other","long-fixture-hash-placeholder").await.unwrap();
        let first=app.store.for_user(owner["user_id"].as_str().unwrap()).await.unwrap();
        let second=app.store.for_user(other["user_id"].as_str().unwrap()).await.unwrap();
        for store in [&first,&second] {
            store.put("session:s",&json!({"id":"s","status":"active","agent_id":"shared_default","created_at":1})).await.unwrap();
            store.put("run:r",&json!({"id":"r","session_id":"s","agent_id":"shared_default",
                "status":"running","generation":"g","lease_epoch":0,"pending":{"id":"call-1","state":"executing"}})).await.unwrap();
        }
        let display=json!({"id":"plan-overview","type":"status","title":"Plan",
            "data":{"sections":[{"label":"Goal","value":"Private"}]},"authority":"display_only"});
        first.base_write_artifact("s","r","call-1","g","component","plan-overview",0,&display).await.unwrap();
        let projected=first.base_chat_components("s").await.unwrap();
        assert_eq!(projected.len(),1);
        assert_eq!(projected[0]["component"]["data"]["sections"][0]["value"],"Private");
        assert!(second.base_chat_components("s").await.unwrap().is_empty());
        assert!(first.base_chat_components("missing").await.is_err());
    }
}
