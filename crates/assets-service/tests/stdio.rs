//! Real private-pipe wrapper equivalence on the supported confined-service platform.
#![cfg(all(target_os="linux",target_arch="x86_64"))]
use serde_json::{json,Value};
use std::{io::Write,process::{Command,Stdio},time::{Duration,Instant}};
use webagent_assets_contracts::{Plan,Request,State};

/// Run one deterministic frame with a hard deadline; never leave a hung child behind.
fn call(input:Value)->std::process::Output {
 let mut child=Command::new(env!("CARGO_BIN_EXE_webagent-assets-service"))
  .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
 let mut bytes=serde_json::to_vec(&input).unwrap();bytes.push(b'\n');
 child.stdin.take().unwrap().write_all(&bytes).unwrap();
 let deadline=Instant::now()+Duration::from_secs(5);
 loop {
  if child.try_wait().unwrap().is_some(){return child.wait_with_output().unwrap();}
  if Instant::now()>=deadline {let _=child.kill();let _=child.wait();panic!("asset service timeout");}
  std::thread::sleep(Duration::from_millis(10));
 }
}
/// Compare the actual confined executable response with the linked implementation.
#[test]fn linked_and_real_service_are_equivalent() {
 let request=Request::Place{version:1,temporary:vec![],state:State::default(),history:vec![json!({"role":"user","content":"public fixture"})],
  definitions:vec!["main".into()],schema_names:vec!["file_read".into()],history_basis:"epoch".into(),append:true};
 let output=call(json!({"version":1,"id":"fixture","run_key":"public-fixture","input":request}));
 assert!(output.status.success(),"service failed");
 let reply:Value=serde_json::from_slice(&output.stdout).unwrap();
 assert_eq!(reply["id"],"fixture");assert_eq!(reply["result"]["status"],"success");
 let result:Plan=serde_json::from_value(reply["result"]["plan"].clone()).unwrap();
 assert_eq!(result,webagent_context_assets::plan(&request).unwrap());
 webagent_assets_contracts::validate_plan(&request,&result).unwrap();
}
/// Unsupported invocation cannot accidentally expose a compute or filesystem interface.
#[test]fn unsupported_invocation_is_rejected() {
 let output=call(json!({"version":1,"id":"fixture","run_key":"public-fixture","operation":"open","input":{}}));
 assert!(!output.status.success());assert!(output.stdout.is_empty());
}
