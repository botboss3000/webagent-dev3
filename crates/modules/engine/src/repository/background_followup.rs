//! Engine delayed-read delivery and follow-up run admission in the existing transaction.
use super::*;
use webagent_access::storage::RecordCodec;
impl<'a,R:Records,H:ChatEffects,Q:AdmissionEffects,N:NotificationEffects,U:UsageEffects> Repository<'a,R,H,Q,N,U> {
    pub fn background_postrun<'s>(&'s self,observed:&'s Value,wake:Option<&'s Value>,max_runs:usize,permitted:bool)->impl std::future::Future<Output=Result<()>>+Send+'s {async move {
        let id=observed["id"].as_str().ok_or_else(||anyhow!("Missing task identity"))?;
        let mut tx=self.pool.begin().await?;
        sqlx::query("UPDATE background_task_index SET revision=revision WHERE id=?").bind(id).execute(&mut *tx).await?;
        let raw:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(format!("background_task:{id}")).fetch_optional(&mut *tx).await?;
        let Some(raw)=raw else {return Ok(())};
        let mut task=self.records.unseal(&raw)?;
        if task["user_id"]!=self.records.current_user_id() || task["status"]!="completed" || (task["notified"]==true && task["on_run_end"]!="wake") || task["delivery_run_id"].is_string() {return Ok(());}
        let rid=task["run_id"].as_str().ok_or_else(||anyhow!("Missing task origin"))?.to_owned();
        let sid=task["session_id"].as_str().ok_or_else(||anyhow!("Missing task session"))?.to_owned();
        let origin_raw:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(format!("run:{rid}")).fetch_one(&mut *tx).await?;
        let origin=self.records.unseal(&origin_raw)?;
        if crate::send_future(self.chat.require_participation(&mut tx,&origin)).await.is_err() {
            task["notified"]=json!(true);task["on_run_end"]=json!("stop");
            let revision=task["revision"].as_u64().unwrap_or(0)+1;task["revision"]=json!(revision);
            sqlx::query("UPDATE background_task_index SET revision=? WHERE id=?").bind(revision as i64).bind(id).execute(&mut *tx).await?;
            sqlx::query("UPDATE docs SET value=? WHERE key=?").bind(self.records.seal(&task)?).bind(format!("background_task:{id}")).execute(&mut *tx).await?;
            tx.commit().await?;return Ok(());
        }
        if origin["status"]!="completed" {return Ok(());}
        if origin["background_delivered"].as_array().is_some_and(|items|items.contains(&task["id"])) {
            task["delivery_run_id"]=json!(rid);
        } else {
            crate::send_future(self.chat.require_active_session(&mut tx,&sid)).await?;
            let label=task["name"].as_str().unwrap_or("read tool");
            let text=if permitted {format!("Background result received: {label}. The originating run has already finished; this delayed result is available for the next turn.")}
                else {format!("Background task finished: {label}. Its result is withheld because current access is unavailable.")};
            if task["notified"]!=true {
                crate::send_future(self.chat.insert_working_message(&mut tx,&sid,&rid,&format!("working:{rid}:background:{id}"),&text)).await?;
            }
            task["notified"]=json!(true);
            if let Some(wake)=wake {
                let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM run_index WHERE status NOT IN ('completed','cancelled','failed')").fetch_one(&mut *tx).await?;
                let session_count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM run_index WHERE session_id=? AND status NOT IN ('completed','cancelled','failed')").bind(&sid).fetch_one(&mut *tx).await?;
                if count<(max_runs as i64) && session_count==0 {
                    let capacity=match crate::send_future(self.admission.reserve_requester_run(wake)).await {
                        Ok(())=>true,
                        Err(error) if error.to_string()=="Account run limit reached"=>false,
                        Err(error)=>return Err(error),
                    };
                    if capacity {

                    let wake_id=wake["id"].as_str().ok_or_else(||anyhow!("Missing wake identity"))?;
                    crate::send_future(self.records.write_document(&mut tx,&format!("run:{wake_id}"),wake)).await?;
                    self.index_run(&mut tx,wake).await?;
                    task["wake_run_id"]=json!(wake_id);
                    task["delivery_run_id"]=json!(wake_id);
                    }
                }
            }
        }
        if let Some(delivery_run)=task["delivery_run_id"].as_str() {
            sqlx::query("INSERT OR IGNORE INTO background_task_delivery(id,run_id) VALUES(?,?)").bind(id).bind(delivery_run).execute(&mut *tx).await?;
        }
        let revision=task["revision"].as_u64().unwrap_or(0)+1;
        task["revision"]=json!(revision);
        sqlx::query("UPDATE background_task_index SET revision=? WHERE id=?").bind(revision as i64).bind(id).execute(&mut *tx).await?;
        sqlx::query("UPDATE docs SET value=? WHERE key=?").bind(self.records.seal(&task)?).bind(format!("background_task:{id}")).execute(&mut *tx).await?;
        tx.commit().await?;
        self.records.invalidate(&[format!("background_task:{id}")]);
        Ok(())
    }
    }
}
