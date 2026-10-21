//! Engine interruption detection, steering boundary and stable successor assembly.
//! Chat retains queue persistence, consumption transactions and delegation revalidation.
use super::{Runtime,config,models,native,ports::*};
use crate::ports::{ChatEffects,AdmissionEffects,NotificationEffects,UsageEffects};
use crate::{digest,now,status};
use anyhow::{anyhow,Result};
use serde_json::{json,Value};
use std::future::Future;

/// Existing Chat queue operations always receive the current trusted storage scope.
pub trait FollowupQueue<S:Sync>:Clone+Send+Sync+'static {
 fn followup_queue<'a>(&'a self,store:&'a S,sid:&'a str)->impl Future<Output=Result<Value>>+Send+'a;
 fn followup_consume<'a>(&'a self,store:&'a S,sid:&'a str,choice:&'a str,run:&'a mut Value,origin:Option<&'a mut Value>)->impl Future<Output=Result<bool>>+Send+'a;
 fn followup_hold<'a>(&'a self,store:&'a S,sid:&'a str,reason:&'a str)->impl Future<Output=Result<()>>+Send+'a;
}

pub fn successor_id(owner: &str, sid: &str, entry: &Value) -> Result<String> {
    let id=entry["id"].as_str().ok_or_else(||anyhow!("Followup entry id missing"))?;
    if let Ok(id)=::uuid::Uuid::parse_str(id) {return Ok(id.to_string());}
    let hash=digest(&json!(["webagent-private-followup-successor-v1",owner,sid,id]));
    // A stable UUID preserves retries and legacy route expectations.
    Ok(::uuid::Uuid::parse_str(&hash[..32])?.to_string())
}

impl<S:ScopedStorage+ChatEffects+AdmissionEffects+NotificationEffects+UsageEffects+RunObservation+RunPresentation+models::ModelRecords,C:config::SchemaCatalog,B:Sync,A,H:ChatAuthority<S>,M:ModuleBindings<S>,X:AgentPolicy<S>+FollowupQueue<S>+native::NativeDispatch<S,C,B,H::Binding>> Runtime<S,C,B,A,H,M,X> {
pub async fn interrupt_requested(&self, rid: &str) -> Result<bool> {
    if let Some(r)=self.store.get(&format!("run:{rid}")).await? {if let Some(s)=r["session_id"].as_str(){self.native.messaging_revalidate(self.native_view(),s).await?;}}
    if let Some(run)=self.store.get(&format!("run:{rid}")).await? {if run["requester_user_id"].as_str().is_some_and(|actor|actor!=self.store.current_user_id()) || !self.chat.participation_allows_run(self.store.records(),&run).await? {return Ok(false);}}
    let Some(r) = self.store.get(&format!("run_view:{rid}")).await? else {
        return Ok(false);
    };
    let q = self.native
        .followup_queue(self.store.records(),r["session_id"].as_str().unwrap_or(""))
        .await?;
    Ok(q["entries"].as_array().unwrap().iter().any(|e| {
        e["choice"] == "interrupt"
            && e["status"] == "pending"
            && e["review_status"] == "acceptable"
            && (e["target_run_id"].is_null() || e["target_run_id"] == rid)
    }))
}
pub async fn apply_steering(&self, run: &mut Value) -> Result<bool> {
    if run["requester_user_id"].as_str().is_some_and(|actor|actor!=self.store.current_user_id()){return Ok(false);}
    if self.interrupt_requested(run["id"].as_str().unwrap()).await? {
        return Ok(false);
    }
    let s = run["session_id"].as_str().unwrap().to_owned();
    self.native.followup_consume(self.store.records(),&s, "steer", run, None).await
}
pub async fn finish_interrupt(&self, run: &mut Value) -> Result<bool> {
    if run["requester_user_id"].as_str().is_some_and(|actor|actor!=self.store.current_user_id()){return Ok(false);}
    if run["uncertain"] == true || matches!(status(run), "recovery" | "waiting_external") {
        return Ok(false);
    }
    let s = run["session_id"].as_str().unwrap().to_owned();
    let q = self.native.followup_queue(self.store.records(),&s).await?;
    let Some(e) = q["entries"].as_array().unwrap().iter().find(|e| {
        e["choice"] == "interrupt"
            && e["status"] == "pending"
            && e["review_status"] == "acceptable"
            && (e["target_run_id"].is_null() || e["target_run_id"] == run["id"])
    }) else {
        return Ok(false);
    };
    if self.shutoff().await? {
        return Ok(false);
    }
    let mut next = e["template"].clone();
    if self.run_candidate(&next).await.is_err() {
        self.native
            .followup_hold(
                self.store.records(),&s,
                "Interrupted work is held because the model is unavailable",
            )
            .await?;
        return Ok(false);
    }
    next["id"] = json!(successor_id(self.store.current_user_id(),&s,&e)?);
    next["created_at"] = json!(now());
    next["updated_at"] = json!(now());
    next["input"] = run["input"].clone();
    next["context_hydration_pending"] = json!(false);
    next["interrupted_run_id"] = run["id"].clone();
    next["input"].as_array_mut().ok_or_else(||anyhow!("Missing interrupted context"))?.push(json!({"role":"developer","content":"Previous run interrupted by user. Retained tool results describe completed work; interruption does not undo effects. Unknown writes require reconciliation before retry."}));
    self.native
        .followup_consume(self.store.records(),&s, "interrupt", &mut next, Some(run))
        .await
}
}
