//! Replaceable recovery selection; the host owns desired state, crash guards and Docker.
use anyhow::Result;
use webagent_boot_contracts::*;
pub const IMPLEMENTATION_VERSION: &str = "0.1.1";
/// Select only host-qualified faults, with a timer only while an actual recovery is pending.
pub fn plan(input: &Request) -> Result<Plan> {
 validate_request(input)?;
 let mut result=Plan{version:INTERFACE_VERSION,actions:vec![],next_deadline_ms:None};
 for (index,s) in input.services.iter().enumerate() {
  if !s.eligible || !s.dependency_ready {continue;}
  let operation=match s.state {
   State::Stopped => Operation::Start,
   State::Running if s.unhealthy => Operation::Restart,
   _ => continue,
  };
  if s.safe_after_ms > input.now_ms {
   result.next_deadline_ms=Some(result.next_deadline_ms.map_or(s.safe_after_ms,|t|t.min(s.safe_after_ms)));
  } else {result.actions.push(Action{index,operation});}
 }
 validate_plan(input,&result)?;
 Ok(result)
}
#[cfg(test)]
mod tests {
 use super::*;
 fn service(state:State)->Service {Service{state,unhealthy:false,eligible:true,dependency_ready:true,safe_after_ms:0}}
 /// Healthy, missing, unknown and Docker-managed restart states require no periodic wake.
 #[test] fn quiescent_has_no_timer() {
  for state in [State::Running,State::Missing,State::Unknown,State::Restarting] {
   let p=plan(&Request{version:1,now_ms:10,services:vec![service(state)]}).unwrap();
   assert!(p.actions.is_empty());assert_eq!(p.next_deadline_ms,None);
  }
 }
 /// Start/recovery grace, dependency readiness and manual/crash fencing preserve host decisions.
 #[test] fn recovery_is_bounded_and_selective() {
  let mut a=service(State::Stopped);a.safe_after_ms=30;
  let mut b=service(State::Running);b.unhealthy=true;b.eligible=false;
  let mut c=service(State::Stopped);c.dependency_ready=false;
  let mut r=Request{version:1,now_ms:10,services:vec![a,b,c]};
  assert_eq!(plan(&r).unwrap(),Plan{version:1,actions:vec![],next_deadline_ms:Some(30)});
  r.now_ms=30;
  assert_eq!(plan(&r).unwrap().actions,vec![Action{index:0,operation:Operation::Start}]);
 }
}
