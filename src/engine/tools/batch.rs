//! Engine-owned parallel execution compatibility helpers.
use crate::*;
pub(super) fn executing_write(run:&Value)->bool {webagent_engine::runtime::batch::executing_write::<tool_execution::NativeActions>(run)}
pub(super) fn uncertain_count(run:&Value)->usize {webagent_engine::runtime::batch::uncertain_count(run)}
pub(super) fn mark_uncertain_writes(run:&mut Value) {webagent_engine::runtime::batch::mark_uncertain_writes::<tool_execution::NativeActions>(run)}
#[cfg(test)]
pub(crate) async fn execute_parallel_batch(
 app:&App,id:&str,generation:&str,cancel:&CancellationToken,
 workflow_policy:&workflow::Policy,access:&access::authority::LiveAccess,
 run:Value,guard:tokio::sync::MutexGuard<'_,()>,
)->Result<()> {
 crate::engine_runtime::Engine::execute_parallel_batch(&app.engine(),id,generation,cancel,workflow_policy,access,run,guard).await
}
