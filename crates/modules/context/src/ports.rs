//! Borrow current owner resources; no pools, caches or authorization are created here.
use anyhow::Result;
use serde_json::Value;
use std::{future::Future,path::Path};
use sqlx::{Sqlite,Transaction};
/// Borrow the existing agent pool; this interface creates no pool and performs no metadata mutation.
pub trait AgentMetadata:webagent_access::storage::RecordCodec+Sync {
 type Agent:AgentMetadata;
 fn agent_pool(&self)->&sqlx::SqlitePool;
 fn root(&self)->&std::path::Path;
 fn for_agent<'a>(&'a self,agent:&'a str)->impl Future<Output=Result<Self::Agent>>+Send+'a;
}

pub trait Records:Sync {
 fn current_user_id(&self)->&str;
 fn unseal(&self,raw:&[u8])->Result<Value>;
 fn write_document<'a,'c>(&'a self,tx:&'a mut Transaction<'c,Sqlite>,key:&'a str,value:&'a Value)->impl Future<Output=Result<()>>+Send+'a where 'c:'a;
}
pub trait Planner:Sync {
 fn assets_plan<'a>(&'a self,root:&'a Path,request:&'a webagent_assets_contracts::Request,cancel:&'a tokio_util::sync::CancellationToken)->impl Future<Output=Result<webagent_assets_contracts::Plan>>+Send+'a;
 fn tool_results_plan<'a>(&'a self,root:&'a Path,run:&'a Value,request:&'a webagent_context_contracts::tool_results::Request,cancel:&'a tokio_util::sync::CancellationToken)->impl Future<Output=Result<webagent_context_contracts::tool_results::Plan>>+Send+'a;
}
pub trait Journal:Sync {
 fn run<'a>(&'a self,id:&'a str)->impl Future<Output=Result<Value>>+Send+'a;
 fn document<'a>(&'a self,key:&'a str)->impl Future<Output=Result<Option<Value>>>+Send+'a;
 fn tool_result<'a>(&'a self,sid:&'a str,rid:&'a str,id:&'a str)->impl Future<Output=Result<Option<Value>>>+Send+'a;
 fn tool_result_origin<'a>(&'a self,sid:&'a str,id:&'a str)->impl Future<Output=Result<Option<(String,Value)>>>+Send+'a;
 fn tool_call_record<'a>(&'a self,sid:&'a str,rid:&'a str,id:&'a str)->impl Future<Output=Result<Option<Value>>>+Send+'a;
}

pub trait DedupSettings:Sync {
 fn dedup_settings<'a>(&'a self,sid:&'a str)->impl Future<Output=Result<Value>>+Send+'a;
}
