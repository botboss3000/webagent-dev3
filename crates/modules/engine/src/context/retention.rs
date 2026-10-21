//! Engine fresh settings, component pause and save composition; Context owns retention/IO algorithms.
pub use webagent_context::retention::*;
use anyhow::Result;
use serde_json::{json,Value};
use crate::runtime::{Runtime,config,ports::*,models::ModelRecords};
use crate::ports::*;
use crate::context::abilities::Catalog;
impl<S:ScopedStorage+ModelRecords+ChatEffects+AdmissionEffects+NotificationEffects+UsageEffects+RunObservation+RunPresentation+ToolJournal,C:config::SchemaCatalog,B:Catalog,A:crate::runtime::actions::Actions<S>+Clone+PartialEq,H:ChatAuthority<S>,M:ModuleBindings<S>,X:AgentPolicy<S>+NativePolicy+CredentialBindings<S>+crate::runtime::native::NativeDispatch<S,C,B,H::Binding>> Runtime<S,C,B,A,H,M,X> {
pub async fn dedup_prepare(&self,run:&mut Value,preview_bytes:usize,cancel:&tokio_util::sync::CancellationToken)->Result<Vec<Value>> {
 let _=run["session_id"].as_str().ok_or_else(||anyhow::anyhow!("Projection session missing"))?;
 let agent=run["agent_id"].as_str().unwrap_or("shared_default");
 let policy=dedup_policy(self.store.records(),agent).await?;
 let template=if policy.enabled {Some(dedup_template(crate::ports::AgentMetadata::root(self.store.records()))?)} else {None};
 let journal=crate::context::bridge::Journal {store:self.store.records()};
 let (projected,changed)=webagent_context::retention::dedup_prepare(run,preview_bytes,crate::ports::AgentMetadata::root(self.store.records()),policy,template,self,&journal,&crate::context::bridge::Planner,cancel).await?;
 if changed {self.save(run).await?;} Ok(projected)
}
pub async fn pause_dedup(&self,run:&mut Value,error:&anyhow::Error)->Result<()> {let app=self;
    if !crate::components::available(crate::ports::AgentMetadata::root(app.store.records())) {
        return app.component_pause(run).await;
    }
    run["status"]=json!("paused");run["component_pause"]=Value::Null;
    run["error"]=json!(format!("Tool result context paused: {error}"));
    app.save(run).await
}
pub async fn dedup_summary_source(&self,run:&Value,source:&[Value])->Result<Vec<Value>> {
 webagent_context::retention::dedup_summary_source(&crate::context::bridge::Journal{store:self.store.records()},run,source).await
}
pub async fn dedup_config_fingerprint(&self,agent:&str,sid:&str)->Result<String> {let app=self;
    let policy=dedup_policy(app.store.records(),agent).await?;
    let template=if policy.enabled {Some(dedup_template(crate::ports::AgentMetadata::root(app.store.records()))?)} else {None};
    let (_,_,settings,_)=app.ability_controls(sid).await?;
    Ok(crate::digest(&json!([policy,template,settings["tool_result_deduplication"]])))
}
}
impl<S:ScopedStorage+ModelRecords+ChatEffects+AdmissionEffects+NotificationEffects+UsageEffects+RunObservation+RunPresentation+ToolJournal,C:config::SchemaCatalog,B:Catalog,A:crate::runtime::actions::Actions<S>+Clone+PartialEq,H:ChatAuthority<S>,M:ModuleBindings<S>,X:AgentPolicy<S>+NativePolicy+CredentialBindings<S>+crate::runtime::native::NativeDispatch<S,C,B,H::Binding>> webagent_context::ports::DedupSettings for Runtime<S,C,B,A,H,M,X> {
 fn dedup_settings<'a>(&'a self,sid:&'a str)->impl std::future::Future<Output=Result<Value>>+Send+'a {async move {let (_,_,settings,_)=self.ability_controls(sid).await?;Ok(settings)}}
}
