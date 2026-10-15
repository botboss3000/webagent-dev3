//! Version-2 metadata-only planning contract; the host retains all authority and content.
use serde::{Deserialize, Serialize};
use anyhow::{bail, Result};
pub const INTERFACE_VERSION: u32 = 2;
pub const MAX_TASKS: usize = 256;
pub const MAX_WORKERS: usize = 4;
/// Bootstrap default only; the host supplies every effective interval.
pub const DEFAULT_RESCAN_INTERVAL_MS: u64 = 5000;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all="snake_case")]
/// Host-reported lifecycle state; it is scheduling evidence, never a permission.
pub enum State { Queued, Running, Completed, Cancelled, Failed }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// One content-free scan position; active ownership and lease expiry remain host facts.
pub struct Task { pub state: State, pub active: bool, pub lease_until: u64 }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Versioned bounded scan projection, with capacity reserved by the host executor.
pub struct Request { pub version: u32, pub now: u64, pub slots: usize, pub rescan_interval_ms:u64, pub tasks: Vec<Task> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
/// Advisory positions and retry timing, validated again before host effects.
pub struct Plan { pub version: u32, pub dispatch: Vec<usize>, pub followup: Vec<usize>, pub retry_ms: u64 }
/// Bound the metadata projection before either linked or remote implementation sees it.
pub fn validate_request(input: &Request) -> Result<()> {
 if input.version!=INTERFACE_VERSION || input.rescan_interval_ms==0 || input.slots>MAX_WORKERS || input.tasks.len()>MAX_TASKS { bail!("Background request bounds"); }
 Ok(())
}
/// Validate safety independently of selection policy; positions never grant execution authority.
pub fn validate_plan(input: &Request, plan: &Plan) -> Result<()> {
 validate_request(input)?;
 if plan.version!=INTERFACE_VERSION || plan.dispatch.len()>input.slots || plan.followup.len()>input.tasks.len()
   || plan.retry_ms==0 || plan.retry_ms>input.rescan_interval_ms
   || input.tasks.iter().any(|task|!task.active && task.state==State::Running && task.lease_until>input.now
    && plan.retry_ms>task.lease_until.saturating_sub(input.now).saturating_mul(1000)) { bail!("Background plan bounds"); }
 let mut seen=std::collections::BTreeSet::new();
 for &index in &plan.dispatch {
  let task=input.tasks.get(index).ok_or_else(||anyhow::anyhow!("Background plan index"))?;
  if !seen.insert(index) || task.active || !matches!(task.state,State::Queued|State::Running)
    || task.state==State::Running && task.lease_until>input.now { bail!("Background dispatch rejected"); }
 }
 for &index in &plan.followup {
  let task=input.tasks.get(index).ok_or_else(||anyhow::anyhow!("Background plan index"))?;
  if !seen.insert(index) || task.state!=State::Completed { bail!("Background followup rejected"); }
 }
 Ok(())
}
#[cfg(test)] mod tests {
 use super::*;
 /// Reject forged positions, duplicate selection, unexpired leases and active ownership.
 #[test] fn forged_indices_leases_and_capacity_are_rejected() {
  let input=Request{version:2,now:10,slots:1,rescan_interval_ms:5000,tasks:vec![Task{state:State::Running,active:false,lease_until:11}]};
  for dispatch in [vec![0],vec![1],vec![0,0]] { assert!(validate_plan(&input,&Plan{version:2,dispatch,followup:vec![],retry_ms:1000}).is_err()); }
  let mut ready=input.clone();ready.tasks[0].lease_until=10;
  assert!(validate_plan(&ready,&Plan{version:2,dispatch:vec![0],followup:vec![],retry_ms:5000}).is_ok());
  ready.tasks[0].active=true;assert!(validate_plan(&ready,&Plan{version:2,dispatch:vec![0],followup:vec![],retry_ms:5000}).is_err());
 }
}

/// Versioned content-free host diagnostic projection; missing milestones remain absent.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSummary {
 pub version:u32, pub task_id:String, pub session_id:String, pub run_id:String, pub status:State,
 pub created_at_ms:Option<u64>, pub claimed_at_ms:Option<u64>, pub completed_at_ms:Option<u64>,
}

#[cfg(test)] mod interval_tests {
 use super::*;
 /// Host timing is positive, uncapped metadata; old wire versions cannot hide a fixed ceiling.
 #[test] fn configurable_bounds_and_legacy_rejection() {
  for interval in [1,250,5000,15000,u64::MAX] {
   let input=Request{version:2,now:10,slots:0,rescan_interval_ms:interval,tasks:vec![]};
   let mut plan=Plan{version:2,dispatch:vec![],followup:vec![],retry_ms:interval};
   validate_plan(&input,&plan).unwrap();
   plan.retry_ms=0;assert!(validate_plan(&input,&plan).is_err());
   let mut legacy=input.clone();legacy.version=1;assert!(validate_request(&legacy).is_err());
  }
  let input=Request{version:2,now:10,slots:0,rescan_interval_ms:0,tasks:vec![]};
  assert!(validate_request(&input).is_err());
 }
 /// A remote planner cannot postpone an unowned lease or exceed the host's configured interval.
 #[test] fn earlier_foreign_lease_is_independently_enforced() {
  let input=Request{version:2,now:10,slots:0,rescan_interval_ms:15000,
   tasks:vec![Task{state:State::Running,active:false,lease_until:11}]};
  let mut plan=Plan{version:2,dispatch:vec![],followup:vec![],retry_ms:1000};
  validate_plan(&input,&plan).unwrap();
  plan.retry_ms=1001;assert!(validate_plan(&input,&plan).is_err());
  let mut owned=input.clone();owned.tasks[0].active=true;plan.retry_ms=15000;
  validate_plan(&owned,&plan).unwrap();
  plan.retry_ms=15001;assert!(validate_plan(&owned,&plan).is_err());
 }
}
