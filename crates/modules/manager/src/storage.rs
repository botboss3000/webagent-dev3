//! Borrow the existing canonical owner resources; no pools, keys or caches are created here.
use anyhow::Result;
use serde_json::Value;
use sqlx::{Sqlite,SqlitePool,Transaction};
use std::{future::Future,path::Path};
pub trait RecordEffects:Sync {
    fn unseal(&self,bytes:&[u8])->Result<Value>;
    fn unseal_agent(&self,_agent:&str,bytes:&[u8])->Result<Value> {self.unseal(bytes)}
    fn encode_user_record(&self,value:&Value)->Result<Vec<u8>>;
    fn resolved_prompts(&self,root:&Path)->Result<Value>;
    fn prompt_defaults(&self)->Result<Vec<u8>>;
    fn public_usage(&self,value:&Value)->Value;
    fn record_usage(&self,record:&mut Value,response:&Value);
    fn invalidate_documents(&self,keys:&[String]);
    fn agent_pool<'a>(&'a self,id:&'a str)->impl Future<Output=Result<SqlitePool>>+Send+'a;
    fn list_documents<'a>(&'a self,prefix:&'a str)->impl Future<Output=Result<Vec<Value>>>+Send+'a;
    fn read_document<'a>(&'a self,key:&'a str)->impl Future<Output=Result<Option<Value>>>+Send+'a;
    fn requester_claim_ready<'a>(&'a self,run:&'a Value)->impl Future<Output=Result<bool>>+Send+'a;
    fn participation_allows_run<'a>(&'a self,run:&'a Value)->impl Future<Output=Result<bool>>+Send+'a;
    fn reconcile_requester_slots(&self)->impl Future<Output=Result<()>>+Send+'_;
    fn require_active_session_in<'a,'c>(&'a self,tx:&'a mut Transaction<'c,Sqlite>,sid:&'a str)->impl Future<Output=Result<()>>+Send+'a where 'c:'a;
    fn write_document_in<'a,'c>(&'a self,tx:&'a mut Transaction<'c,Sqlite>,key:&'a str,value:&'a Value)->impl Future<Output=Result<()>>+Send+'a where 'c:'a;
    fn admit_message_in<'a,'c>(&'a self,tx:&'a mut Transaction<'c,Sqlite>,id:&'a str,sid:&'a str,run:&'a Value,message:&'a Value,command_id:&'a str)->impl Future<Output=Result<()>>+Send+'a where 'c:'a;
}
pub struct Repository<'a,E:RecordEffects> {pub(crate) root:&'a Path,pub(crate) pool:&'a SqlitePool,pub(crate) owner:&'a str,pub(crate) effects:&'a E}
impl<'a,E:RecordEffects> Repository<'a,E> {
 pub fn new(root:&'a Path,pool:&'a SqlitePool,owner:&'a str,effects:&'a E)->Self {Self{root,pool,owner,effects}}
 pub fn public(&self,record:&Value)->Value {crate::public(record,|usage|self.effects.public_usage(usage))}
 pub async fn decorate(&self,messages:&mut[Value])->Result<()> {
  for message in messages.iter_mut().filter(|m|m["role"]=="user") {if let Some(rid)=message["run_id"].as_str(){if let Some(record)=self.effects.read_document(&format!("manager_review:{rid}")).await?{message["admission"]=self.public(&record);}}} Ok(())
 }
}
