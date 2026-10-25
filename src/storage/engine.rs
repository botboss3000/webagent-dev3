//! Root composition for existing Storage/Chat/Access/Notifications/Usage owners.
//! Engine owns all checkpoint and lease decisions; these adapters keep existing resources.
use super::*;
use webagent_engine::ports::*;
type EngineRepository<'a>=webagent_engine::repository::Repository<'a,Store,Store,Store,Store,Store>;
impl Store {
 pub(crate) fn engine_repository(&self)->EngineRepository<'_>{
  EngineRepository::new(&self.root,&self.pool,&self.app_pool,&self.agent_id,self,self,self,self,self)
 }
}
impl Records for Store {
 fn current_user_id(&self)->&str {Store::current_user_id(self)}
 fn encode_user_record(&self,value:&Value)->Result<Vec<u8>> {Store::encode_user_record(self,value)}
 fn context_gate(&self)->&tokio::sync::RwLock<()> {&self.context_gate}
 fn invalidate(&self,keys:&[String]) {self.cache.lock().unwrap().invalidate(keys);}
 fn invalidate_context(&self,sid:&str) {self.context_cache.lock().unwrap().invalidate(&self.context_cache_key(sid));}
 fn reasoning_hashes(&self,source:&[Value])->Result<Vec<String>> {reasoning_cleanup::hashes(source)}
 fn decode_document<'a>(&'a self,connection:&'a mut sqlx::SqliteConnection,raw:&'a [u8])->impl std::future::Future<Output=Result<Value>>+Send+'a {Store::decode_document(self,connection,raw)}
 fn write_document<'a,'c>(&'a self,tx:&'a mut sqlx::Transaction<'c,sqlx::Sqlite>,key:&'a str,value:&'a Value)->impl std::future::Future<Output=Result<()>>+Send+'a where 'c:'a {Store::write_document(self,tx,key,value)}
}
impl AgentMetadata for Store {
 type Agent=Store;
 fn agent_pool(&self)->&SqlitePool {&self.agent_pool}
 fn root(&self)->&std::path::Path {&self.root}
 fn for_agent<'a>(&'a self,agent:&'a str)->impl std::future::Future<Output=Result<Self::Agent>>+Send+'a {Store::for_agent(self,agent)}
}
impl ChatEffects for Store {
 fn require_active_session<'a,'c>(&'a self,tx:&'a mut sqlx::Transaction<'c,sqlx::Sqlite>,sid:&'a str)->impl std::future::Future<Output=Result<()>>+Send+'a where 'c:'a {Store::require_active_session(self,tx,sid)}
 fn require_participation<'a,'c>(&'a self,tx:&'a mut sqlx::Transaction<'c,sqlx::Sqlite>,run:&'a Value)->impl std::future::Future<Output=Result<()>>+Send+'a where 'c:'a {Store::require_participation(self,tx,run)}
 fn insert_interaction<'a,'c>(&'a self,tx:&'a mut sqlx::Transaction<'c,sqlx::Sqlite>,sid:&'a str,run_id:&'a str,step:&'a str,item:&'a Value)->impl std::future::Future<Output=Result<()>>+Send+'a where 'c:'a {Store::insert_interaction(self,tx,sid,run_id,step,item)}
 fn attach_execution_metadata<'a,'c>(&'a self,tx:&'a mut sqlx::Transaction<'c,sqlx::Sqlite>,sid:&'a str,rid:&'a str,call_id:&'a str,value:&'a Value)->impl std::future::Future<Output=Result<()>>+Send+'a where 'c:'a {Store::attach_execution_metadata(self,tx,sid,rid,call_id,value)}
 fn insert_working_message<'a,'c>(&'a self,tx:&'a mut sqlx::Transaction<'c,sqlx::Sqlite>,sid:&'a str,rid:&'a str,message_id:&'a str,text:&'a str)->impl std::future::Future<Output=Result<()>>+Send+'a where 'c:'a {Store::insert_working_message(self,tx,sid,rid,message_id,text)}
 fn insert_chat<'a,'c>(&'a self,tx:&'a mut sqlx::Transaction<'c,sqlx::Sqlite>,sid:&'a str,run_id:&'a str,role:&'a str,content:&'a Value)->impl std::future::Future<Output=Result<()>>+Send+'a where 'c:'a {Store::insert_chat(self,tx,sid,run_id,role,content)}
 fn queue_session_naming<'a,'c>(&'a self,tx:&'a mut sqlx::Transaction<'c,sqlx::Sqlite>,sid:&'a str,run:&'a Value)->impl std::future::Future<Output=Result<()>>+Send+'a where 'c:'a {Store::queue_session_naming(self,tx,sid,run)}
 fn followup_publication_guard<'a,'c>(&'a self,tx:&'a mut sqlx::Transaction<'c,sqlx::Sqlite>,sid:&'a str)->impl std::future::Future<Output=Result<()>>+Send+'a where 'c:'a {Store::followup_publication_guard(self,tx,sid)}
}

impl AdmissionEffects for Store {
 fn requester_claim_ready<'a>(&'a self,run:&'a Value)->impl std::future::Future<Output=Result<bool>>+Send+'a {Store::requester_claim_ready(self,run)}
 fn reconcile_requester_slots<'a>(&'a self)->impl std::future::Future<Output=Result<()>>+Send+'a {Store::reconcile_requester_slots(self)}
 fn reserve_requester_run<'a>(&'a self,run:&'a Value)->impl std::future::Future<Output=Result<()>>+Send+'a {Store::reserve_requester_run(self,run)}
}
impl NotificationEffects for Store {
 fn stage_run_notification<'a,'c>(&'a self,tx:&'a mut sqlx::Transaction<'c,sqlx::Sqlite>,run:&'a Value)->impl std::future::Future<Output=Result<()>>+Send+'a where 'c:'a {Store::stage_run_notification(self,tx,run)}
 fn stage_notification<'a,'c>(&'a self,tx:&'a mut sqlx::Transaction<'c,sqlx::Sqlite>,event_key:&'a str,kind:&'a str,sid:&'a str,rid:Option<&'a str>,status:Option<&'a str>)->impl std::future::Future<Output=Result<()>>+Send+'a where 'c:'a {Store::stage_notification(self,tx,event_key,kind,sid,rid,status)}
}
impl UsageEffects for Store {
 fn stage_run_usage_in<'a,'c>(&'a self,tx:&'a mut sqlx::Transaction<'c,sqlx::Sqlite>,run:&'a Value)->impl std::future::Future<Output=Result<Value>>+Send+'a where 'c:'a {webagent_usage::stage_session_total(self,tx,run)}
}
impl webagent_usage::SessionRecords for Store {
 async fn previous_run_usage(&self,tx:&mut sqlx::Transaction<'_,sqlx::Sqlite>,run:&Value)->Result<Value> {
        let view_key=format!("run_view:{}",run["id"].as_str().unwrap());
        let previous:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(&view_key).fetch_optional(&mut **tx).await?;
        Ok(previous.map(|raw|self.unseal(&raw)).transpose()?.map_or(Value::Null,|view|view["usage"].clone()))
 }
 async fn read_total(&self,tx:&mut sqlx::Transaction<'_,sqlx::Sqlite>,sid:&str)->Result<Option<Value>> {
        let raw:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(format!("session_usage:{sid}")).fetch_optional(&mut **tx).await?;
        raw.map(|raw|self.unseal(&raw)).transpose()
 }
 async fn write_total(&self,tx:&mut sqlx::Transaction<'_,sqlx::Sqlite>,sid:&str,total:&Value)->Result<()> {
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").bind(format!("session_usage:{sid}")).bind(self.encode_user_record(total)?).execute(&mut **tx).await?;
        Ok(())
 }
 fn invalidate_total(&self,sid:&str){self.cache.lock().unwrap().invalidate(&[format!("session_usage:{sid}")]);}
}

impl webagent_engine::runtime::ports::ScopedStorage for Store {
 fn user_pool(&self)->&SqlitePool{&self.pool}
 fn application_pool(&self)->&SqlitePool{&self.app_pool}
 fn agent_id(&self)->&str{&self.agent_id}
 fn configure_cache(&self,documents:usize,contexts:usize){Store::configure_cache(self,documents,contexts)}
 fn for_user<'a>(&'a self,user:&'a str)->impl std::future::Future<Output=Result<Self>>+Send+'a{Store::for_user(self,user)}
 fn all_user_ids(&self)->impl std::future::Future<Output=Result<Vec<String>>>+Send{Store::all_user_ids(self)}
 fn get<'a>(&'a self,key:&'a str)->impl std::future::Future<Output=Result<Option<Value>>>+Send+'a{Store::get(self,key)}
 fn put<'a>(&'a self,key:&'a str,value:&'a Value)->impl std::future::Future<Output=Result<()>>+Send+'a{Store::put(self,key,value)}
}
impl webagent_engine::runtime::ports::RunPresentation for Store {fn present_run(&self,run:&Value)->Value{crate::public_run(run)}}

impl webagent_engine::runtime::models::ModelRecords for Store {
 fn application_pool(&self)->&sqlx::SqlitePool {&self.app_pool}
 fn agent_id(&self)->&str {self.current_agent_id()}
}

impl ToolJournal for Store {
 fn tool_result<'a>(&'a self,sid:&'a str,rid:&'a str,call_id:&'a str)->impl std::future::Future<Output=Result<Option<Value>>>+Send+'a {Store::tool_result(self,sid,rid,call_id)}
 fn tool_result_origin<'a>(&'a self,sid:&'a str,call_id:&'a str)->impl std::future::Future<Output=Result<Option<(String,Value)>>>+Send+'a {Store::tool_result_origin(self,sid,call_id)}
 fn tool_call_record<'a>(&'a self,sid:&'a str,rid:&'a str,call_id:&'a str)->impl std::future::Future<Output=Result<Option<Value>>>+Send+'a {Store::tool_call_record(self,sid,rid,call_id)}
}

impl webagent_engine::runtime::ports::SessionRecords for Store {
 fn context<'a>(&'a self,sid:&'a str)->impl std::future::Future<Output=Result<Vec<Value>>>+Send+'a {Store::context(self,sid)}
 fn shared_model_context<'a>(&'a self,sid:&'a str)->impl std::future::Future<Output=Result<Vec<Value>>>+Send+'a {Store::shared_model_context(self,sid)}
 fn drain_message_queues(&self)->impl std::future::Future<Output=Result<()>>+Send {Store::drain_message_queues(self)}
 fn cleanup_one_reasoning(&self)->impl std::future::Future<Output=Result<bool>>+Send {Store::cleanup_one_reasoning(self)}
}
