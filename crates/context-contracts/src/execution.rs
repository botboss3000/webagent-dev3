//! Versioned execution evidence; transport correlation is separate from tool content.
use anyhow::{bail,Result};
use serde::{Deserialize,Serialize};
use serde_json::Value;
pub const WORKER_PROTOCOL:u32=2;
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcCall {pub peer:String,pub id:String}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {pub version:u32,pub rpc_calls:Vec<RpcCall>}
impl Default for Metadata {fn default()->Self{Self{version:1,rpc_calls:Vec::new()}}}
impl Metadata {
 pub fn validate(&self)->Result<()> {
  if self.version!=1||self.rpc_calls.len()>256||self.rpc_calls.iter().any(|call|
   !matches!(call.peer.as_str(),"tool-worker"|"build-host")||call.id.is_empty()||call.id.len()>128||call.id.chars().any(char::is_control)) {bail!("Invalid execution metadata");}Ok(())
 }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerReply {version:u32,id:String,result:Value}
/// Decode the fixed worker envelope without removing domain IDs in its payload.
pub fn worker_reply(value:Value,expected:&str)->Result<Value> {
 let reply:WorkerReply=serde_json::from_value(value)?;
 if reply.version!=WORKER_PROTOCOL||reply.id!=expected {bail!("Worker RPC protocol or correlation mismatch");}Ok(reply.result)
}
/// The protected host retains its existing flat envelope; only its routing field is metadata.
pub fn host_reply(mut value:Value,expected:&str)->Result<Value> {
 if value["id"].as_str()!=Some(expected)||!value.is_object(){bail!("Host RPC correlation mismatch");}
 value.as_object_mut().unwrap().remove("id");Ok(value)
}
#[cfg(test)]mod tests {
 use super::*;use serde_json::json;
 #[test]fn preserves_payload_ids_and_rejects_mixed_protocols(){
  let body=json!({"id":"domain","output":{"id":"nested","timestamp":7},"status":"success"});
  assert_eq!(worker_reply(json!({"version":2,"id":"routing","result":body}),"routing").unwrap(),body);
  assert!(worker_reply(json!({"id":"routing","status":"success"}),"routing").is_err());
  assert!(worker_reply(json!({"version":2,"id":"wrong","result":body}),"routing").is_err());
  assert_eq!(host_reply(json!({"id":"routing","output":{"id":"job"}}),"routing").unwrap(),json!({"output":{"id":"job"}}));
 }
}
