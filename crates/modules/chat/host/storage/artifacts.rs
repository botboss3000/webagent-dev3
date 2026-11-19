//! Session artifacts: what the agent produced in a session (the agreed plan first), one row
//! each in the session owner's database. Only ids, kind, size and times are plain columns;
//! the title and content are an encrypted, compressed user record. Rows are erased with
//! their session and count toward the owner's storage like the rest of the database.
use super::*;

/// Encoded size limit of one artifact body.
const MAX_ARTIFACT_BYTES: usize = 64 * 1024;
/// Artifacts one session keeps.
const MAX_SESSION_ARTIFACTS: i64 = 200;

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Store {
    /// Save artifact `id` of `kind` with `body` for the executing tool call `pending_id` of run
    /// `rid`; a later save of the same id replaces it with the next revision. A retried call
    /// returns its first result through the call's receipt.
    pub(crate) async fn session_artifact_write(
        &self,
        sid: &str,
        rid: &str,
        pending_id: &str,
        generation: &str,
        kind: &str,
        id: &str,
        body: &Value,
    ) -> Result<Value> {
        if !valid_id(id) || !valid_id(kind) || !valid_id(pending_id) {
            return Err(anyhow!("Invalid session artifact identifier"));
        }
        let sealed = self.encode_user_record(body)?;
        if sealed.len() > MAX_ARTIFACT_BYTES {
            return Err(anyhow!("Session artifact exceeds 64 KiB"));
        }
        // The receipt shares the session's base namespace, so it is erased with the session.
        let receipt_key = format!("receipt:base:{sid}:{rid}:{pending_id}:artifact");
        let fingerprint = crate::digest(&json!([kind, id, body]));
        let _guard = self.context_gate.write().await;
        let mut tx = self.pool.begin().await?;
        if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&receipt_key)
            .fetch_optional(&mut *tx)
            .await?
        {
            let receipt = self.unseal(&raw)?;
            if receipt["fingerprint"] != fingerprint {
                return Err(anyhow!("Artifact call ID reused with different input"));
            }
            let output = receipt["output"].clone();
            tx.commit().await?;
            return Ok(output);
        }
        self.base_run_fence(&mut tx, sid, rid, Some(pending_id), Some(generation))
            .await?;
        let prior: Option<(i64, i64)> = sqlx::query_as(
            "SELECT revision,created_at FROM session_artifacts WHERE session_id=? AND artifact_id=?",
        )
        .bind(sid)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        if prior.is_none() {
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM session_artifacts WHERE session_id=?")
                    .bind(sid)
                    .fetch_one(&mut *tx)
                    .await?;
            if count >= MAX_SESSION_ARTIFACTS {
                return Err(anyhow!("This session already keeps 200 artifacts"));
            }
        }
        let now = crate::now() as i64;
        let revision = prior.map_or(1, |(revision, _)| revision + 1);
        let created_at = prior.map_or(now, |(_, created)| created);
        sqlx::query("INSERT INTO session_artifacts(session_id,artifact_id,kind,revision,run_id,bytes,body,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?) ON CONFLICT(session_id,artifact_id) DO UPDATE SET kind=excluded.kind,revision=excluded.revision,run_id=excluded.run_id,bytes=excluded.bytes,body=excluded.body,updated_at=excluded.updated_at")
            .bind(sid).bind(id).bind(kind).bind(revision).bind(rid)
            .bind(sealed.len() as i64).bind(&sealed).bind(created_at).bind(now)
            .execute(&mut *tx).await?;
        let output = json!({"id":id,"kind":kind,"revision":revision});
        let receipt = json!({"fingerprint":fingerprint,"output":output});
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(&receipt_key)
            .bind(self.encode_user_record(&receipt)?)
            .execute(&mut *tx)
            .await?;
        // Open chats re-read the session when its component revision moves.
        sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,'base_task_artifact',?)")
            .bind(sid)
            .bind(self.encode_user_record(&json!({"kind":"artifact","id":id,"revision":revision}))?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(output)
    }

    /// Save artifact `id` of `kind` with `body` for a user's request `request_id` (the plan's
    /// checklist after a task panel edit): no run is involved, so the caller has already
    /// checked the user's access to the session. A retried request returns its first result.
    pub(crate) async fn session_artifact_user_write(
        &self,
        sid: &str,
        request_id: &str,
        kind: &str,
        id: &str,
        body: &Value,
    ) -> Result<Value> {
        if !valid_id(id) || !valid_id(kind) || !valid_id(request_id) {
            return Err(anyhow!("Invalid session artifact identifier"));
        }
        let sealed = self.encode_user_record(body)?;
        if sealed.len() > MAX_ARTIFACT_BYTES {
            return Err(anyhow!("Session artifact exceeds 64 KiB"));
        }
        let receipt_key = format!("receipt:base:{sid}:user:{request_id}:artifact");
        let fingerprint = crate::digest(&json!([kind, id, body]));
        let _guard = self.context_gate.write().await;
        let mut tx = self.pool.begin().await?;
        if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&receipt_key)
            .fetch_optional(&mut *tx)
            .await?
        {
            let receipt = self.unseal(&raw)?;
            if receipt["fingerprint"] != fingerprint {
                return Err(anyhow!("Artifact request ID reused with different input"));
            }
            let output = receipt["output"].clone();
            tx.commit().await?;
            return Ok(output);
        }
        // A user edit only updates an artifact an agent run already created.
        let revision = sqlx::query_scalar::<_, i64>(
            "SELECT revision FROM session_artifacts WHERE session_id=? AND artifact_id=?",
        )
        .bind(sid)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| anyhow!("Session artifact not found"))?
            + 1;
        sqlx::query("UPDATE session_artifacts SET kind=?,revision=?,bytes=?,body=?,updated_at=? WHERE session_id=? AND artifact_id=?")
            .bind(kind).bind(revision).bind(sealed.len() as i64).bind(&sealed)
            .bind(crate::now() as i64).bind(sid).bind(id)
            .execute(&mut *tx).await?;
        let output = json!({"id":id,"kind":kind,"revision":revision});
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(&receipt_key)
            .bind(self.encode_user_record(&json!({"fingerprint":fingerprint,"output":output}))?)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,'base_task_artifact',?)")
            .bind(sid)
            .bind(self.encode_user_record(&json!({"kind":"artifact","id":id,"revision":revision}))?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(output)
    }

    /// The artifacts of session `sid`, oldest first, each with its decoded title and content.
    pub(crate) async fn session_artifacts(&self, sid: &str) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT artifact_id,kind,revision,run_id,body,created_at,updated_at FROM session_artifacts WHERE session_id=? ORDER BY created_at,artifact_id LIMIT 200",
        )
        .bind(sid)
        .fetch_all(&self.pool)
        .await?;
        let mut artifacts = Vec::with_capacity(rows.len());
        for row in rows {
            let body = self.unseal(&row.get::<Vec<u8>, _>("body"))?;
            artifacts.push(json!({"id":row.get::<String,_>("artifact_id"),
                "kind":row.get::<String,_>("kind"),"revision":row.get::<i64,_>("revision"),
                "run_id":row.get::<String,_>("run_id"),"created_at":row.get::<i64,_>("created_at"),
                "updated_at":row.get::<i64,_>("updated_at"),"title":body["title"],
                "sections":body["sections"],"element":body["element"],"params":body["params"]}));
        }
        Ok(artifacts)
    }
}
