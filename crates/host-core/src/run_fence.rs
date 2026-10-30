//! Run fence: a tool call may write only while its run is still the session's live, leased dispatch.
use crate::StoreCore;
use anyhow::{anyhow, Result};
use sqlx::{Row, Sqlite, Transaction};

impl StoreCore {
    /// Checks inside `tx` that the session is active and the run is still running, unreplaced,
    /// on its original generation and pending dispatch, and holding its database lease.
    ///
    /// The routed user database owns both records. Stop/replacement and lease checks keep an
    /// old tool call from writing after recovery.
    pub async fn base_run_fence(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        sid: &str,
        rid: &str,
        pending_id: Option<&str>,
        generation: Option<&str>,
    ) -> Result<()> {
        let session_raw = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(format!("session:{sid}"))
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| anyhow!("Session unavailable"))?;
        let session = self.unseal(&session_raw)?;
        if session["id"] != sid || session["status"] == "archived" {
            return Err(anyhow!("Session unavailable"));
        }
        let run_raw = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(format!("run:{rid}"))
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| anyhow!("Run unavailable"))?;
        let run = self.unseal(&run_raw)?;
        if run["id"] != rid
            || run["session_id"] != sid
            || run["status"] != "running"
            || run["agent_id"].as_str().unwrap_or("shared_default")
                != session["agent_id"].as_str().unwrap_or("shared_default")
        {
            return Err(anyhow!(
                "Task run is stopped, replaced or belongs to another session"
            ));
        }
        if generation.is_some_and(|expected| run["generation"].as_str() != Some(expected)) {
            return Err(anyhow!("Task run generation changed"));
        }
        if let Some(pending_id) = pending_id {
            let single =
                run["pending"]["id"] == pending_id && run["pending"]["state"] == "executing";
            let parallel = run["batch"].as_array().is_some_and(|batch| {
                batch
                    .iter()
                    .any(|item| item["pending"]["id"] == pending_id && item["state"] == "executing")
            });
            if !single && !parallel {
                return Err(anyhow!("Task call is no longer the pending tool dispatch"));
            }
        }
        if run["lease_epoch"].as_i64().unwrap_or(0) > 0 {
            let lease = sqlx::query(
                "SELECT lease_epoch,lease_owner,lease_expires_at FROM run_index WHERE id=?",
            )
            .bind(rid)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| anyhow!("Run lease unavailable"))?;
            let now = crate::now() as i64;
            if lease.get::<i64, _>("lease_epoch") != run["lease_epoch"].as_i64().unwrap_or(-1)
                || lease.get::<Option<String>, _>("lease_owner").as_deref()
                    != run["lease_owner"].as_str()
                || lease.get::<i64, _>("lease_expires_at") < now
            {
                return Err(anyhow!("Run lease changed or expired"));
            }
        }
        Ok(())
    }

    /// Native Core dispatch retains the caller's original generation and checks its live
    /// database lease.
    pub async fn core_native_fence(
        &self,
        sid: &str,
        rid: &str,
        pending: &str,
        generation: &str,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        self.base_run_fence(&mut tx, sid, rid, Some(pending), Some(generation))
            .await?;
        tx.commit().await?;
        Ok(())
    }
}
