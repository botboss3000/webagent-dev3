//! Versioned metadata-only boot planning; identities, grants and effects stay in the host.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
pub const INTERFACE_VERSION: u32 = 1;
pub const MAX_SERVICES: usize = 32;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Unknown observations never authorize a start; Docker restarting is already recovering.
pub enum State { Missing, Unknown, Stopped, Running, Restarting }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Host-qualified recovery need and absolute monotonic deadline, without names or credentials.
pub struct Service {
 pub state: State, pub unhealthy: bool, pub eligible: bool, pub dependency_ready: bool,
 pub safe_after_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// A bounded snapshot; the host must fence it against fresh state before execution.
pub struct Request { pub version: u32, pub now_ms: u64, pub services: Vec<Service> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Operation { Start, Restart }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Action { pub index: usize, pub operation: Operation }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
/// None means no timer: wait for an external event, rather than continuously polling.
pub struct Plan { pub version: u32, pub actions: Vec<Action>, pub next_deadline_ms: Option<u64> }
/// Reject an unsupported interface or oversized host projection before calculation.
pub fn validate_request(input: &Request) -> Result<()> {
 if input.version != INTERFACE_VERSION || input.services.len() > MAX_SERVICES { bail!("Boot request bounds"); }
 Ok(())
}
/// Validate proposed effects independently; positions never grant Docker authority.
pub fn validate_plan(input: &Request, plan: &Plan) -> Result<()> {
 validate_request(input)?;
 if plan.version != INTERFACE_VERSION || plan.actions.len() > input.services.len()
  || plan.next_deadline_ms.is_some_and(|t| t <= input.now_ms) { bail!("Boot plan bounds"); }
 let mut seen=std::collections::BTreeSet::new();
 for action in &plan.actions {
  let s=input.services.get(action.index).ok_or_else(||anyhow::anyhow!("Boot index"))?;
  if !seen.insert(action.index) || !s.eligible || !s.dependency_ready || s.safe_after_ms > input.now_ms
   || match action.operation { Operation::Start => s.state != State::Stopped,
       Operation::Restart => s.state != State::Running || !s.unhealthy } { bail!("Boot action rejected"); }
 }
 if let Some(t)=plan.next_deadline_ms {
  if !input.services.iter().any(|s|s.eligible && s.dependency_ready && s.safe_after_ms == t
      && (s.state == State::Stopped || s.state == State::Running && s.unhealthy)) { bail!("Boot deadline rejected"); }
 }
 Ok(())
}
#[cfg(test)]
mod tests {
 use super::*;
 /// Forged healthy restarts, unknown starts, duplicates and elapsed deadlines fail closed.
 #[test] fn unsafe_plans_are_rejected() {
  let input=Request{version:1,now_ms:10,services:vec![Service{state:State::Running,unhealthy:false,eligible:true,dependency_ready:true,safe_after_ms:0}]};
  let action=Action{index:0,operation:Operation::Restart};
  assert!(validate_plan(&input,&Plan{version:1,actions:vec![action],next_deadline_ms:None}).is_err());
  assert!(validate_plan(&input,&Plan{version:1,actions:vec![],next_deadline_ms:Some(10)}).is_err());
  let mut stopped=input;stopped.services[0].state=State::Stopped;
  let a=Action{index:0,operation:Operation::Start};
  assert!(validate_plan(&stopped,&Plan{version:1,actions:vec![a.clone(),a],next_deadline_ms:None}).is_err());
 }
}
