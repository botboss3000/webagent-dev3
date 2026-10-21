//! Static owner adapters. Implementations retain their existing pools, locks and caches.
//! These methods never own or decide Engine revisions, leases, scheduling or state transitions.
use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use sqlx::{Sqlite,SqliteConnection,Transaction};
pub trait Records:webagent_access::storage::RecordCodec+Sync {
 fn current_user_id(&self)->&str;
 fn encode_user_record(&self,value:&Value)->Result<Vec<u8>>;
 fn context_gate(&self)->&tokio::sync::RwLock<()>;
 fn invalidate(&self,keys:&[String]);
 fn invalidate_context(&self,sid:&str);
 fn reasoning_hashes(&self,source:&[Value])->Result<Vec<String>>;
 fn decode_document<'a>(&'a self,connection:&'a mut SqliteConnection,raw:&'a [u8])->impl Future<Output=Result<Value>>+Send+'a;
 fn write_document<'a,'c>(&'a self,tx:&'a mut Transaction<'c,Sqlite>,key:&'a str,value:&'a Value)->impl Future<Output=Result<()>>+Send+'a where 'c:'a;
}
pub trait ChatEffects:Sync {
 fn require_active_session<'a,'c>(&'a self,tx:&'a mut Transaction<'c,Sqlite>,sid:&'a str)->impl Future<Output=Result<()>>+Send+'a where 'c:'a;
 fn require_participation<'a,'c>(&'a self,tx:&'a mut Transaction<'c,Sqlite>,run:&'a Value)->impl Future<Output=Result<()>>+Send+'a where 'c:'a;
 fn insert_interaction<'a,'c>(&'a self,tx:&'a mut Transaction<'c,Sqlite>,sid:&'a str,run_id:&'a str,step:&'a str,item:&'a Value)->impl Future<Output=Result<()>>+Send+'a where 'c:'a;
 fn attach_execution_metadata<'a,'c>(&'a self,tx:&'a mut Transaction<'c,Sqlite>,sid:&'a str,rid:&'a str,call_id:&'a str,value:&'a Value)->impl Future<Output=Result<()>>+Send+'a where 'c:'a;
 fn insert_working_message<'a,'c>(&'a self,tx:&'a mut Transaction<'c,Sqlite>,sid:&'a str,rid:&'a str,message_id:&'a str,text:&'a str)->impl Future<Output=Result<()>>+Send+'a where 'c:'a;
 fn insert_chat<'a,'c>(&'a self,tx:&'a mut Transaction<'c,Sqlite>,sid:&'a str,run_id:&'a str,role:&'a str,content:&'a Value)->impl Future<Output=Result<()>>+Send+'a where 'c:'a;
 fn queue_session_naming<'a,'c>(&'a self,tx:&'a mut Transaction<'c,Sqlite>,sid:&'a str,run:&'a Value)->impl Future<Output=Result<()>>+Send+'a where 'c:'a;
 fn followup_publication_guard<'a,'c>(&'a self,tx:&'a mut Transaction<'c,Sqlite>,sid:&'a str)->impl Future<Output=Result<()>>+Send+'a where 'c:'a;
}

pub trait AdmissionEffects:Sync {
 fn requester_claim_ready<'a>(&'a self,run:&'a Value)->impl Future<Output=Result<bool>>+Send+'a;
 fn reconcile_requester_slots<'a>(&'a self)->impl Future<Output=Result<()>>+Send+'a;
 fn reserve_requester_run<'a>(&'a self,run:&'a Value)->impl Future<Output=Result<()>>+Send+'a;
}
pub trait NotificationEffects:Sync {
 fn stage_run_notification<'a,'c>(&'a self,tx:&'a mut Transaction<'c,Sqlite>,run:&'a Value)->impl Future<Output=Result<()>>+Send+'a where 'c:'a;
 fn stage_notification<'a,'c>(&'a self,tx:&'a mut Transaction<'c,Sqlite>,event_key:&'a str,kind:&'a str,sid:&'a str,rid:Option<&'a str>,status:Option<&'a str>)->impl Future<Output=Result<()>>+Send+'a where 'c:'a;
}
pub trait UsageEffects:Sync {
 fn stage_run_usage_in<'a,'c>(&'a self,tx:&'a mut Transaction<'c,Sqlite>,run:&'a Value)->impl Future<Output=Result<Value>>+Send+'a where 'c:'a;
}

/// Borrow the existing agent pool; this interface creates no pool and performs no metadata mutation.
pub trait AgentMetadata:webagent_access::storage::RecordCodec+Sync {
 type Agent:AgentMetadata;
 fn agent_pool(&self)->&sqlx::SqlitePool;
 fn root(&self)->&std::path::Path;
 fn for_agent<'a>(&'a self,agent:&'a str)->impl Future<Output=Result<Self::Agent>>+Send+'a;
}

pub trait ToolJournal:Sync {
 fn tool_result<'a>(&'a self,sid:&'a str,rid:&'a str,call_id:&'a str)->impl Future<Output=Result<Option<Value>>>+Send+'a;
 fn tool_result_origin<'a>(&'a self,sid:&'a str,call_id:&'a str)->impl Future<Output=Result<Option<(String,Value)>>>+Send+'a;
 fn tool_call_record<'a>(&'a self,sid:&'a str,rid:&'a str,call_id:&'a str)->impl Future<Output=Result<Option<Value>>>+Send+'a;
}
