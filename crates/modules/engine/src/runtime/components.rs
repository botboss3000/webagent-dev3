//! Component withdrawal/restore decisions stay with Engine run state.
use super::{config, models::ModelRecords, ports::*, Runtime};
use crate::components::*;
use crate::context::abilities::Catalog;
use crate::ports::*;
use anyhow::Result;
use serde_json::{json, Value};
impl<
        S: ScopedStorage
            + ModelRecords
            + ChatEffects
            + AdmissionEffects
            + NotificationEffects
            + UsageEffects
            + RunObservation
            + RunPresentation,
        C: config::SchemaCatalog,
        B: Catalog,
        A: super::actions::Actions<S> + Clone + PartialEq,
        H: ChatAuthority<S>,
        M: ModuleBindings<S>,
        X: AgentPolicy<S>
            + NativePolicy
            + CredentialBindings<S>
            + super::native::NativeDispatch<S, C, B, H::Binding>,
    > Runtime<S, C, B, A, H, M, X>
{
    pub async fn component_pause(&self, run: &mut Value) -> Result<()> {
        let app = self;
        let component = run["component_pause"]["component"]
            .as_str()
            .filter(|id| working_component(id))
            .map(str::to_owned)
            .unwrap_or_else(|| {
                WORKING_COMPONENTS
                    .into_iter()
                    .find(|id| {
                        !component_available(
                            crate::ports::AgentMetadata::root(app.store.records()),
                            id,
                        )
                    })
                    .map(str::to_owned)
                    // An expired cooldown no longer shows which component failed; the recorded failure does.
                    .or_else(|| {
                        last_failure()
                            .map(|(id, _)| id)
                            .filter(|id| working_component(id))
                    })
                    .unwrap_or_else(|| CONTEXT.to_owned())
            });
        let cause = last_failure()
            .filter(|(id, _)| *id == component)
            .map(|(_, cause)| cause);
        run["status"] = json!("paused");
        run["component_pause"] = json!({"component":component,"version":1});
        // Name the recorded cause so the pause is diagnosable from the run itself.
        run["error"]=json!(match cause{
  Some(cause)=>format!("Required engine component {component} unavailable ({cause}); work resumes when the component is restored"),
  None=>"Required engine component unavailable; work resumes when the component is restored".to_owned()});
        app.save(run).await
    }
    pub async fn component_restore(&self, id: &str) -> Result<()> {
        let app = self;
        if !available(crate::ports::AgentMetadata::root(app.store.records()))
            || app.shutoff().await?
            || app.active.lock().await.contains_key(id)
        {
            return Ok(());
        }
        let mut run = app.run(id).await?;
        if !retained_pause(&run) {
            return Ok(());
        }
        if !app
            .chat
            .participation_allows_run(app.store.records(), &run)
            .await?
            || !app.run_model_authorized(&run).await
        {
            run["status"] = json!("stopped");
            run["component_pause"] = Value::Null;
            run["error"] = json!("Access changed while engine component was unavailable");
        } else {
            run["status"] = json!("queued");
            run["component_pause"] = Value::Null;
            run["error"] = Value::Null;
        }
        app.save(&mut run).await?;
        crate::wake::notify(&app.control_dir).await;
        Ok(())
    }
}
