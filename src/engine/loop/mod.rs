//! Run recovery and scheduling stay in Rust; the worker only executes authorized tools.
use super::*;
mod run;
pub(crate) use run::{execute_run, settle_abandoned_interrupt, settle_followup_interrupt};
/// Stage explicit selection effects before clearing pending call identity; the host Store commits them with the canonical tool result.
pub(super) fn append_tool(run:&mut Value,result:&Value)->Result<()> {crate::engine_runtime::Engine::append_tool(run,result)}

mod recovery;
pub(crate) use recovery::{attach_external_build_result, reconcile_late_tool_result, user_apps, recover};
pub(crate) mod watchdog;
mod scheduler;
#[cfg(test)]
pub(crate) use scheduler::scheduler;
pub(crate) use scheduler::{scheduler_with_progress,requeue_auto_approval};
// Follow one claimed run through the fixed workflow. At the relevant
// checkpoints, reread access and policy so earlier permission cannot linger.
