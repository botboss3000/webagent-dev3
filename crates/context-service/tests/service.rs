//! Real process boundary checks: pipe reuse, immutable inputs and syscall confinement.
use std::{io::{BufRead,BufReader,Write},process::{Command,Stdio}};
use serde_json::{json,Value};
use webagent_context_contracts::{protocol::PrepareRequest,context_input::{CallIdentity,ModelMetadata},context_compaction::Config};
/// Resolve an independent host input fixture without giving the process config access.
fn input(history:Vec<Value>)->PrepareRequest {
 let mut compaction:Config=serde_json::from_str(include_str!("../../../config/context-compaction.json")).unwrap();
 compaction.trigger_tokens=10000;compaction.tail_tokens=1000;compaction.summary_reserve_bytes=100;
 PrepareRequest{history,base:"host instructions".into(),guidance:" guidance".into(),schemas:vec![],contributions:vec![],
 identity:CallIdentity{turn:1,call:1},model:ModelMetadata{provider:"deepseek".into(),model:"test".into(),transport:"chat_completions".into()},
 max_bytes:8192,compaction,system_input_budget:None,summary_instructions:"summary instructions".into()}
}
#[test]
fn real_service_reuses_process_and_resets_same_length_history() {
 let mut child=Command::new(env!("CARGO_BIN_EXE_webagent-context-service")).env_clear().stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
 let pid=child.id();let mut writer=child.stdin.take().unwrap();let mut reader=BufReader::new(child.stdout.take().unwrap());
 for (index,text) in ["short","unicode é🌍 with changed length","append test"].iter().enumerate() {
  let request=input(vec![json!({"role":"user","content":text})]);
  let expected=webagent_context_assembly::protocol::prepare_owned(&request,&mut Default::default()).unwrap();
  writeln!(writer,"{}",json!({"version":1,"id":format!("call-{index}"),"run_key":"host-run-generation","input":request})).unwrap();writer.flush().unwrap();
  let mut line=String::new();reader.read_line(&mut line).unwrap();let reply:Value=serde_json::from_str(&line).unwrap();
  assert_eq!(reply["id"],format!("call-{index}"));assert_eq!(reply["result"]["prepared"],serde_json::to_value(expected).unwrap());
  assert_eq!(child.id(),pid);
 }
 for (index,expected) in [true,false].iter().enumerate() {
  writeln!(writer,"{}",json!({"version":1,"id":format!("release-{index}"),"run_key":"host-run-generation","operation":"release"})).unwrap();writer.flush().unwrap();
  let mut line=String::new();reader.read_line(&mut line).unwrap();let reply:Value=serde_json::from_str(&line).unwrap();
  assert_eq!(reply["id"],format!("release-{index}"));assert_eq!(reply["result"]["status"],"released");assert_eq!(reply["result"]["removed"],*expected);
 }
 drop(writer);assert!(child.wait().unwrap().success());
}
#[test]
fn real_service_denies_ambient_authority_and_alternate_syscalls() {
 let output=Command::new(env!("CARGO_BIN_EXE_webagent-context-service")).env_clear().arg("--isolation-probe").output().unwrap();
 assert!(output.status.success());let probes:Value=serde_json::from_slice(&output.stdout).unwrap();
 for name in ["open","file_write","socket","network","fork","exec","x32_open","handle"] {assert_eq!(probes[name],true,"probe {name}");}
}
#[test]
fn unknown_invocation_is_rejected_without_a_result() {
 let mut child=Command::new(env!("CARGO_BIN_EXE_webagent-context-service")).env_clear().stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
 writeln!(child.stdin.take().unwrap(),"{}",json!({"version":2,"id":"bad","run_key":"host","input":input(vec![])})).unwrap();
 let output=child.wait_with_output().unwrap();assert!(!output.status.success());assert!(output.stdout.is_empty());
}
