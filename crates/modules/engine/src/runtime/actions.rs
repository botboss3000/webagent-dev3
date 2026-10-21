//! Static foreign-owner Action operations. Durable intent, receipts and reconciliation remain root Actions.
use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use crate::tools::execution::ActionCatalog;
pub trait Actions<S:Sync>:ActionCatalog {
 fn strip(args:&Value)->Value;
 fn response(action:&Value)->Value;
 fn summary(action:&Value)->Value;
 fn ledger<'a>(&'a self,store:&'a S,run:&'a Value)->impl Future<Output=Result<S>>+Send+'a;
 fn list_for_agent<'a>(&'a self,ledger:&'a S,agent:&'a str)->impl Future<Output=Result<Vec<Value>>>+Send+'a;
 fn prepare<'a>(&'a self,store:&'a S,run:&'a Value,name:&'a str,args:&'a Value,pending_id:&'a str,visibility:&'a Value)->impl Future<Output=Result<Option<Value>>>+Send+'a;
 fn preflight<'a>(&'a self,store:&'a S,run:&'a mut Value,name:&'a str,args:&'a Value,visibility:&'a Value)->impl Future<Output=Result<Option<Value>>>+Send+'a;
 fn claim<'a>(&'a self,store:&'a S,run:&'a Value,pending:&'a Value)->impl Future<Output=Result<Option<Value>>>+Send+'a;
 fn finish<'a>(&'a self,store:&'a S,run:&'a Value,action:&'a Value,result:&'a Result<Value>)->impl Future<Output=Result<Value>>+Send+'a;
 fn reconcile<'a>(&'a self,store:&'a S,run:&'a Value,action:&'a Value)->impl Future<Output=Result<Option<Value>>>+Send+'a;
 fn not_dispatched<'a>(&'a self,store:&'a S,run:&'a Value,pending:&'a Value)->impl Future<Output=Result<()>>+Send+'a;
 fn recover_results<'a>(&'a self,store:&'a S,run:&'a mut Value)->impl Future<Output=Result<bool>>+Send+'a;
 fn interrupt_run<'a>(&'a self,store:&'a S,run:&'a Value)->impl Future<Output=Result<()>>+Send+'a;
}
