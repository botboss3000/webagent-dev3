//! Existing foreign native handlers. Engine owns all dispatch authorization and execution fences.
use super::{Handles,config::SchemaCatalog,ports::SessionBinding};
use crate::model_client::Provider;
use anyhow::Result;
use serde_json::Value;
use std::{path::Path,sync::Arc,future::Future};
use tokio::sync::{Mutex,RwLock};
use tokio_util::sync::CancellationToken;
pub struct View<'a,S,C:SchemaCatalog,B,H:SessionBinding> {
 pub store:&'a S,pub catalog:&'a Arc<B>,pub actor:Option<&'a str>,pub binding:Option<&'a H>,
 pub handles:&'a Handles<C>,pub provider:&'a Provider,
 pub refresh:&'a Arc<Mutex<()>>,pub login:&'a Arc<RwLock<Option<Value>>>,
 pub tool_socket:&'a Path,pub build_socket:&'a Path,
}
pub struct Call {
 pub sid:String,pub run_id:String,pub generation:String,pub name:String,pub args:Value,
 pub pending_id:String,pub policy_version:u64,pub cancel:CancellationToken,
}
pub trait NativeDispatch<S:Sync,C:SchemaCatalog,B:Sync,H:SessionBinding>:Clone+Send+Sync+'static {
 fn call<'a>(&'a self,view:View<'a,S,C,B,H>,request:Call)->impl Future<Output=Result<Value>>+Send+'a;
 #[cfg(any(test,feature="test-support"))]
 fn test_purchase<'a>(&'a self,view:View<'a,S,C,B,H>,run:&'a Value,action:&'a Value,args:&'a Value)->impl Future<Output=Result<Value>>+Send+'a;

 fn decision<'a>(&'a self,view:View<'a,S,C,B,H>,sid:&'a str,run:&'a str,args:&'a Value,cancel:CancellationToken)->impl Future<Output=Result<Value>>+Send+'a;
 fn own_skill_guidance<'a>(&'a self,view:View<'a,S,C,B,H>,sid:&'a str,run:&'a str,read_only:bool)->impl Future<Output=Result<Vec<webagent_capability_contracts::Guidance>>>+Send+'a;

 fn optimization_prepare<'a>(&'a self,view:View<'a,S,C,B,H>,sid:&'a str,run:&'a mut Value)->impl Future<Output=Result<()>>+Send+'a;
 fn optimization_guidance(run:&Value)->&str;
 fn chat_model_preference<'a>(&'a self,view:View<'a,S,C,B,H>,run:&'a mut Value)->impl Future<Output=Result<bool>>+Send+'a;
 fn messaging_preflight<'a>(&'a self,view:View<'a,S,C,B,H>,run:&'a mut Value,name:&'a str,args:&'a Value)->impl Future<Output=Result<()>>+Send+'a;

 fn host_fingerprint<'a>(&'a self,view:View<'a,S,C,B,H>,sid:&'a str,run:&'a str,authority:&'a str)->impl Future<Output=Result<String>>+Send+'a;
 fn recovery_build_socket(&self,view:View<'_,S,C,B,H>)->std::path::PathBuf;

 fn naming_service<'a>(&'a self,view:View<'a,S,C,B,H>)->impl Future<Output=()>+Send+'a;
 fn manager_service<'a>(&'a self,view:View<'a,S,C,B,H>)->impl Future<Output=()>+Send+'a;
 fn modules_service<'a>(&'a self,view:View<'a,S,C,B,H>)->impl Future<Output=()>+Send+'a;
 fn followup_tick<'a>(&'a self,view:View<'a,S,C,B,H>)->impl Future<Output=Result<()>>+Send+'a;

 fn code_index_configuration_attach<'a>(&'a self,view:View<'a,S,C,B,H>,args:&'a Value,result:&'a mut Value,cancel:&'a CancellationToken)->impl Future<Output=Result<()>>+Send+'a;
 fn code_index_configuration_snippet_bytes(source:&Value)->usize;

 fn messaging_revalidate<'a>(&'a self,view:View<'a,S,C,B,H>,sid:&'a str)->impl Future<Output=Result<()>>+Send+'a;

}
