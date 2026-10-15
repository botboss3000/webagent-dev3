//! Replaceable scheduling policy over bounded lifecycle metadata only.
use anyhow::Result;
use webagent_background_contracts::{Request,Plan,State,validate_request,INTERFACE_VERSION};
pub const IMPLEMENTATION_VERSION: &str = "0.2.0";
/// Preserve scan order and the four-worker ceiling; reclaim only expired unowned reads.
pub fn plan(input: &Request) -> Result<Plan> {
 validate_request(input)?;
 let mut output=Plan{version:INTERFACE_VERSION,dispatch:Vec::new(),followup:Vec::new(),retry_ms:input.rescan_interval_ms};
 for (index,task) in input.tasks.iter().enumerate() {
  if task.state==State::Completed { output.followup.push(index); continue; }
  if task.active { continue; }
  match task.state {
   State::Queued => if output.dispatch.len()<input.slots {output.dispatch.push(index);},
   State::Running => if task.lease_until<=input.now {
    if output.dispatch.len()<input.slots {output.dispatch.push(index);}
   } else {
    output.retry_ms=output.retry_ms.min(task.lease_until.saturating_sub(input.now).saturating_mul(1000).max(1));
   },
   _ => {}
  }
 }
 Ok(output)
}
#[cfg(test)] mod tests {
 use super::*;use webagent_background_contracts::{Task,validate_plan};
 /// Characterize queue order, local ownership, foreign lease expiry and completion selection.
 #[test] fn preserves_ready_order_completion_and_expiry() {
  let input=Request{version:2,now:100,slots:2,rescan_interval_ms:5000,tasks:vec![
   Task{state:State::Queued,active:false,lease_until:0},Task{state:State::Queued,active:true,lease_until:0},
   Task{state:State::Running,active:false,lease_until:101},Task{state:State::Running,active:false,lease_until:100},
   Task{state:State::Completed,active:false,lease_until:0},Task{state:State::Cancelled,active:false,lease_until:0}]};
  let output=plan(&input).unwrap();assert_eq!(output.dispatch,vec![0,3]);assert_eq!(output.followup,vec![4]);assert_eq!(output.retry_ms,1000);validate_plan(&input,&output).unwrap();
 }
 /// Empty or saturated work retains the bounded recovery delay.
 #[test] fn empty_and_saturated_inputs_do_not_spin() {
  let input=Request{version:2,now:0,slots:0,rescan_interval_ms:5000,tasks:vec![]};let output=plan(&input).unwrap();assert!(output.dispatch.is_empty());assert_eq!(output.retry_ms,5000);
 }
}

#[cfg(test)] mod configurable_tests {
 use super::*;
 /// No tasks means the exact requested rescan period, including subsecond and very large values.
 #[test] fn idle_interval_has_no_five_second_ceiling() {
  for interval in [1,250,5000,15000,u64::MAX] {
   let input=Request{version:2,now:0,slots:0,rescan_interval_ms:interval,tasks:vec![]};
   let output=plan(&input).unwrap();assert_eq!(output.retry_ms,interval);
   webagent_background_contracts::validate_plan(&input,&output).unwrap();
  }
 }
}
