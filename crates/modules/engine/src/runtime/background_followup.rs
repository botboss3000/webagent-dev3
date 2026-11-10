//! A background read that finished after its run ended reports as a background notice.
use super::{config, models::ModelRecords, ports::*, Runtime};
use crate::context::abilities::Catalog;
use crate::ports::*;
use crate::status;
use anyhow::{anyhow, Result};
use serde_json::Value;
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
    /// Report a read that finished after its run ended: `wake` sends a background notice
    /// that wakes the agent with its normal permissions; `silent` sends a quiet notice
    /// that joins the conversation with the user's next message. `stop` work was
    /// cancelled when the run ended.
    pub async fn background_followup(&self, task: &Value) -> Result<()> {
        if task["status"] != "completed"
            || task["delivery_run_id"].is_string()
            || !matches!(task["on_run_end"].as_str(), Some("silent" | "wake"))
        {
            return Ok(());
        }
        let rid = task["run_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing task origin"))?;
        if status(&self.run(rid).await?) != "completed" {
            return Ok(());
        }
        let mut task = task.clone();
        if !self.background_permitted(&task).await.unwrap_or(false) {
            task["result"] = serde_json::json!({"status":"denied",
                "output":"Background result withheld: current authority unavailable"});
        }
        let quiet = task["on_run_end"] == "silent";
        self.background_notify_result(task, quiet).await
    }
}
