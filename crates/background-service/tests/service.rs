//! Real pipe protocol equivalence and fail-closed invocation tests.
use std::{io::{BufRead,BufReader,Write},process::{Command,Stdio}};
use serde_json::{json,Value};
use webagent_background_contracts::{Request,State,Task};
/// Transported plans match direct library behavior across lease expiry without process churn.
#[test] fn service_matches_library_reuses_and_drains() {
 let mut child=Command::new(env!("CARGO_BIN_EXE_webagent-background-service")).env_clear().stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
 let pid=child.id();let mut writer=child.stdin.take().unwrap();let mut reader=BufReader::new(child.stdout.take().unwrap());
 for (now,rescan_interval_ms) in [(10,250),(11,15000),(12,u64::MAX)] {
  let input=Request{version:2,now,slots:1,rescan_interval_ms,tasks:vec![Task{state:State::Running,active:false,lease_until:11},Task{state:State::Completed,active:false,lease_until:0}]};
  writeln!(writer,"{}",json!({"version":1,"id":format!("{now}"),"run_key":"public-test","input":input})).unwrap();writer.flush().unwrap();
  let mut line=String::new();reader.read_line(&mut line).unwrap();let reply:Value=serde_json::from_str(&line).unwrap();
  assert_eq!(reply["result"]["implementation_version"],webagent_background_dispatch::IMPLEMENTATION_VERSION);
  assert_eq!(reply["result"]["plan"],serde_json::to_value(webagent_background_dispatch::plan(&input).unwrap()).unwrap());assert_eq!(child.id(),pid);
 }
 drop(writer);assert!(child.wait().unwrap().success());
}
/// Reject unsupported protocol versions before producing any selection.
#[test] fn invalid_contract_returns_no_plan() {
 let mut child=Command::new(env!("CARGO_BIN_EXE_webagent-background-service")).env_clear().stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
 writeln!(child.stdin.take().unwrap(),"{}",json!({"version":2,"id":"bad","run_key":"public-test","input":{}})).unwrap();
 let output=child.wait_with_output().unwrap();assert!(!output.status.success());assert!(output.stdout.is_empty());
}

/// A valid transport envelope must not disguise an incompatible version-one planner request.
#[test] fn legacy_planner_payload_is_rejected() {
 let mut child=Command::new(env!("CARGO_BIN_EXE_webagent-background-service")).env_clear()
  .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
 writeln!(child.stdin.take().unwrap(),"{}",json!({"version":1,"id":"old","run_key":"public-test",
  "input":{"version":1,"now":0,"slots":0,"rescan_interval_ms":5000,"tasks":[]}})).unwrap();
 let output=child.wait_with_output().unwrap();assert!(!output.status.success());assert!(output.stdout.is_empty());
}
