//! Host-scoped execution evidence. Tools cannot supply metadata through result content.
use anyhow::{bail,Result};
use serde_json::{json,Value};
use webagent_context_contracts::execution::{Metadata,RpcCall};
use std::{cell::RefCell,future::Future};
tokio::task_local! {static EVIDENCE:RefCell<Metadata>;}
pub(crate) struct Outcome {pub content:Value,pub metadata:Metadata}
impl std::ops::Deref for Outcome {type Target=Value;fn deref(&self)->&Value{&self.content}}
// Existing result diagnostics format content only, never execution metadata.
impl std::fmt::Display for Outcome {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{self.content.fmt(f)}}
/// Each independently polled dispatch owns its evidence, including nested helper RPCs.
pub(crate) async fn capture(operation:impl Future<Output=Result<Value>>)->Result<Outcome> {
 EVIDENCE.scope(RefCell::new(Metadata::default()),async {
  let content=operation.await?;let metadata=EVIDENCE.with(|slot|slot.borrow().clone());metadata.validate()?;
  Ok(Outcome{content,metadata})
 }).await
}
pub(crate) fn rpc(peer:&str,id:&str)->Result<()> {
 EVIDENCE.try_with(|slot|{
  let mut evidence=slot.borrow_mut();if evidence.rpc_calls.len()>=256 {bail!("Execution RPC evidence limit");}
  evidence.rpc_calls.push(RpcCall{peer:peer.into(),id:id.into()});Ok(())
 }).unwrap_or(Ok(()))
}
/// Stage beside the encrypted run checkpoint, never in its model-facing input.
pub(crate) fn stage(run:&mut Value,metadata:&Metadata)->Result<()> {
 metadata.validate()?;if metadata.rpc_calls.is_empty(){return Ok(());}
 let id=run["pending"]["call_id"].as_str().filter(|id|!id.is_empty()).ok_or_else(||anyhow::anyhow!("Execution call identity missing"))?.to_owned();
 if !run["_tool_execution_metadata"].is_object(){run["_tool_execution_metadata"]=json!({});}
 run["_tool_execution_metadata"][id]=serde_json::to_value(metadata)?;Ok(())
}
#[cfg(test)]mod tests {
 use super::*;
 #[tokio::test]async fn parallel_scopes_and_nested_calls_are_isolated(){
  let mut set=tokio::task::JoinSet::new();
  for id in ["a","b"] {set.spawn(async move{capture(async{rpc("tool-worker",id)?;tokio::task::yield_now().await;rpc("build-host",id)?;Ok(json!({"id":"domain","status":"success"}))}).await.unwrap()});}
  while let Some(result)=set.join_next().await {let result=result.unwrap();assert_eq!(result.metadata.rpc_calls.len(),2);assert_eq!(result.metadata.rpc_calls[0].id,result.metadata.rpc_calls[1].id);assert_eq!(result.content["id"],"domain");}
  assert!(capture(async{Ok(json!({"status":"success"}))}).await.unwrap().metadata.rpc_calls.is_empty());
 }
}

#[cfg(test)]mod wire_tests {
 use super::*;
 use tokio::io::{AsyncBufReadExt,AsyncWriteExt,BufReader};
 #[tokio::test]async fn repeated_rpc_results_have_distinct_metadata_and_identical_model_content(){
  let root=tempfile::tempdir().unwrap();let socket=root.path().join("worker.sock");
  let listener=tokio::net::UnixListener::bind(&socket).unwrap();
  let server=tokio::spawn(async move{
   for _ in 0..2 {
    let (stream,_)=listener.accept().await.unwrap();let mut reader=BufReader::new(stream);let mut line=String::new();reader.read_line(&mut line).await.unwrap();
    let request:Value=serde_json::from_str(&line).unwrap();assert_eq!(request["version"],2);
    let reply=json!({"version":2,"id":request["id"],"result":{"status":"success","id":"domain-id","output":"content".repeat(1000)}});
    reader.get_mut().write_all(format!("{reply}\n").as_bytes()).await.unwrap();
   }
  });
  let sid=uuid::Uuid::new_v4().to_string();let args=json!({"path":"a.rs"});let mut outputs=Vec::new();
  for _ in 0..2 {outputs.push(capture(super::super::execute("file_read",&args,&sid,&socket,tokio_util::sync::CancellationToken::new())).await.unwrap());}
  server.await.unwrap();assert_eq!(outputs[0].content,outputs[1].content);
  assert_ne!(outputs[0].metadata.rpc_calls[0].id,outputs[1].metadata.rpc_calls[0].id);
  assert_eq!(outputs[0].content["id"],"domain-id");
  use webagent_context_contracts::tool_results::*;
  let request=Request{version:VERSION,observations:outputs.iter().enumerate().map(|(i,out)|Observation{
   call_id:format!("call-{i}"),tool:"file_read".into(),inputs:args.clone(),scope:json!({"session":sid}),output:out.content.to_string(),success:true,included:true
  }).collect(),state:State::default(),enabled:true,template:Some("Call {current_call_id} matches {reference_call_id} ({tool})".into()),rebase:false};
  let plan=webagent_context_assembly::tool_results::project(&request).unwrap();assert_eq!(plan.replacements.len(),1);assert!(plan.replacements.contains_key("call-1"));
  let mut run=json!({"pending":{"call_id":"call-1"},"input":[]});stage(&mut run,&outputs[1].metadata).unwrap();assert_eq!(run["input"],json!([]));
 }
}
