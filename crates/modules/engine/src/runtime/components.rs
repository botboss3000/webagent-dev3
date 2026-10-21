//! Component withdrawal/restore decisions stay with Engine run state.
use super::{Runtime,config,ports::*,models::ModelRecords};
use crate::ports::*;
use crate::components::*;
use crate::context::abilities::Catalog;
use serde_json::{json,Value};
use anyhow::Result;
impl<S:ScopedStorage+ModelRecords+ChatEffects+AdmissionEffects+NotificationEffects+UsageEffects+RunObservation+RunPresentation,C:config::SchemaCatalog,B:Catalog,A:super::actions::Actions<S>+Clone+PartialEq,H:ChatAuthority<S>,M:ModuleBindings<S>,X:AgentPolicy<S>+NativePolicy+CredentialBindings<S>+super::native::NativeDispatch<S,C,B,H::Binding>> Runtime<S,C,B,A,H,M,X> {
pub async fn component_pause(&self,run:&mut Value)->Result<()> {let app=self;
 let component=run["component_pause"]["component"].as_str().filter(|id|working_component(id))
  .map(str::to_owned).unwrap_or_else(||WORKING_COMPONENTS.into_iter()
   .find(|id|!component_available(crate::ports::AgentMetadata::root(app.store.records()),id)).unwrap_or(CONTEXT).to_owned());
 run["status"]=json!("paused");run["component_pause"]=json!({"component":component,"version":1});
 run["error"]=json!("Required engine component unavailable; work resumes when the component is restored");
 app.save(run).await
}
pub async fn component_restore(&self,id:&str)->Result<()> {let app=self;
 if !available(crate::ports::AgentMetadata::root(app.store.records()))||app.shutoff().await?||app.active.lock().await.contains_key(id){return Ok(());}
 let mut run=app.run(id).await?;
 if !retained_pause(&run){return Ok(());}
 if !app.chat.participation_allows_run(app.store.records(),&run).await?||!app.run_model_authorized(&run).await {
  run["status"]=json!("stopped");run["component_pause"]=Value::Null;run["error"]=json!("Access changed while engine component was unavailable");
 }else{
  run["status"]=json!("queued");run["component_pause"]=Value::Null;run["error"]=Value::Null;
 }
 app.save(&mut run).await?;crate::wake::notify(&app.control_dir).await;Ok(())
}
}
