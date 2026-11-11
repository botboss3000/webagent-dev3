//! A run waiting for tool approval parks when a message or background notice arrives:
//! its card stays open, the conversation carries on in a new run, and the decided call
//! reports back as a background notice instead of another model step.
use super::{background::finished, config, models::ModelRecords, native, ports::*, Runtime};
use crate::context::abilities::Catalog;
use crate::ports::*;
use crate::{now, status};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};

const WAITING: &str = "waiting_approval";

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
            + super::followup::FollowupQueue<S>
            + native::NativeDispatch<S, C, B, H::Binding>,
    > Runtime<S, C, B, A, H, M, X>
{
    /// Park a run waiting for approval. Each undecided call becomes a background task
    /// holding its place (and the repeat guard), and every call of the turn gets a
    /// recorded result so the conversation's next run sees a complete history.
    /// The pending approval itself is untouched: deciding it runs the existing path.
    pub async fn park_approval(&self, run: Value) -> Result<()> {
        let app = self;
        let _gate = app.handles.gate.lock().await;
        let id = run["id"].as_str().ok_or_else(|| anyhow!("Missing run"))?;
        let mut run = app.run(id).await?;
        if status(&run) != WAITING || run["parked"] == true {
            return Ok(());
        }
        run["parked"] = json!(true);
        // (call_id, result) in call order; a waiting call's result names its task.
        let mut results: Vec<(Value, Value)> = Vec::new();
        let mut tasks = Vec::new();
        if let Some(batch) = run["batch"].as_array().cloned() {
            for item in &batch {
                let call_id = item["pending"]["call_id"].clone();
                if item["state"] == "done" {
                    results.push((call_id, item["result"].clone()));
                    continue;
                }
                let Some(task) = app.park_call(&run, &item["pending"]).await? else {
                    return Ok(());
                };
                tasks.push(json!({"task_id":task,"call_id":call_id}));
                results.push((call_id, standin(&task)));
            }
        } else {
            let pending = run["pending"].clone();
            let Some(task) = app.park_call(&run, &pending).await? else {
                return Ok(());
            };
            tasks.push(json!({"task_id":task,"call_id":pending["call_id"]}));
            results.push((pending["call_id"].clone(), standin(&task)));
            let next = run["call_index"].as_u64().unwrap_or(0) as usize + 1;
            for call in run["calls"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .skip(next)
            {
                results.push((
                    call["call_id"].clone(),
                    json!({"status":"not_run","output":
                    "Requested but not run: the user sent a message while an earlier call in \
                     this step waited for approval. Issue it again if it still applies."}),
                ));
            }
        }
        run["parked_tasks"] = json!(tasks);
        app.save(&mut run).await?;
        // The store records each save's last output in the conversation history. The run's
        // own input is never sent to a model again, so these entries only feed that history.
        for (call_id, result) in results {
            run["input"]
                .as_array_mut()
                .ok_or_else(|| anyhow!("Malformed run input"))?
                .push(json!({"type":"function_call_output","call_id":call_id,"output":result.to_string()}));
            app.store
                .save_run_observed(
                    &mut run,
                    "tool_result",
                    &json!({"run_id":id,"call_id":call_id,"result":result}),
                )
                .await?;
        }
        crate::wake::notify(&app.control_dir).await;
        Ok(())
    }
    /// One undecided call as a running background task; None at task capacity.
    async fn park_call(&self, run: &Value, pending: &Value) -> Result<Option<String>> {
        let mut probe = run.clone();
        probe["pending"] = pending.clone();
        self.background_detach_record(&probe, &pending["arguments"], Value::Null)
            .await
    }
    /// Whether the call this parked run now waits on was cancelled as a background task.
    pub async fn parked_call_cancelled(&self, run: &Value) -> Result<bool> {
        let call_id = &run["pending"]["call_id"];
        for task in run["parked_tasks"].as_array().into_iter().flatten() {
            if &task["call_id"] == call_id {
                let id = task["task_id"].as_str().unwrap_or("");
                return Ok(self
                    .store
                    .background_get(id)
                    .await?
                    .is_some_and(|task| task["status"] == "cancelled"));
            }
        }
        Ok(false)
    }
    /// End a parked run once its decided calls have results: report each as a
    /// background notice. The run makes no further model step.
    pub async fn finish_parked(&self, mut run: Value) -> Result<()> {
        for parked in run["parked_tasks"].as_array().cloned().unwrap_or_default() {
            let call_id = &parked["call_id"];
            let result = run["input"]
                .as_array()
                .into_iter()
                .flatten()
                .rev()
                .find(|item| item["type"] == "function_call_output" && &item["call_id"] == call_id)
                .and_then(|item| serde_json::from_str::<Value>(item["output"].as_str()?).ok())
                .filter(|result| result["status"] != WAITING)
                .unwrap_or_else(|| json!({"status":"not_run","output":"The call did not run."}));
            let id = parked["task_id"].as_str().unwrap_or("");
            let Some(mut task) = self.store.background_get(id).await? else {
                continue;
            };
            if task["delivery_run_id"].is_string() {
                continue;
            }
            if !finished(&task) {
                task["status"] = json!("completed");
            }
            task["result"] = result;
            task["completed_at"] = json!(now());
            self.background_notify_result(task).await?;
        }
        run["status"] = json!("completed");
        self.save(&mut run).await
    }
}

/// The recorded result of a call parked on its approval card.
fn standin(task: &str) -> Value {
    json!({"status":WAITING,"task_id":task,"output":format!(
        "Waiting for the user's approval as background task {task}: the user sent a message, \
         so answer it now. The approval card stays open; when the user decides, this call \
         runs (or is declined) and its result is delivered to you automatically. Do not \
         repeat this call; if the user asks to drop it, cancel it with background_cancel.")})
}
