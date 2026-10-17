//! Run recovery and scheduling stay in Rust; the worker only executes authorized tools.
use super::*;
mod run;
pub(crate) use run::{execute_run, settle_abandoned_interrupt, settle_followup_interrupt};
/// Stage explicit selection effects before clearing pending call identity; the host Store commits them with the canonical tool result.
pub(super) fn append_tool(run: &mut Value, result: &Value) -> Result<()> {
    workflow::record_tool_result(run, result)?;
    let previous_tools=run["selected_tools"].clone();
    let previous_skills=run["loaded_skills"].clone();
    tools::core_tool_search::apply_selection(run,result);
    tools::core_skills::apply_selection(run,result);
    crate::context_assets::record_tool_operation(run,result,&previous_tools,&previous_skills)?;
    crate::tool_output::record(run,result)?;
    let call_id = run["pending"]["call_id"].clone();
    run["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"function_call_output","call_id":call_id,"output":result.to_string()}));
    run["call_index"] = json!(run["call_index"].as_u64().unwrap() + 1);
    run["pending"] = Value::Null;
    Ok(())
}
mod recovery;
pub(crate) use recovery::{attach_external_build_result, reconcile_late_tool_result, user_apps, recover};
pub(crate) mod watchdog;
mod scheduler;
#[cfg(test)]
pub(crate) use scheduler::scheduler;
pub(crate) use scheduler::{scheduler_with_progress,requeue_auto_approval};
// Follow one claimed run through the fixed workflow. At the relevant
// checkpoints, reread access and policy so earlier permission cannot linger.
