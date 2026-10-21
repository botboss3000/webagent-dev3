//! Borrowed owner adapters for Context; no authority is cached or copied.
use anyhow::Result;
use serde_json::Value;
use crate::ports::{Records,ToolJournal};
use crate::runtime::ports::ScopedStorage;
pub(crate) struct RecordAdapter<'a,R>(pub &'a R);
impl<R:Records> webagent_context::ports::Records for RecordAdapter<'_,R> {
 fn current_user_id(&self)->&str {self.0.current_user_id()}
 fn unseal(&self,raw:&[u8])->Result<Value>{self.0.unseal(raw)}
 fn write_document<'a,'c>(&'a self,tx:&'a mut sqlx::Transaction<'c,sqlx::Sqlite>,key:&'a str,value:&'a Value)->impl std::future::Future<Output=Result<()>>+Send+'a where 'c:'a {self.0.write_document(tx,key,value)}
}
pub(crate) struct Planner;
impl webagent_context::ports::Planner for Planner {
 fn assets_plan<'a>(&'a self,root:&'a std::path::Path,request:&'a webagent_assets_contracts::Request,cancel:&'a tokio_util::sync::CancellationToken)->impl std::future::Future<Output=Result<webagent_assets_contracts::Plan>>+Send+'a {crate::components::assets_plan(root,request,cancel)}
 fn tool_results_plan<'a>(&'a self,root:&'a std::path::Path,run:&'a Value,request:&'a webagent_context_contracts::tool_results::Request,cancel:&'a tokio_util::sync::CancellationToken)->impl std::future::Future<Output=Result<webagent_context_contracts::tool_results::Plan>>+Send+'a {crate::components::tool_results_plan(root,run,request,cancel)}
}
pub(crate) struct Journal<'a,S>{pub store:&'a S}
impl<S:ScopedStorage+ToolJournal> webagent_context::ports::Journal for Journal<'_,S> {
 fn run<'a>(&'a self,id:&'a str)->impl std::future::Future<Output=Result<Value>>+Send+'a {async move {self.store.get(&format!("run:{id}")).await?.ok_or_else(||anyhow::anyhow!("Run not found"))}}
 fn document<'a>(&'a self,key:&'a str)->impl std::future::Future<Output=Result<Option<Value>>>+Send+'a {self.store.get(key)}
 fn tool_result<'a>(&'a self,sid:&'a str,rid:&'a str,id:&'a str)->impl std::future::Future<Output=Result<Option<Value>>>+Send+'a {self.store.tool_result(sid,rid,id)}
 fn tool_result_origin<'a>(&'a self,sid:&'a str,id:&'a str)->impl std::future::Future<Output=Result<Option<(String,Value)>>>+Send+'a {self.store.tool_result_origin(sid,id)}
 fn tool_call_record<'a>(&'a self,sid:&'a str,rid:&'a str,id:&'a str)->impl std::future::Future<Output=Result<Option<Value>>>+Send+'a {self.store.tool_call_record(sid,rid,id)}
}
