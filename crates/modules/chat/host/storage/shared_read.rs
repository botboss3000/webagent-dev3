//! Personal read cursors stored atomically with the canonical conversation.
use super::*;
impl Store {
    pub(crate) async fn shared_read_summary(&self, sid: &str, actor: &str) -> Result<Value> {
        if actor == self.current_user_id() {
            return self.session_read_summary(sid).await;
        }
        let mut tx = self.pool.begin().await?;
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(format!("session:{sid}"))
            .fetch_one(&mut *tx)
            .await?;
        let mut session = self.unseal(&raw)?;
        session["read_through_seq"] =
            json!(session["participant_reads"][actor].as_u64().unwrap_or(0));
        let value = self.read_summary_in(&mut tx, sid, &session).await?;
        tx.commit().await?;
        Ok(value)
    }
    pub(crate) async fn shared_ack_read(
        &self,
        sid: &str,
        actor: &str,
        through: u64,
    ) -> Result<session::SessionPatchOutcome> {
        if actor == self.current_user_id() {
            return self.acknowledge_session_read(sid, through).await;
        }
        let Ok(seq) = i64::try_from(through) else {
            return Ok(session::SessionPatchOutcome::Invalid("Invalid read cursor"));
        };
        let key = format!("session:{sid}");
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE docs SET key=key WHERE key=?")
            .bind(&key)
            .execute(&mut *tx)
            .await?;
        let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&key)
            .fetch_optional(&mut *tx)
            .await?
        else {
            return Ok(session::SessionPatchOutcome::NotFound);
        };
        let mut value = self.unseal(&raw)?;
        if value["status"] != "active" {
            return Ok(session::SessionPatchOutcome::Conflict(
                "Session is recycled",
            ));
        }
        if seq != 0 {
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM chat WHERE session_id=? AND seq=?")
                    .bind(sid)
                    .bind(seq)
                    .fetch_one(&mut *tx)
                    .await?;
            if count != 1 {
                return Ok(session::SessionPatchOutcome::Invalid(
                    "Read cursor is not a message in this session",
                ));
            }
        }
        let previous = value["participant_reads"][actor].as_u64().unwrap_or(0);
        if through > previous {
            if !value["participant_reads"].is_object() {
                value["participant_reads"] = json!({});
            }
            value["participant_reads"][actor] = json!(through);
            self.write_document(&mut tx, &key, &value).await?;
        }
        let mut projected = value;
        projected["read_through_seq"] = json!(through.max(previous));
        let summary = self.read_summary_in(&mut tx, sid, &projected).await?;
        tx.commit().await?;
        self.cache.lock().unwrap().invalidate(&[key]);
        Ok(session::SessionPatchOutcome::Saved(
            json!({"session":summary}),
        ))
    }
}
