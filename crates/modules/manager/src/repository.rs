//! Durable review transitions. Only acceptance creates model context and an agent run.
use crate::{storage::{Repository,RecordEffects},Verdict};
use anyhow::{anyhow,Result};
use serde_json::{json,Value};
use crate::runtime::now;
impl<E:RecordEffects> Repository<'_,E> {
    pub fn claim_manager_review<'s>(&'s self, id: &'s str) -> impl std::future::Future<Output=Result<Option<Value>>>+Send+'s {async move {
        let key = format!("manager_review:{id}");
        let Some(saved)=self.effects.read_document(&key).await? else {return Ok(None)};
        if saved["status"]!="pending_review" || !self.effects.requester_claim_ready(&saved["run"]).await? {return Ok(None);}
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE docs SET value=value WHERE key=?")
            .bind(&key)
            .execute(&mut *tx)
            .await?;
        let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&key)
            .fetch_optional(&mut *tx)
            .await?
        else {
            return Ok(None);
        };
        let mut record = self.effects.unseal(&raw)?;
        if record["status"] != "pending_review" {
            return Ok(None);
        }
        crate::runtime::send_future(self.effects.require_active_session_in(&mut tx,record["session_id"].as_str().ok_or_else(||anyhow!("Missing review session"))?)).await?;
        record["status"] = json!("reviewing");
        record["generation"] = json!(uuid::Uuid::new_v4().to_string());
        record["deadline"] = json!(now() + 50);
        crate::runtime::send_future(self.effects.write_document_in(&mut tx, &key, &record)).await?;
        tx.commit().await?;
        self.effects.invalidate_documents(&[key]);
        Ok(Some(record))
    }
    }
    pub fn finish_manager_review<'s>(
        &'s self,
        observed: &'s Value,
        state: &'s str,
        verdict: Option<Verdict>,
    ) -> impl std::future::Future<Output=Result<bool>>+Send+'s {async move {
        if !matches!(state, "acceptable" | "unacceptable" | "review_error") {
            return Err(anyhow!("Invalid manager state"));
        }
        let id = observed["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Review ID missing"))?;
        let sid = observed["session_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Review session missing"))?;
        let key = format!("manager_review:{id}");
        let pointer = format!("manager_pending:{sid}");
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE docs SET value=value WHERE key=?")
            .bind(&key)
            .execute(&mut *tx)
            .await?;
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(&key)
            .fetch_one(&mut *tx)
            .await?;
        let mut record = self.effects.unseal(&raw)?;
        if record["status"] != "reviewing" || record["generation"] != observed["generation"] {
            return Ok(false);
        }
        let pending: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(&pointer)
            .fetch_optional(&mut *tx)
            .await?;
        if pending
            .as_ref()
            .map(|v| self.effects.unseal(v))
            .transpose()?
            .is_none_or(|v| v["id"] != id)
        {
            return Ok(false);
        }
        if (state == "unacceptable" && verdict != Some(Verdict::Unacceptable))
            || (state == "review_error" && verdict.is_some())
        {
            return Err(anyhow!("Review state and verdict disagree"));
        }
        let acceptable = state == "acceptable";
        if acceptable {
            if verdict != Some(Verdict::Acceptable) {
                return Err(anyhow!("Acceptable verdict required"));
            }
            if record["deadline"].as_u64().unwrap_or(0) <= now() {
                return Err(anyhow!("Manager review lease expired"));
            }
            let mut run = record["run"].clone();
            run["run_revision"] = json!(1);
            crate::runtime::send_future(self.effects.admit_message_in(&mut tx,id,sid,&run,&record["message"],&format!("manager:{id}"))).await?;
        }
        record["status"] = json!(state);
        record["verdict"] = json!(verdict);
        record["error"] = if state == "review_error" {
            json!("Review could not be completed. This message was not sent to the agent; you may submit a new message.")
        } else {
            Value::Null
        };
        record["usage"] = observed["usage"].clone();
        record["updated_at"] = json!(now());
        // Retain submission and criteria as private audit evidence; public views expose only status.
        crate::runtime::send_future(self.effects.write_document_in(&mut tx, &key, &record)).await?;
        sqlx::query("DELETE FROM docs WHERE key=?")
            .bind(&pointer)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,'manager_verdict',?)")
            .bind(sid)
            .bind(self.effects.encode_user_record(&self.public(&record))?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.effects.invalidate_documents(&[
            key,
            pointer,
            format!("run:{id}"),
            format!("run_view:{id}"),
            format!("session:{sid}"),
        ]);
        if state!="acceptable" {self.effects.reconcile_requester_slots().await?;}
        Ok(true)
    }
    }
}
